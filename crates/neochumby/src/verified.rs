//! A tree mounted over NFS, checked: nothing from it runs, or is read by
//! the session, unless the manifest neochumbyd signed vouches for it
//! (neochumby_sync::signed has the manifest and the checks).
//!
//! NFS is unauthenticated: anyone who can answer for the server could
//! serve a tree of their own, and the Chumby runs its tree as root. So a
//! paired Chumby boots a tree from NFS like this:
//!
//! 1. The boot hook mounts the tree `noexec`, so nothing on it can run at
//!    all, and runs [`BOOT`], neochumby-boot, from /psp (the Chumby's own
//!    flash, which only root writes).
//! 2. neochumby-boot reads the tree's signed manifest, checks it with the
//!    key the Chumby pinned when it paired ([`boot`]), copies the session
//!    out of the tree into [`RUN`] (a tmpfs of its own, in RAM), checking
//!    it against the manifest as it goes, and runs that copy, telling it
//!    where the manifest is ([`ENV`]).
//! 3. The session reads the apps' manifests and the boot hook through
//!    [`read`], and runs each program through [`exe`]: a checked copy in
//!    [`RUN`], bound over the program's path in the app's jail, or run as
//!    it is for an unjailed app. A file the manifest doesn't name, or whose
//!    bytes differ, is refused, and the app doesn't start.
//!
//! What was checked is what runs: a copy is made from the bytes that were
//! hashed, and a small file is used as it was read, whatever NFS serves
//! later. What isn't checked is the data an app reads from its directory
//! as it runs (a game's levels, say).
//!
//! The session installs neochumby-boot in /psp from a tree it can trust
//! ([`install_boot`]), as it does the boot hook. Until one is there, as on
//! a Chumby paired before this, the tree is mounted and run unchecked.

use std::convert::Infallible;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use neochumby_proto::{PublicKey, hex, unhex_key};
use neochumby_sync::manifest::SESSION;
use neochumby_sync::signed::{self, Verified};

use crate::conf;

/// Where the tree is mounted over NFS.
pub const MNT: &str = "/mnt/neochumby";
/// neochumby-boot, installed in the Chumby's own flash.
pub const BOOT: &str = "/psp/neochumby-boot";
/// Its name in the tree.
pub const BOOT_IN_TREE: &str = "neochumby-boot";
/// The hash, in the tree's manifest, of the neochumby-boot installed.
const BOOT_SHA: &str = "/psp/neochumby-boot.sha256";
/// The tmpfs the checked copies run from, root's alone.
pub const RUN: &str = "/tmp/neochumby-run";
/// Set for the session by neochumby-boot: the signed manifest it checked,
/// copied into [`RUN`]. Unset, the tree is not checked (a stick's).
pub const ENV: &str = "NEOCHUMBY_VERIFIED";
/// The most RAM [`RUN`] takes: the session, and the programs running.
const RUN_KIB: u32 = 20 << 10;
/// The largest neochumby-boot written to /psp, which is 2 MB in all.
const BOOT_MAX: u64 = 768 << 10;
/// How often neochumby-boot tries the manifest: neochumbyd may be making
/// it anew just as the tree changes.
const TRIES: u32 = 3;

/// The checked tree: its root, the key that signs its manifest, and the
/// manifest as last checked.
struct Checked {
    root: PathBuf,
    key: PublicKey,
    manifest: Mutex<Option<Verified>>,
}

static CHECKED: OnceLock<Option<Checked>> = OnceLock::new();

/// The checked tree, if this session was started checked ([`ENV`]). Once
/// set, it stays: a manifest that won't load leaves every file refused.
fn checked() -> Option<&'static Checked> {
    CHECKED
        .get_or_init(|| {
            let path = std::env::var_os(ENV)?;
            let root = crate::apps::root();
            let key = paired_key().unwrap_or([0; 32]);
            let manifest = signed::read_capped(Path::new(&path), signed::MAX_LEN)
                .and_then(|text| signed::open(&text, &key))
                .map_err(|e| eprintln!("neochumby: the signed manifest: {e}; nothing in the tree runs"))
                .ok();
            Some(Checked { root, key, manifest: Mutex::new(manifest) })
        })
        .as_ref()
}

/// Whether this session checks the tree it runs from.
pub fn on() -> bool {
    checked().is_some()
}

/// The key of the neochumbyd this Chumby paired with.
pub fn paired_key() -> io::Result<PublicKey> {
    let hex = conf::get(conf::CONF, "server_key").ok_or_else(|| io::Error::other("not paired"))?;
    unhex_key(&hex)
}

impl Checked {
    /// `path`, relative to the tree's root, as the manifest names it.
    fn rel<'a>(&self, path: &'a Path) -> io::Result<&'a str> {
        path.strip_prefix(&self.root)
            .ok()
            .and_then(Path::to_str)
            .ok_or_else(|| io::Error::other(format!("{}: not in the tree", path.display())))
    }

    /// `f` with the manifest; if it fails, once more with the tree's
    /// manifest as it is now, if its signature checks: neochumbyd may have
    /// been given a new tree since this one was checked.
    fn with<T>(&self, f: impl Fn(&Verified) -> io::Result<T>) -> io::Result<T> {
        let mut m = self.manifest.lock().unwrap_or_else(|e| e.into_inner());
        let first = match m.as_ref() {
            Some(v) => f(v),
            None => Err(io::Error::other("no signed manifest")),
        };
        let Err(e) = first else { return first };
        match signed::load(&self.root, &self.key) {
            Ok(v) if m.as_ref().is_none_or(|old| old.digest != v.digest) => {
                eprintln!("neochumby: the tree's signed manifest is now {}", hex(&v.digest));
                let r = f(&v);
                *m = Some(v);
                r
            }
            _ => Err(e),
        }
    }
}

/// `path`, a file of the tree, read whole: checked against the signed
/// manifest if this session checks its tree.
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    match checked() {
        None => fs::read(path),
        Some(c) => {
            let rel = c.rel(path)?;
            c.with(|v| v.read(&c.root, rel))
        }
    }
}

/// What to run for `path`, a program in the tree: `path` itself if this
/// session doesn't check its tree; else a copy of it in [`RUN`], checked
/// against the signed manifest. Copies are kept by their hash, so a
/// program run again is not copied again.
pub fn exe(path: &Path) -> io::Result<PathBuf> {
    let Some(c) = checked() else { return Ok(path.to_path_buf()) };
    let rel = c.rel(path)?;
    c.with(|v| {
        let e = v.entry(rel)?;
        let copy = Path::new(RUN).join(&hex(&e.sha256)[..32]);
        if fs::symlink_metadata(&copy).is_ok_and(|m| m.is_file() && m.uid() == 0) {
            return Ok(copy);
        }
        v.copy(&c.root, rel, &copy, 0o755)?;
        Ok(copy)
    })
}

/// [`RUN`]: a tmpfs of its own (exec, but `nosuid,nodev`), root's alone.
/// Not /tmp itself, which may be `noexec`, and which anything may write.
fn prepare_run() -> io::Result<()> {
    let run = Path::new(RUN);
    match fs::symlink_metadata(run) {
        Ok(m) if !m.is_dir() || m.uid() != 0 => {
            return Err(io::Error::other(format!("{RUN} is not root's directory")));
        }
        Ok(_) => {}
        Err(_) => fs::create_dir(run)?,
    }
    let tmp = fs::metadata("/tmp")?;
    if fs::metadata(run)?.dev() == tmp.dev() {
        neochumby_sys::jail::mount_tmpfs(run, RUN_KIB, 0o700, 0, 0, false)?;
    }
    fs::set_permissions(run, fs::Permissions::from_mode(0o700))
}

/// neochumby-boot's work, and the session's on a restart: checks the tree
/// at `root` against its signed manifest, copies the session out of it and
/// runs that, in this process's place. Returns only on failure, having run
/// nothing from the tree.
pub fn boot(root: &Path) -> io::Error {
    match try_boot(root) {
        Ok(never) => match never {},
        Err(e) => e,
    }
}

fn try_boot(root: &Path) -> io::Result<Infallible> {
    let key = paired_key()?;
    let mut tries = 0;
    let (text, v) = loop {
        let r = signed::read_capped(&root.join(signed::FILE), signed::MAX_LEN)
            .and_then(|text| signed::open(&text, &key).map(|v| (text, v)));
        tries += 1;
        match r {
            Ok(r) => break r,
            Err(e) if tries >= TRIES => return Err(e),
            Err(_) => crate::sleep_ms(1000),
        }
    };
    prepare_run()?;
    // A session starting runs nothing yet: the copies an older one made
    // go, so that RAM holds only what this tree runs.
    for e in fs::read_dir(RUN)?.flatten() {
        if e.file_type().is_ok_and(|t| t.is_file()) {
            let _ = fs::remove_file(e.path());
        }
    }
    let manifest = Path::new(RUN).join("manifest");
    conf::write_atomic(&manifest, &text, 0o600)?;
    let session = Path::new(RUN).join(&hex(&v.entry(SESSION)?.sha256)[..32]);
    v.copy(root, SESSION, &session, 0o755)?;
    eprintln!("neochumby: the tree at {} checked: manifest {}", root.display(), hex(&v.digest));
    Err(Command::new(&session).env(ENV, &manifest).env("NEOCHUMBY_ROOT", root).exec())
}

/// Installs or updates [`BOOT`] from the tree at `root`, on a paired
/// Chumby. From the tree over NFS, only as its signed manifest has it, and
/// only when that differs from what was installed last (whose hash
/// [`BOOT_SHA`] keeps, so the tree's copy isn't read on every boot). From
/// a tree on the stick (a synced set, or the stick pairing runs from), as
/// it is, as the boot hook is, and only if there is none yet. True if it
/// was written.
pub fn install_boot(root: &Path) -> io::Result<bool> {
    let key = paired_key()?;
    let got = if root == Path::new(MNT) {
        match checked() {
            Some(c) => c.with(|v| staged_boot(v, &c.root))?,
            None => staged_boot(&signed::load(root, &key)?, root)?,
        }
    } else {
        let tree_boot = root.join(BOOT_IN_TREE);
        if Path::new(BOOT).exists() || !tree_boot.is_file() {
            return Ok(false);
        }
        // Of the ELF, only what is loaded, as a checked copy is.
        let mut b = signed::read_capped(&tree_boot, 64 << 20)?;
        b.truncate(signed::loaded_len(&b).unwrap_or(u64::MAX).min(b.len() as u64) as usize);
        if b.len() as u64 > BOOT_MAX {
            return Err(io::Error::other(format!("{}: larger than {BOOT_MAX} bytes", tree_boot.display())));
        }
        Some((b, None))
    };
    let Some((new, sha)) = got else { return Ok(false) };
    if fs::read(BOOT).ok().as_ref() != Some(&new) {
        // /psp is small, and holds the Chumby's own settings: never fill it.
        let free = neochumby_sys::fs::available_bytes(Path::new("/psp"))?;
        let old = fs::metadata(BOOT).map_or(0, |m| m.len());
        if free + old < new.len() as u64 + (256 << 10) {
            return Err(io::Error::other(format!("/psp has {free} bytes free; {BOOT} takes {}", new.len())));
        }
        if let Err(e) = conf::write_atomic(Path::new(BOOT), &new, 0o700) {
            let _ = fs::remove_file(Path::new(BOOT).with_extension("new"));
            return Err(e);
        }
    }
    match sha {
        Some(sha) => conf::write_atomic(Path::new(BOOT_SHA), format!("{sha}\n").as_bytes(), 0o644)?,
        None => {
            let _ = fs::remove_file(BOOT_SHA);
        }
    }
    Ok(true)
}

/// neochumby-boot as `v` has it in the tree at `root`, copied out checked
/// (what is loaded of it), with its hash in `v`; None if that is the one
/// installed already.
fn staged_boot(v: &Verified, root: &Path) -> io::Result<Option<(Vec<u8>, Option<String>)>> {
    let sha = hex(&v.entry(BOOT_IN_TREE)?.sha256);
    if fs::read_to_string(BOOT_SHA).is_ok_and(|s| s.trim() == sha) && Path::new(BOOT).is_file() {
        return Ok(None);
    }
    prepare_run()?;
    let staged = Path::new(RUN).join("boot");
    let r = v.copy(root, BOOT_IN_TREE, &staged, 0o700).and_then(|()| signed::read_capped(&staged, BOOT_MAX));
    let _ = fs::remove_file(&staged);
    Ok(Some((r?, Some(sha))))
}
