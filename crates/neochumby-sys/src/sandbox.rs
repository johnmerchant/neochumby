//! Confinement for the programs neochumbyd runs for apps: their server
//! programs and companions. neochumbyd's state directory holds its key
//! (`identity`), the pairings and the owner's token, and these programs run
//! as neochumbyd's user, so without this any app's program could read them.
//!
//! - **Linux: Landlock** (5.13 on, unprivileged). The rules are built in
//!   neochumbyd, the parent; the child, between fork and exec, only sets
//!   no_new_privs and turns them on, and fails to start if it can't. A
//!   confined program may:
//!   - read and run the system ([`SYSTEM`]: `/usr`, `/etc`, `/proc` and so
//!     on), and use `/dev/null` and the random devices;
//!   - read and run the tree;
//!   - read and write its app's own directory, which holds its TMPDIR.
//!
//!   Nothing else: not the state directory, not other apps' directories,
//!   not `/home` or `/tmp`. Every file-system right the kernel's ABI knows
//!   is handled. From ABI 4, a server program may bind only the TCP ports
//!   its manifest lists; from ABI 6, it can't signal processes outside its
//!   domain or reach their abstract Unix sockets. From ABI 1, it can't
//!   ptrace them or read their memory. [`Sandbox::gaps`] says what the
//!   running kernel leaves open.
//! - **macOS: Apple's sandbox**, by running the program through
//!   `/usr/bin/sandbox-exec` with [`APPLE_PROFILE`]: it may write only its
//!   own directory (and `/dev/null`), read nothing in the user's home
//!   directory or the state directory but the tree and its own directory,
//!   and connect to no Unix socket in the state directory. It may read the
//!   rest of the system, and use the network. Apple has deprecated
//!   sandbox-exec and doesn't document the profile language, so the
//!   profile is kept to these few lines. macOS is for development.
//! - **Anywhere else**, [`detect`] says why not, and neochumbyd runs no
//!   program unconfined unless its owner says so.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What confines programs here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sandbox {
    /// Linux's Landlock, at this ABI version.
    Landlock(u32),
    /// Apple's sandbox, through sandbox-exec.
    Apple,
}

/// Where a confined program may go.
pub struct Confinement<'a> {
    /// neochumbyd's state directory.
    pub state: &'a Path,
    /// The app tree, to read and run.
    pub tree: &'a Path,
    /// The app's own directory, in the state directory, to read and write.
    pub own: &'a Path,
    /// The home directory of the user neochumbyd runs as, closed but for
    /// the tree and `own`. On Linux it's closed anyway: it isn't in
    /// [`SYSTEM`].
    pub home: Option<&'a Path>,
    /// The TCP ports it may bind, enforced from Landlock ABI 4; None leaves
    /// binding as it is.
    pub bind: Option<&'a [u16]>,
}

/// What a confined program may read and run, on Linux, besides the tree
/// and its own directory: the system, not anyone's files. A path that is
/// (or is above) the state directory gives only what is beside the way
/// down to it.
pub const SYSTEM: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/etc",
    "/opt",
    "/nix/store",
    "/proc",
    "/sys",
    "/run",
];

/// Devices it may read and write.
pub const DEVICES: &[&str] = &["/dev/null", "/dev/zero", "/dev/full", "/dev/random", "/dev/urandom"];

/// The sandbox this system has, or why it has none.
pub fn detect() -> Result<Sandbox, String> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    return landlock::abi().map(Sandbox::Landlock);
    #[cfg(target_os = "macos")]
    return apple::check().map(|()| Sandbox::Apple);
    #[allow(unreachable_code)]
    Err("no sandbox neochumbyd knows on this system (Landlock on Linux, Apple's on macOS)".into())
}

impl Sandbox {
    /// What it is, for the log.
    pub fn describe(&self) -> String {
        match self {
            Sandbox::Landlock(abi) => format!("Landlock, ABI {abi}"),
            Sandbox::Apple => "Apple's sandbox (sandbox-exec)".into(),
        }
    }

    /// What it doesn't confine, for the log.
    pub fn gaps(&self) -> Vec<&'static str> {
        let mut g = Vec::new();
        match *self {
            Sandbox::Landlock(abi) => {
                if abi < 2 {
                    g.push("moving files between directories is refused altogether (ABI 1)");
                }
                if abi < 3 {
                    g.push("truncating a file it can open (ABI 3)");
                }
                if abi < 4 {
                    g.push("binding TCP ports (ABI 4)");
                }
                if abi < 6 {
                    g.push("signalling other processes, and their abstract Unix sockets (ABI 6)");
                }
                if abi > 6 {
                    g.push("what this kernel's ABI adds after 6, which neochumbyd doesn't use yet");
                }
                g.push("chmod, chown, utime and extended attributes, of files it can reach");
                g.push("outbound TCP and UDP");
            }
            Sandbox::Apple => {
                g.push("reading files outside the home and state directories");
                g.push("the network, and Unix sockets outside the state directory");
            }
        }
        g.push("CPU and memory");
        g
    }

    /// A Command that runs `program` confined to `c`. Add arguments,
    /// environment and the rest to it as usual.
    pub fn command(&self, program: &Path, c: &Confinement) -> io::Result<Command> {
        let state = c.state.canonicalize()?;
        let tree = c.tree.canonicalize()?;
        let own = c.own.canonicalize()?;
        if !own.starts_with(&state) || own == state {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the app's directory must be in the state directory",
            ));
        }
        match *self {
            #[cfg(any(target_os = "linux", target_os = "android"))]
            Sandbox::Landlock(abi) => landlock::command(abi, program, &state, &tree, &own, c.bind),
            #[cfg(target_os = "macos")]
            Sandbox::Apple => {
                let home = match c.home.map(Path::canonicalize) {
                    Some(Ok(h)) => h,
                    _ => own.clone(),
                };
                Ok(apple::command(program, &state, &tree, &own, &home))
            }
            #[allow(unreachable_patterns)]
            _ => Err(io::Error::new(io::ErrorKind::Unsupported, "not this system's sandbox")),
        }
    }
}

/// What to allow for `path`, keeping out of `inner`: `path` itself, or, if
/// it is `inner` or above it, every entry beside the way down to `inner`.
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
fn except(path: &Path, inner: &Path) -> Vec<PathBuf> {
    if !inner.starts_with(path) {
        return vec![path.to_path_buf()];
    }
    let mut out = Vec::new();
    let mut dir = inner;
    while let Some(parent) = dir.parent() {
        if !parent.starts_with(path) {
            break;
        }
        if let Ok(rd) = std::fs::read_dir(parent) {
            out.extend(rd.flatten().map(|e| e.path()).filter(|p| p != dir));
        }
        dir = parent;
    }
    out
}

/// The profile for Apple's sandbox: no writing but to the app's own
/// directory and the null devices; no reading the user's home directory or
/// neochumbyd's state directory but for the tree and the app's own
/// directory; no connecting to a Unix socket in the state directory (the
/// file rules don't cover that). The last rule that matches wins.
pub const APPLE_PROFILE: &str = r#"(version 1)
(allow default)
(deny file-write*)
(allow file-write* (literal "/dev/null") (literal "/dev/zero"))
(deny file-read* (subpath (param "HOME")))
(deny file-read* file-write* (subpath (param "STATE")))
(deny network-outbound (remote unix-socket (subpath (param "STATE"))))
(allow file-read* (subpath (param "TREE")))
(allow file-read* file-write* (subpath (param "OWN")))
"#;

#[cfg(target_os = "macos")]
mod apple {
    use super::*;

    const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

    /// Whether sandbox-exec is there and can apply a profile: it can't from
    /// inside another sandbox.
    pub fn check() -> Result<(), String> {
        let out = Command::new(SANDBOX_EXEC)
            .args(["-p", "(version 1)\n(allow default)\n", "/usr/bin/true"])
            .output()
            .map_err(|e| format!("{SANDBOX_EXEC}: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("{SANDBOX_EXEC} can't apply a profile here: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    pub fn command(program: &Path, state: &Path, tree: &Path, own: &Path, home: &Path) -> Command {
        let mut cmd = Command::new(SANDBOX_EXEC);
        cmd.arg("-p").arg(APPLE_PROFILE);
        for (k, v) in [("STATE", state), ("TREE", tree), ("OWN", own), ("HOME", home)] {
            let mut d = std::ffi::OsString::from(format!("{k}="));
            d.push(v);
            cmd.arg("-D").arg(d);
        }
        cmd.arg(program);
        cmd
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod landlock {
    use std::fs::OpenOptions;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;

    use super::*;

    // From <linux/landlock.h>: a stable kernel ABI.
    const CREATE_RULESET_VERSION: u32 = 1 << 0;
    const RULE_PATH_BENEATH: libc::c_int = 1;
    const RULE_NET_PORT: libc::c_int = 2;
    const EXECUTE: u64 = 1 << 0;
    const WRITE_FILE: u64 = 1 << 1;
    const READ_FILE: u64 = 1 << 2;
    const READ_DIR: u64 = 1 << 3;
    /// ABI 1's rights: the thirteen up to MAKE_SYM.
    const ABI1: u64 = (1 << 13) - 1;
    const REFER: u64 = 1 << 13;
    const TRUNCATE: u64 = 1 << 14;
    const IOCTL_DEV: u64 = 1 << 15;
    /// The rights that apply to a file; the rest are a directory's.
    const FILE: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE | IOCTL_DEV;
    const NET_BIND_TCP: u64 = 1 << 0;
    const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
    const SCOPE_SIGNAL: u64 = 1 << 1;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
        handled_access_net: u64,
        scoped: u64,
    }

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    #[repr(C, packed)]
    struct NetPortAttr {
        allowed_access: u64,
        port: u64,
    }

    /// The kernel's Landlock ABI version, or why there's none.
    pub fn abi() -> Result<u32, String> {
        // SAFETY: with a null attribute and a size of 0, the VERSION flag
        // asks only for the version; the kernel reads and writes nothing.
        let v = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                CREATE_RULESET_VERSION,
            )
        };
        if v >= 1 {
            return Ok(v as u32);
        }
        let e = io::Error::last_os_error();
        Err(match e.raw_os_error() {
            Some(libc::ENOSYS) => "this kernel has no Landlock (Linux 5.13 or later, built with it)".into(),
            Some(libc::EOPNOTSUPP) => "Landlock is turned off on this kernel (it isn't in lsm= at boot)".into(),
            _ => format!("Landlock: {e}"),
        })
    }

    /// Every file-system right Landlock knows at `abi` (up to 6): all are
    /// handled, so each is refused unless a rule allows it.
    fn handled(abi: u32) -> u64 {
        let mut r = ABI1;
        if abi >= 2 {
            r |= REFER;
        }
        if abi >= 3 {
            r |= TRUNCATE;
        }
        if abi >= 5 {
            r |= IOCTL_DEV;
        }
        r
    }

    struct Ruleset {
        fd: OwnedFd,
        handled: u64,
    }

    impl Ruleset {
        fn new(abi: u32, bind: bool) -> io::Result<Ruleset> {
            let attr = RulesetAttr {
                handled_access_fs: handled(abi),
                handled_access_net: if bind && abi >= 4 { NET_BIND_TCP } else { 0 },
                scoped: if abi >= 6 { SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL } else { 0 },
            };
            // SAFETY: the kernel reads `size_of::<RulesetAttr>()` bytes from
            // `attr`, a live value of that size. An older kernel's struct is
            // shorter, and it accepts the rest when it's zero, as it is for
            // the fields it predates.
            let fd = unsafe {
                libc::syscall(libc::SYS_landlock_create_ruleset, &raw const attr, size_of::<RulesetAttr>(), 0u32)
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the kernel returned a new descriptor (close-on-exec),
            // which nothing else owns.
            let fd = unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) };
            Ok(Ruleset { fd, handled: handled(abi) })
        }

        /// Allows `access` beneath `path`. A symbolic link is passed over
        /// (what it points to is judged where it is), and so is anything
        /// that can't be opened, such as what isn't on this system.
        fn allow(&self, path: &Path, access: u64) -> io::Result<()> {
            let Ok(f) = OpenOptions::new().read(true).custom_flags(libc::O_PATH | libc::O_NOFOLLOW).open(path) else {
                return Ok(());
            };
            let t = f.metadata()?.file_type();
            if t.is_symlink() {
                return Ok(());
            }
            let access = access & self.handled & if t.is_dir() { u64::MAX } else { FILE };
            if access == 0 {
                return Ok(());
            }
            let attr = PathBeneathAttr { allowed_access: access, parent_fd: f.as_raw_fd() };
            // SAFETY: the kernel reads one PathBeneathAttr from `attr`, a
            // live value; both descriptors are open while borrowed here.
            let r = unsafe {
                libc::syscall(
                    libc::SYS_landlock_add_rule,
                    self.fd.as_raw_fd(),
                    RULE_PATH_BENEATH,
                    &raw const attr,
                    0u32,
                )
            };
            if r != 0 {
                return Err(io::Error::other(format!("{}: {}", path.display(), io::Error::last_os_error())));
            }
            Ok(())
        }

        fn allow_bind(&self, port: u16) -> io::Result<()> {
            let attr = NetPortAttr { allowed_access: NET_BIND_TCP, port: port.into() };
            // SAFETY: the kernel reads one NetPortAttr from `attr`, a live
            // value; the descriptor is open while borrowed here.
            let r = unsafe {
                libc::syscall(libc::SYS_landlock_add_rule, self.fd.as_raw_fd(), RULE_NET_PORT, &raw const attr, 0u32)
            };
            if r != 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
        }
    }

    pub fn command(
        abi: u32,
        program: &Path,
        state: &Path,
        tree: &Path,
        own: &Path,
        bind: Option<&[u16]>,
    ) -> io::Result<Command> {
        let rules = Ruleset::new(abi, bind.is_some())?;
        let read = READ_FILE | READ_DIR | EXECUTE;
        for p in SYSTEM {
            for q in except(Path::new(p), state) {
                rules.allow(&q, read)?;
            }
        }
        for d in DEVICES {
            rules.allow(Path::new(d), READ_FILE | WRITE_FILE | TRUNCATE | IOCTL_DEV)?;
        }
        // The tree, in the state directory or not (and if it's above it,
        // what's beside the way down).
        for q in except(tree, state) {
            rules.allow(&q, read)?;
        }
        rules.allow(own, rules.handled)?;
        if abi >= 4 {
            for &port in bind.unwrap_or_default() {
                rules.allow_bind(port)?;
            }
        }

        let mut cmd = Command::new(program);
        let fd = rules.fd;
        // SAFETY: the closure runs in the forked child, where only
        // async-signal-safe calls are allowed: it makes two system calls,
        // with integers and a descriptor the parent opened (and keeps open
        // while `cmd` lives), and allocates nothing. If either fails, the
        // program isn't started.
        unsafe {
            cmd.pre_exec(move || {
                // prctl's arguments are unsigned longs, passed as such.
                let (one, zero): (libc::c_ulong, libc::c_ulong) = (1, 0);
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, one, zero, zero, zero) != 0
                    || libc::syscall(libc::SYS_landlock_restrict_self, fd.as_raw_fd(), 0u32) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program confined to an app's directory can't read neochumbyd's key
    /// or the owner's token, change them, chmod them (where the sandbox
    /// covers that), or reach another app's directory, and can't write the
    /// tree. It can read the tree and read and write its own directory.
    #[test]
    fn confined() {
        let sandbox = match detect() {
            Ok(s) => s,
            // A Linux CI runner must have Landlock, or this tests nothing.
            Err(why) if std::env::var_os("CI").is_some() && cfg!(target_os = "linux") => panic!("{why}"),
            Err(why) => return eprintln!("skipped: {why}"),
        };
        eprintln!("sandbox: {}; not confined: {}", sandbox.describe(), sandbox.gaps().join("; "));
        let dir = std::env::temp_dir().join(format!("neochumby-sandbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (state, home) = (dir.join("state"), dir.join("home"));
        let (tree, own, other) = (state.join("tree"), state.join("apps/web"), state.join("apps/other"));
        for d in [&tree, &own, &other, &home] {
            std::fs::create_dir_all(d).unwrap();
        }
        for f in [state.join("identity"), state.join("owner-token"), other.join("secret"), tree.join("manifest")] {
            std::fs::write(f, "x\n").unwrap();
        }
        std::fs::write(home.join(".ssh-key"), "x\n").unwrap();
        let c = Confinement { state: &state, tree: &tree, own: &own, home: Some(&home), bind: Some(&[]) };
        let sh = |script: &str| {
            let mut cmd = sandbox.command(Path::new("/bin/sh"), &c).unwrap();
            cmd.arg("-c").arg(script).current_dir(&own);
            cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            cmd.status().unwrap().success()
        };
        let p = |p: PathBuf| p.display().to_string();
        assert!(sh("exit 0"), "runs at all");
        for secret in ["identity", "owner-token"] {
            let f = p(state.join(secret));
            assert!(!sh(&format!("cat '{f}'")), "read {secret}");
            assert!(!sh(&format!("echo y > '{f}'")), "wrote {secret}");
            assert!(!sh(&format!("rm -f '{f}'")) || state.join(secret).exists(), "removed {secret}");
            // Landlock doesn't cover chmod: a gap, in SECURITY.md.
            if sandbox == Sandbox::Apple {
                assert!(!sh(&format!("chmod 666 '{f}'")), "chmod {secret}");
            }
        }
        assert!(!sh(&format!("cat '{}'", p(other.join("secret")))), "read another app's directory");
        assert!(!sh(&format!("echo y > '{}'", p(state.join("paired")))), "wrote in the state directory");
        assert!(sh(&format!("cat '{}'", p(tree.join("manifest")))), "read the tree");
        assert!(!sh(&format!("echo y > '{}'", p(tree.join("manifest")))), "wrote the tree");
        assert!(sh(&format!("echo y > '{0}' && cat '{0}' && rm '{0}'", p(own.join("f")))), "own directory");
        assert!(sh("echo y > /dev/null && head -c 4 /dev/urandom > /dev/null"), "devices");
        assert!(!sh(&format!("cat '{}'", p(home.join(".ssh-key")))), "read the home directory");
        assert!(!sh(&format!("echo y > '{}'", p(home.join(".profile")))), "wrote the home directory");
        assert!(!sh(&format!("echo y > '{}'", p(dir.join("beside")))), "wrote outside its directory");
        for secret in ["identity", "owner-token"] {
            assert_eq!(std::fs::read_to_string(state.join(secret)).unwrap(), "x\n");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Allowing a path above the state directory allows only what's beside
    /// the way down to it.
    #[test]
    fn except_keeps_out() {
        let dir = std::env::temp_dir().join(format!("neochumby-except-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("srv/state/apps")).unwrap();
        std::fs::create_dir_all(dir.join("srv/tree")).unwrap();
        std::fs::create_dir_all(dir.join("other")).unwrap();
        let state = dir.join("srv/state");
        let a = except(&dir, &state);
        assert!(a.contains(&dir.join("srv/tree")) && a.contains(&dir.join("other")));
        assert!(!a.iter().any(|p| state.starts_with(p) || p.starts_with(&state)), "{a:?}");
        assert_eq!(except(&dir.join("other"), &state), [dir.join("other")]);
        for p in SYSTEM {
            assert!(!except(Path::new(p), &state).iter().any(|q| state.starts_with(q)));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
