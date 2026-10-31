//! Where the session runs apps: each in a jail of its own
//! (neochumby_sys::jail), holding what its manifest's `access` lists and
//! nothing else, as a user of its own.
//!
//! The jail of an app, whether its `exec` (in front) or its service:
//!
//!   /mnt/neochumby/apps/<name>/   its directory, bound from the tree, read-
//!                                 only (the tree is); NEOCHUMBY_ROOT and its
//!                                 working directory
//!   /tmp                          its own tmpfs (nosuid, nodev, noexec), HOME
//!   /dev/null zero random urandom
//!
//! and, as its `access` lists:
//!
//!   fb        /dev/fb0                     (its exec only; without it, its
//!             exec has a terminal as its stdin and stdout, which the
//!             session shows: neochumby::term)
//!   touch     the touchscreen (neochumby::input::touchscreen), and the
//!             calibration, /psp/ts_settings, read-only (its exec only)
//!   speaker   /dev/dsp, and /tmp/neochumby, where processes hand the
//!             speaker over (neochumby::audio), shared by every app that
//!             lists it (a nosuid, nodev, noexec tmpfs); the mute and the
//!             volume are the session's
//!   accel     /dev/accel
//!   system    /proc, /sys and /etc; where /etc is on the initramfs's
//!             rootfs, which 2.6.16 won't bind from, copies of the /etc
//!             files anyone may read instead (copy_etc)
//!   network   copies of /etc/resolv.conf and /etc/hosts (with `system`,
//!             /etc has them); and the session lets it keep internet
//!             sockets (network.rs)
//!   terminal  /dev/ptmx and the Chumby's /dev/pts, for pseudo-terminals
//!             (neochumby::term)
//!   storage   /psp/apps/<name>, its own, lasting across boots and shared
//!             by its exec and service (storage.rs); sealed nosuid, nodev
//!             and noexec (Jail::seal: 2.6.16 and 5.9 both can), or, on a
//!             kernel that can't seal a bind mount, as /psp is (said once)
//!   logs      the Chumby's logs, each file bound at its own path: the
//!             session's (/tmp/neochumby.log), /tmp's other *.log and
//!             /var/log's files, as they are when the app starts; only
//!             plain files, none setuid or setgid, readable as their
//!             modes say (the session's log is root's, 0644)
//!
//! Each app runs as its own uid (from FIRST_UID, in the tree's name order:
//! an app's exec and its service share one), with a group of the same id
//! and, if it lists `speaker`, SPEAKER_GID beside it. Its devices are made
//! anew in its jail, its own; the host's /dev is left as it is. It may have
//! PROCESSES processes, FILES descriptors and no core dumps.
//!
//! In a checked tree (neochumby::verified), mounted `noexec`, each of the
//! app's programs (its exec or service, and every other ARM ELF or script
//! in its directory that is executable) is bound over from its checked
//! copy: only those can run in the jail, and only as checked.
//!
//! `access = ["root"]` (home) and the session's own services
//! (neochumby-sync) run as before: root, unjailed.
//!
//! Jails need root and the mounts it can make. Where neither is allowed
//! (a test harness that runs the session unprivileged), NEOCHUMBY_JAIL=off
//! turns them off, and the session then runs every app as root, saying so
//! once.

use std::ffi::OsStr;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};
use std::path::Path;
use std::sync::OnceLock;

use neochumby::apps::App;
use neochumby::input;
use neochumby_sys::jail::{Jail, Limit, Process, User};

/// Where each jail's root is mounted, in its own mount namespace.
const MOUNT_POINT: &str = "/tmp/neochumby-jail";
/// The first app's uid.
const FIRST_UID: u32 = 20000;
/// The group that may hand the speaker over.
const SPEAKER_GID: u32 = 19999;
/// The variable that turns jails off, for test harnesses.
pub const OFF_ENV: &str = "NEOCHUMBY_JAIL";

/// What looking hosts up needs, for `network`: copies, as the app starts.
const NAMES: [&str; 2] = ["/etc/resolv.conf", "/etc/hosts"];

const PROCESSES: u32 = 64;
const FILES: u32 = 256;
/// The jail's root and the app's /tmp, in KiB.
const ROOT_KIB: u32 = 256;
const TMP_KIB: u32 = 4096;
/// How much of /etc a jail's root holds when /etc is copied, in KiB, and
/// how many levels of its directories.
const ETC_KIB: usize = 128;
const ETC_DEPTH: u32 = 4;
/// Whether /etc is on rootfs, so copied rather than bound (on_rootfs).
static ETC_ON_ROOTFS: OnceLock<bool> = OnceLock::new();

/// What an app is started as: its exec, in front, or its service.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Front,
    Service,
}

/// The apps' names, in the tree's order, which gives their uids.
static JAILS: OnceLock<Vec<String>> = OnceLock::new();
/// Whether the session has said that storage is executable on this
/// kernel.
static UNSEALED: std::sync::Once = std::sync::Once::new();
/// Whether [`prepare`] succeeded. When it hasn't, [`start`] refuses every
/// app rather than running one unjailed.
static READY: OnceLock<bool> = OnceLock::new();

/// Turns jails on for `apps` (the tree's, in name order), unless they
/// can't be: not root, or NEOCHUMBY_JAIL=off. Once, as the session starts;
/// until then, and in tests, nothing is jailed.
pub fn init(apps: &[App]) {
    if std::env::var(OFF_ENV).as_deref() == Ok("off") {
        eprintln!("neochumby: {OFF_ENV}=off: every app runs as root, unjailed");
        return;
    }
    if !neochumby_sys::process::is_root() {
        eprintln!("neochumby: not root: apps run as this user, unjailed");
        return;
    }
    // Jails stay on even so: an app's own start fails, and says why.
    let ready = match prepare() {
        Ok(()) => true,
        Err(e) => {
            eprintln!("neochumby: preparing jails: {e}");
            false
        }
    };
    let _ = READY.set(ready);
    let _ = JAILS.set(apps.iter().map(|a| a.name.clone()).collect());
}

/// The mount point, and /tmp/neochumby for the speaker's handover: the
/// session's, and SPEAKER_GID's. The handover is a tmpfs of its own,
/// `nosuid,nodev,noexec`: it is the one host path a speaker app may write,
/// so nothing executable may be put there.
fn prepare() -> io::Result<()> {
    std::fs::create_dir_all(MOUNT_POINT)?;
    std::fs::set_permissions(MOUNT_POINT, std::fs::Permissions::from_mode(0o700))?;
    let speaker = neochumby_sys::speaker::DIR;
    std::fs::create_dir_all(speaker)?;
    // A fresh tmpfs on /tmp/neochumby. If the session is restarted, the
    // directory is already one of ours: its st_dev differs from /tmp's.
    let tmp = std::fs::metadata("/tmp")?;
    let handover = std::fs::metadata(speaker)?;
    if tmp.dev() == handover.dev() {
        neochumby_sys::jail::mount_tmpfs(Path::new(speaker), 64, 0o770, 0, SPEAKER_GID, true)?;
    }
    chown(speaker, Some(0), Some(SPEAKER_GID))?;
    std::fs::set_permissions(speaker, std::fs::Permissions::from_mode(0o770))
}

/// Whether `app` runs jailed.
pub fn jailed(app: &App) -> bool {
    JAILS.get().is_some() && !app.access.root
}

/// The number `app` runs as, jailed: its uid, and its group.
fn number(app: &App) -> io::Result<u32> {
    let names = JAILS.get().ok_or_else(|| io::Error::other("jails are off"))?;
    let i = names.iter().position(|n| *n == app.name).ok_or_else(|| io::Error::other("not in the tree"))?;
    Ok(FIRST_UID + i as u32)
}

/// The uid `app` runs as, if it runs jailed.
pub fn owner(app: &App) -> Option<u32> {
    if jailed(app) { number(app).ok() } else { None }
}

/// The uid of `app`, if it runs jailed without `network`: one the session
/// ends if it finds it with an internet socket.
pub fn offline(app: &App) -> Option<u32> {
    (jailed(app) && !app.access.network).then(|| number(app).ok()).flatten()
}

/// The Chumby's logs, for `logs`: plain files only, none setuid or setgid
/// (a bind mount keeps its source's flags, and /var/log may not be
/// nosuid), so no other kind of file comes into the jail with them.
fn logs() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    for (dir, only_logs) in [("/tmp", true), ("/var/log", false)] {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            // symlink_metadata: a link isn't followed out of the directory.
            let plain = std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file() && m.mode() & 0o6000 == 0);
            if plain && (!only_logs || p.extension().is_some_and(|x| x == "log")) {
                v.push(p);
            }
        }
    }
    v.sort();
    v
}

/// Whether `path` is on rootfs, the initramfs's own filesystem: 2.6.16
/// refuses to bind anything from it (its superblock is MS_NOUSER, and
/// graft_tree says EINVAL), as on the classic image, where the initramfs is
/// the root. The last mount listed with the longest prefix of `path` is
/// the one it is on.
fn on_rootfs(path: &str) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else { return false };
    let mut on: Option<(usize, bool)> = None;
    for line in mounts.lines() {
        let mut f = line.split(' ');
        let (Some(_), Some(at), Some(fstype)) = (f.next(), f.next(), f.next()) else { continue };
        let covers = at == "/" || path == at || path.strip_prefix(at).is_some_and(|r| r.starts_with('/'));
        if covers && on.is_none_or(|(len, _)| at.len() >= len) {
            on = Some((at.len(), fstype == "rootfs"));
        }
    }
    on.is_some_and(|(_, rootfs)| rootfs)
}

/// Copies into the jail, at the same paths, what a bind of `dir` would let
/// the app's uid read: the plain files anyone may read, in directories
/// anyone may search, `depth` levels down at most. Each is root's and
/// read-only, keeping only its read and execute bits. Links are left out,
/// as are files past `left` bytes, which the jail's root must hold.
fn copy_etc(j: &mut Jail, dir: &Path, depth: u32, left: &mut usize) -> io::Result<()> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(()) };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let (Ok(m), Some(at)) = (std::fs::symlink_metadata(&p), p.to_str()) else { continue };
        if m.is_dir() && m.mode() & 0o005 == 0o005 && depth < ETC_DEPTH {
            j.dirs(at)?;
            copy_etc(j, &p, depth + 1, left)?;
        } else if m.is_file() && m.mode() & 0o004 != 0 {
            let Ok(c) = std::fs::read(&p) else { continue };
            if c.len() > *left {
                eprintln!("neochumby: /etc is larger than a jail holds: {at} and others left out");
                *left = 0;
                continue;
            }
            *left -= c.len();
            j.file(at, c, m.mode() & 0o555, 0, 0)?;
        }
    }
    Ok(())
}

/// What the session gives a jailed app of its own, beside what its access
/// lists.
#[derive(Clone, Copy, Default)]
pub struct Given<'a> {
    /// Its directory in /psp (storage.rs), bound in at the same path.
    pub storage: Option<&'a Path>,
    /// Its terminal, as its stdin and stdout (terminal.rs).
    pub terminal: Option<RawFd>,
}

/// Starts `exe` of `app` jailed, as `role`, with the tree at `root`, only
/// `env` (and HOME, NEOCHUMBY_ROOT, PATH) in its environment, the
/// descriptors in `keep`, and what `given` holds.
pub fn start(
    app: &App,
    exe: &Path,
    role: Role,
    root: &Path,
    mut env: Vec<(String, String)>,
    keep: &[RawFd],
    given: Given,
) -> io::Result<Process> {
    if !READY.get().copied().unwrap_or(false) {
        return Err(io::Error::other("the jail couldn't be prepared"));
    }
    // The ELF loader maps a PT_LOAD at vaddr 0 even though the ARM mmap
    // path refuses MAP_FIXED below PAGE_SIZE. Refuse such a file before
    // exec, wherever it hides in the app's directory.
    neochumby_sys::elf::check(exe)?;
    neochumby_sys::elf::check_app_dir(&app.dir)?;
    let programs = programs(app, exe)?;
    let uid = number(app)?;
    let a = app.access;
    let front = role == Role::Front;
    let path = |p: &Path| {
        p.to_str().map(str::to_string).ok_or_else(|| io::Error::other(format!("{}: not UTF-8", p.display())))
    };
    let dir = path(&app.dir)?;
    let exe = path(exe)?;

    let mut j = Jail::new(Path::new(MOUNT_POINT), ROOT_KIB)?;
    j.bind(&app.dir, &dir)?;
    for (copy, at) in &programs {
        j.bind_over(copy, at)?;
    }
    j.tmpfs("/tmp", TMP_KIB, 0o700, uid, uid)?;
    for d in ["null", "zero", "random", "urandom"] {
        let p = format!("/dev/{d}");
        j.device(&p, Path::new(&p), 0o666, 0, 0)?;
    }
    if a.fb && front {
        j.device("/dev/fb0", Path::new("/dev/fb0"), 0o600, uid, uid)?;
    }
    if a.touch && front {
        // At the same path, which the app is told (input::TOUCHSCREEN_ENV).
        let touch = input::touchscreen();
        j.device(&touch, Path::new(&touch), 0o600, uid, uid)?;
        env.push((input::TOUCHSCREEN_ENV.into(), touch));
        if let Ok(c) = std::fs::read(input::CALIBRATION) {
            j.file(input::CALIBRATION, c, 0o444, 0, 0)?;
        }
    }
    let mut groups = Vec::new();
    if a.speaker {
        j.device("/dev/dsp", Path::new("/dev/dsp"), 0o600, uid, uid)?;
        let handover = neochumby_sys::speaker::DIR;
        j.bind(Path::new(handover), handover)?;
        groups.push(SPEAKER_GID);
    }
    if a.accel {
        j.device("/dev/accel", Path::new("/dev/accel"), 0o600, uid, uid)?;
    }
    if a.system {
        j.kernel_fs("proc", "/proc")?;
        j.kernel_fs("sysfs", "/sys")?;
        if *ETC_ON_ROOTFS.get_or_init(|| on_rootfs("/etc")) {
            let mut left = ETC_KIB * 1024;
            j.dirs("/etc")?;
            copy_etc(&mut j, Path::new("/etc"), 0, &mut left)?;
        } else {
            j.bind(Path::new("/etc"), "/etc")?;
        }
    }
    if a.terminal {
        // The Chumby's own devpts (the only one this kernel has): another
        // uid's terminals are listed there but closed to this one.
        j.device("/dev/ptmx", Path::new("/dev/ptmx"), 0o666, 0, 0)?;
        j.bind(Path::new("/dev/pts"), "/dev/pts")?;
    }
    if let Some(s) = given.storage {
        // Its own, already: the session made it so (storage.rs). Sealed,
        // so nothing the app writes there runs. Where the kernel can't seal
        // it, it is as /psp is, not `nosuid`, but nothing setuid in it can
        // be any uid but the app's.
        let at = path(s)?;
        j.bind(s, &at)?;
        j.seal(s, &at)?;
    }
    if a.logs {
        for f in logs() {
            if let Some(p) = f.to_str() {
                j.bind(&f, p)?;
            }
        }
    }
    if a.network && !a.system {
        for f in NAMES {
            if let Ok(c) = std::fs::read(f) {
                j.file(f, c, 0o444, 0, 0)?;
            }
        }
    }
    j.limit(Limit::Processes(PROCESSES)).limit(Limit::Files(FILES)).limit(Limit::Core(0));
    if let Some(t) = given.terminal {
        j.stdio(t, t);
    }

    env.push(("NEOCHUMBY_ROOT".into(), path(root)?));
    env.push(("HOME".into(), "/tmp".into()));
    env.push(("PATH".into(), dir.clone()));
    // What the boot hook gave the session for its apps (the server, say),
    // and the time zone.
    for (k, v) in std::env::vars() {
        // Not NEOCHUMBY_PLAYER: only the session's choice (child.rs).
        // Nor the checked tree's manifest, which is root's.
        let ours = k.starts_with("NEOCHUMBY_")
            && k != OFF_ENV
            && k != neochumby::session::PLAYER_ENV
            && k != neochumby::verified::ENV;
        if (ours && !env.iter().any(|(e, _)| *e == k)) || k == "TZ" {
            env.push((k, v));
        }
    }
    let user = User { uid, gid: uid, groups };
    let args: [&OsStr; 0] = [];
    let p = j.spawn(&exe, &args, &env, &dir, &user, keep)?;
    if p.unsealed() {
        UNSEALED.call_once(|| eprintln!("neochumby: this kernel can't seal a bind mount: storage is executable"));
    }
    Ok(p)
}

/// In a checked tree, `app`'s programs: `exe`, and every executable ARM
/// ELF or script in its directory, each with its checked copy (refused if
/// it isn't the file the signed manifest names). None otherwise. The tree
/// is mounted `noexec`, so a file not among them can't run, whatever it
/// is by the time it would; each copy is checked as `exe` is.
fn programs(app: &App, exe: &Path) -> io::Result<Vec<(std::path::PathBuf, String)>> {
    if !neochumby::verified::on() {
        return Ok(Vec::new());
    }
    let mut found = vec![exe.to_path_buf()];
    executables(&app.dir, 0, &mut found)?;
    found.sort();
    found.dedup();
    let mut out = Vec::with_capacity(found.len());
    for p in found {
        let copy = neochumby::verified::exe(&p)?;
        neochumby_sys::elf::check(&copy)?;
        let at = p.to_str().ok_or_else(|| io::Error::other(format!("{}: not UTF-8", p.display())))?;
        out.push((copy, at.to_string()));
    }
    Ok(out)
}

/// Executable plain files under `dir` that the Chumby could run: an ARM
/// ELF, or a script. (A companion, built for the server, can't.)
fn executables(dir: &Path, depth: usize, out: &mut Vec<std::path::PathBuf>) -> io::Result<()> {
    if depth > 8 {
        return Ok(());
    }
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        let m = std::fs::symlink_metadata(&p)?;
        if m.is_dir() {
            executables(&p, depth + 1, out)?;
        } else if m.is_file() && m.mode() & 0o111 != 0 && runnable(&p) {
            out.push(p);
        }
    }
    Ok(())
}

/// Whether `p` starts as an ARM ELF (32-bit, little-endian, machine 40)
/// or a script does.
fn runnable(p: &Path) -> bool {
    use std::io::Read;
    let mut h = Vec::new();
    let _ = std::fs::File::open(p).and_then(|f| f.take(20).read_to_end(&mut h));
    h.starts_with(b"#!") || (h.len() == 20 && h.starts_with(b"\x7fELF\x01\x01") && h[18..20] == [40, 0])
}
