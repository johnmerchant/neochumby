//! `access = ["storage"]`: a directory of the app's own that lasts across
//! boots, /psp/apps/<name>/, shared by its exec and its service.
//!
//! The session makes it as it starts either, owned by the app's uid (every
//! file in it too: an app's uid follows its place in the tree, so it can
//! change between boots), mode 0700, and binds it into the jail at the same
//! path. NEOCHUMBY_STORAGE names it, jailed or not.
//!
//! /psp is the Chumby's settings flash, 2 MB in all, so each app may use
//! [`CAP`]. The kernel has no quotas, and a bind mount can't be limited, so
//! the session measures: every [`CHECK`] it adds up each running app's
//! directory and ends an app over its limit (its exec, or its service, as
//! the network check does). An app's limit is [`CAP`], or what it already
//! used as the session started if that was more, so an app left over the
//! cap can still run, and tidy up, but not grow. Like the network check,
//! this finds an app over its limit; it doesn't stop the writes that put it
//! there.
//!
//! What the session measures: each file, directory or link counts its size
//! rounded up to a [`BLOCK`], and at least one, so a pile of empty files
//! counts too. Links are never followed.

use std::io;
use std::os::unix::fs::{PermissionsExt, lchown};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use neochumby::apps::App;

/// Where the apps' directories are.
pub const BASE: &str = "/psp/apps";
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
            let own = m.len().div_ceil(BLOCK).max(1) * BLOCK;
            if m.is_dir() { own + usage(&e.path()) } else { own }
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

/// The apps' directories, and their limits.
pub struct Storage {
    base: PathBuf,
    /// Each app with `storage`, and the most it may use.
    limits: Vec<(String, u64)>,
    due: Instant,
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
        Storage { base, limits, due: Instant::now() + CHECK }
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
                Err(_) => std::fs::create_dir(&dir)?,
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
