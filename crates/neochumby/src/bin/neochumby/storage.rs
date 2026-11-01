//! `access = ["storage"]`: a directory of the app's own that lasts across
//! boots, shared by its exec and its service.
//!
//! FAT has no owners, so a directory on the stick can't be one app's. So the
//! app's directory is in RAM, [`BASE`]/<name>/: the session makes it as it
//! starts either, owned by the app's uid (every file in it too: an app's uid
//! follows its place in the tree, so it can change between boots), mode 0700,
//! binds it into the jail at the same path, and fills it from the stick on the
//! first start of a boot. NEOCHUMBY_STORAGE names it, jailed or not. Every
//! [`KEEP_EVERY`], and as the session ends, what changed is copied back to
//! [`KEEP`]/<name>/ on the stick (write_durable), so a power cut can lose the
//! last half minute, and never a file half written. Names FAT can't hold
//! aren't kept, and say so.
//!
//! Each app may use [`CAP`]. The kernel has no quotas, and a bind mount can't
//! be limited, so the session measures: every [`CHECK`] it adds up each
//! running app's directory and ends an app over its limit (its exec, or its
//! service, as the network check does). An app's limit is [`CAP`], or what it
//! already used as the session started if that was more, so an app left over
//! the cap can still run, and tidy up, but not grow. Like the network check,
//! this finds an app over its limit; it doesn't stop the writes that put it
//! there.
//!
//! What the session measures: each file or link counts its size rounded up
//! to a [`BLOCK`], and at least one, so a pile of empty files counts too;
//! each directory counts exactly one block, whatever the filesystem says its
//! size is (4096 on ext4, little on tmpfs or vfat), so a quota is the same
//! on every filesystem. Links are never followed.

use std::io;
use std::os::unix::fs::{PermissionsExt, lchown};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use neochumby::apps::App;

/// Where the apps' directories are, in RAM.
pub const BASE: &str = "/run/neochumby-storage";
/// Where they are kept, on the stick.
pub const KEEP: &str = "/mnt/usb/neochumby/state/apps";
/// How often what changed is copied there.
const KEEP_EVERY: Duration = Duration::from_secs(30);
/// What each app may use, in bytes.
pub const CAP: u64 = 64 * 1024;
/// How often the session measures.
pub const CHECK: Duration = Duration::from_secs(2);
/// What each entry counts in multiples of.
const BLOCK: u64 = 512;

/// The bytes `dir` and everything in it count for (none if it's missing).
pub fn usage(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| {
            let Ok(m) = e.path().symlink_metadata() else { return 0 };
            if m.is_dir() { BLOCK + usage(&e.path()) } else { m.len().div_ceil(BLOCK).max(1) * BLOCK }
        })
        .sum()
}

/// Gives `dir` and everything in it to `uid`, without following links.
fn chown_all(dir: &Path, uid: u32) -> io::Result<()> {
    lchown(dir, Some(uid), Some(uid))?;
    for e in std::fs::read_dir(dir)?.flatten() {
        let p = e.path();
        if p.symlink_metadata()?.is_dir() {
            chown_all(&p, uid)?;
        } else {
            lchown(&p, Some(uid), Some(uid))?;
        }
    }
    Ok(())
}

/// Makes `to`, on the stick, hold what `from` holds: only what differs is
/// written, each file whole and synced; what's gone from `from` is removed.
fn mirror(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    let mut names = std::collections::HashSet::new();
    for e in std::fs::read_dir(from)?.flatten() {
        let (name, path) = (e.file_name(), e.path());
        names.insert(name.clone());
        let m = path.symlink_metadata()?;
        let there = to.join(&name);
        if m.is_dir() {
            mirror(&path, &there)?;
        } else if m.is_file() {
            let new = std::fs::read(&path)?;
            if std::fs::read(&there).ok().as_ref() != Some(&new) {
                let Some(n) = name.to_str() else { continue };
                neochumby::conf::write_durable(to, n, &new)?;
            }
        }
    }
    for e in std::fs::read_dir(to)?.flatten() {
        if !names.contains(&e.file_name()) {
            let p = e.path();
            if p.symlink_metadata()?.is_dir() { std::fs::remove_dir_all(p)? } else { std::fs::remove_file(p)? }
        }
    }
    Ok(())
}

/// Fills `to`, in RAM, from `from` on the stick, if it has anything.
fn restore(from: &Path, to: &Path) -> io::Result<()> {
    let Ok(rd) = std::fs::read_dir(from) else { return Ok(()) };
    for e in rd.flatten() {
        let (path, there) = (e.path(), to.join(e.file_name()));
        if path.symlink_metadata()?.is_dir() {
            std::fs::create_dir_all(&there)?;
            restore(&path, &there)?;
        } else {
            std::fs::copy(&path, &there)?;
            std::fs::set_permissions(&there, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

/// The apps' directories, and their limits.
pub struct Storage {
    base: PathBuf,
    /// Each app with `storage`, and the most it may use.
    limits: Vec<(String, u64)>,
    due: Instant,
    /// Where they are kept, and when next.
    keep: Option<PathBuf>,
    keep_due: Instant,
}

impl Storage {
    /// The directories under `base` of the apps in `apps` with `storage`,
    /// each limited to [`CAP`] or what it uses now, if more.
    pub fn new(base: PathBuf, apps: &[App]) -> Storage {
        let limits = apps
            .iter()
            .filter(|a| a.access.storage)
            .map(|a| {
                let used = usage(&base.join(&a.name));
                if used > CAP {
                    eprintln!("neochumby: {}'s storage holds {used} bytes, over {CAP}; it may not grow", a.name);
                }
                (a.name.clone(), used.max(CAP))
            })
            .collect();
        Storage { base, limits, due: Instant::now() + CHECK, keep: None, keep_due: Instant::now() + KEEP_EVERY }
    }

    /// Keeps the directories under `keep`: fills them from there as they are
    /// made, and copies them back.
    pub fn keep_in(&mut self, keep: Option<PathBuf>) {
        self.keep = keep;
    }

    /// Copies what changed back to the stick, if it's time.
    pub fn keep_if_due(&mut self) {
        if Instant::now() >= self.keep_due {
            self.keep_due = Instant::now() + KEEP_EVERY;
            self.keep_now();
        }
    }

    /// Copies what changed back to the stick. An app's directory that was
    /// never made this boot is left as the stick has it.
    pub fn keep_now(&self) {
        let Some(keep) = &self.keep else { return };
        for (name, _) in &self.limits {
            let (from, to) = (self.base.join(name), keep.join(name));
            if from.is_dir()
                && let Err(e) = mirror(&from, &to)
            {
                eprintln!("neochumby: keeping {name}'s storage: {e}");
            }
        }
    }

    /// `app`'s directory, made and owned by `owner` (its uid, when it runs
    /// jailed; else root keeps it), if it has `storage`. None, logged, if
    /// it couldn't be made.
    pub fn dir(&self, app: &App, owner: Option<u32>) -> Option<PathBuf> {
        if !app.access.storage {
            return None;
        }
        let dir = self.base.join(&app.name);
        let made = || -> io::Result<()> {
            std::fs::create_dir_all(&self.base)?;
            match dir.symlink_metadata() {
                Ok(m) if !m.is_dir() => return Err(io::Error::other("not a directory")),
                Ok(_) => {}
                Err(_) => {
                    std::fs::create_dir(&dir)?;
                    if let Some(keep) = &self.keep {
                        restore(&keep.join(&app.name), &dir)?;
                    }
                }
            }
            if let Some(uid) = owner {
                chown_all(&dir, uid)?;
            }
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        };
        match made() {
            Ok(()) => Some(dir),
            Err(e) => {
                eprintln!("neochumby: {}: {e}; {} runs without its storage", dir.display(), app.name);
                None
            }
        }
    }

    /// Whether it's time to measure again.
    pub fn due(&mut self) -> bool {
        if Instant::now() < self.due {
            return false;
        }
        self.due = Instant::now() + CHECK;
        true
    }

    /// What `app`'s directory holds, if that's over its limit.
    pub fn over(&self, app: &App) -> Option<u64> {
        let (_, limit) = self.limits.iter().find(|(n, _)| *n == app.name)?;
        let used = usage(&self.base.join(&app.name));
        (used > *limit).then_some(used)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::start::tests::app;

    fn with_storage(name: &str) -> App {
        let mut a = app(name, false);
        a.access.storage = true;
        a
    }

    /// What an app wrote comes back after a reboot (RAM gone, the stick not);
    /// what it removed stays gone; nothing unchanged is written again.
    #[test]
    fn kept_on_the_stick() {
        let root = std::env::temp_dir().join(format!("neochumby-storage-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (ram, stick) = (root.join("ram"), root.join("stick"));
        let apps = [with_storage("radio")];
        let mut s = Storage::new(ram.clone(), &apps);
        s.keep_in(Some(stick.clone()));
        let dir = s.dir(&apps[0], None).unwrap();
        std::fs::write(dir.join("station"), b"3").unwrap();
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d/x"), b"y").unwrap();
        s.keep_now();
        assert_eq!(std::fs::read(stick.join("radio/d/x")).unwrap(), b"y");
        let written = stick.join("radio/station").metadata().unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        s.keep_now();
        assert_eq!(stick.join("radio/station").metadata().unwrap().modified().unwrap(), written, "not again");
        std::fs::remove_file(dir.join("d/x")).unwrap();
        s.keep_now();
        assert!(!stick.join("radio/d/x").exists());

        std::fs::remove_dir_all(&ram).unwrap();
        let mut s = Storage::new(ram.clone(), &apps);
        s.keep_in(Some(stick.clone()));
        let dir = s.dir(&apps[0], None).unwrap();
        assert_eq!(std::fs::read(dir.join("station")).unwrap(), b"3");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn made_measured_and_limited() {
        let base = std::env::temp_dir().join(format!("neochumby-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // Left over the cap from before: it may stay so, but not grow.
        let big = base.join("big");
        std::fs::create_dir_all(&big).unwrap();
        std::fs::write(big.join("a"), vec![0; CAP as usize + 1000]).unwrap();
        let apps = [with_storage("radio"), with_storage("big"), app("doom", false)];
        let s = Storage::new(base.clone(), &apps);

        let dir = s.dir(&apps[0], None).unwrap();
        assert_eq!(dir, base.join("radio"));
        assert_eq!(dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(s.dir(&apps[2], None), None);
        assert_eq!(s.over(&apps[0]), None);

        // Sizes round up to a block, and an empty file counts for one.
        std::fs::write(dir.join("station"), b"3").unwrap();
        std::fs::create_dir(dir.join("d")).unwrap();
        std::fs::write(dir.join("d").join("empty"), b"").unwrap();
        assert_eq!(usage(&dir), 3 * BLOCK);
        // A link counts as itself, not what it points at.
        std::os::unix::fs::symlink(big.join("a"), dir.join("link")).unwrap();
        assert!(usage(&dir) < 5 * BLOCK);

        std::fs::write(dir.join("more"), vec![0; CAP as usize]).unwrap();
        assert!(s.over(&apps[0]).is_some());
        assert_eq!(s.over(&apps[1]), None);
        std::fs::write(big.join("b"), b"x").unwrap();
        assert!(s.over(&apps[1]).is_some());
        // Not an app with storage: never over.
        assert_eq!(s.over(&apps[2]), None);

        // A file where the directory should be: no storage.
        std::fs::write(base.join("odd"), b"").unwrap();
        assert_eq!(s.dir(&with_storage("odd"), None), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
