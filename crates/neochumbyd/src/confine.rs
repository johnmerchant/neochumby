//! How neochumbyd runs apps' programs on the server, their server programs
//! (server.rs) and companions (companion.rs): sandboxed, so that under the
//! state directory each may read only the tree and read and write only its
//! app's own directory. Not `identity`, the pairings or `owner-token`. Two
//! sandboxes do that, and either is enough ([`isolate::plan`]):
//!
//! - this system's own (neochumby_sys::sandbox): Landlock on Linux, Apple's
//!   on macOS, with neochumbyd running as its own user;
//! - a uid of each app's own (`--app-uids`, isolate.rs), with neochumbyd
//!   started as root: for a kernel without Landlock, say. Where both can
//!   be had, both apply, and the uid closes what Landlock leaves open.
//!
//! Where there's neither, neochumbyd runs none of them, and says why,
//! unless its owner starts it with `--unsandboxed`. Then they run with all
//! of neochumbyd's access, and it says so at every start. Never as root.
//!
//! Sandboxed or not, each also runs on its own ([`Plan::prepare`]): in a
//! session of its own with no terminal, with only the descriptors it's
//! given, its stderr (and a server program's stdout) through neochumbyd's
//! log, and only the environment listed in [`PASSED`], plus HOME and
//! TMPDIR in its app's own directory.

use std::io::{self, BufRead, BufReader, Read};
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use neochumby_sys::process;
use neochumby_sys::sandbox::{self, Confinement, Sandbox};

use crate::isolate::{self, Uids};

/// What a program gets of neochumbyd's environment: these, any `LC_`
/// variable, and any `NEOCHUMBY_` one (what the host sets for apps, such
/// as AirPlay's port range). Nothing else, so a token in the shell that
/// started neochumbyd stays there.
pub const PASSED: &[&str] = &["PATH", "LANG", "LANGUAGE", "TZ", "USER", "LOGNAME"];

/// What neochumbyd can do on this system to confine apps' programs, and
/// what its owner said.
#[derive(Clone, Debug)]
pub enum Confine {
    /// This system's sandbox, a uid of each app's own, or both: at least
    /// one of them.
    Sandboxed { sandbox: Option<Sandbox>, uids: Option<Arc<Uids>> },
    /// No sandbox here, and the owner passed `--unsandboxed`.
    Unsandboxed,
    /// Nothing runs, and why not.
    Refused(String),
}

/// How one app's programs run: the decision [`Confine::plan`] makes for
/// each app. Neither a sandbox nor a uid is unsandboxed.
#[derive(Clone, Debug)]
pub struct Plan {
    sandbox: Option<Sandbox>,
    uid: Option<u32>,
}

impl std::fmt::Display for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match (self.uid, self.sandbox) {
            (Some(uid), Some(s)) => write!(f, "sandboxed (uid {uid}, {})", s.describe()),
            (Some(uid), None) => write!(f, "sandboxed (uid {uid})"),
            (None, Some(s)) => write!(f, "sandboxed ({})", s.describe()),
            (None, None) => write!(f, "UNSANDBOXED (--unsandboxed)"),
        }
    }
}

impl Confine {
    /// What this system offers, and what the owner said (`--app-uids`,
    /// `--unsandboxed`); logged. `serve` calls it before it starts a thread
    /// or writes a file, as with `--app-uids` neochumbyd started as root
    /// becomes its own user here. A sandbox is always used where there is
    /// one. With neither it nor uids, nothing runs unless the owner passes
    /// `--unsandboxed`, and not even then as root. An error is
    /// `--app-uids` asked for and set up wrong, and `serve` stops.
    pub fn choose(state: &Path, app_uids: Option<&str>, unsandboxed: bool) -> Result<Confine, String> {
        let root = neochumby_sys::user::effective().uid == 0;
        let range = app_uids.map(isolate::parse_range).transpose()?;
        let detected = sandbox::detect();
        let linux = cfg!(target_os = "linux");
        let c = match (isolate::plan(root, linux, range.is_some(), detected.is_ok(), unsandboxed), range) {
            (Ok(isolate::Plan::Uids), Some(range)) => {
                Confine::Sandboxed { sandbox: detected.ok(), uids: Some(Arc::new(Uids::start(state, range)?)) }
            }
            (Ok(isolate::Plan::Sandbox), _) => {
                if unsandboxed {
                    eprintln!("neochumbyd: --unsandboxed does nothing here: there's a sandbox");
                }
                if range.is_some() {
                    let why = if root { "not on Linux" } else { "neochumbyd isn't root" };
                    eprintln!("neochumbyd: --app-uids does nothing here ({why}): this system's sandbox alone");
                }
                Confine::Sandboxed { sandbox: detected.ok(), uids: None }
            }
            (Ok(_), _) => Confine::Unsandboxed,
            (Err(how), _) => Confine::Refused(match detected {
                Err(why) => format!("{why}; {how}"),
                Ok(_) => how,
            }),
        };
        match &c {
            Confine::Sandboxed { sandbox, uids } => {
                let (what, gaps) = match (sandbox, uids) {
                    (Some(s), Some(u)) => (
                        format!("a uid of each app's own ({}-{}), and {}", u.range.0, u.range.1, s.describe()),
                        s.gaps(true),
                    ),
                    (Some(s), None) => (s.describe(), s.gaps(false)),
                    (None, Some(u)) => (
                        format!("a uid of each app's own ({}-{}), without Landlock", u.range.0, u.range.1),
                        vec!["reading world-readable files, the tree included", "the network", "CPU and memory"],
                    ),
                    (None, None) => unreachable!("Sandboxed has one or both"),
                };
                eprintln!("neochumbyd: apps' server programs and companions run sandboxed ({what})");
                eprintln!("neochumbyd: the sandbox doesn't confine: {}", gaps.join("; "));
            }
            Confine::Unsandboxed => eprintln!(
                "neochumbyd: --unsandboxed: apps' server programs and companions run UNSANDBOXED, able to read neochumbyd's key, pairings and owner token"
            ),
            Confine::Refused(why) => {
                eprintln!("neochumbyd: apps' server programs and companions won't run: {why}")
            }
        }
        Ok(c)
    }

    /// This system's sandbox alone, or unsandboxed where it has none: for
    /// tests that run apps' programs as neochumbyd's own user.
    #[cfg(test)]
    pub fn for_tests() -> Confine {
        match sandbox::detect() {
            Ok(s) => Confine::Sandboxed { sandbox: Some(s), uids: None },
            Err(_) => Confine::Unsandboxed,
        }
    }

    /// How `app`'s programs run, or why they don't: one decision per app.
    /// With `--app-uids`, this gives the app its uid and makes its
    /// directory ready for it (isolate::Uids::ready).
    pub fn plan(&self, app: &str) -> Result<Plan, String> {
        match self {
            Confine::Sandboxed { sandbox, uids } => {
                let uid = uids.as_ref().map(|u| u.ready(app)).transpose().map_err(|e| format!("its uid: {e}"))?;
                Ok(Plan { sandbox: *sandbox, uid })
            }
            Confine::Unsandboxed => Ok(Plan { sandbox: None, uid: None }),
            Confine::Refused(why) => Err(why.clone()),
        }
    }
}

impl Plan {
    /// A Command for `program`, of the app whose directory is `own`.
    /// `bind`: the TCP ports it may bind, where the sandbox can say (None:
    /// as it likes).
    pub fn command(
        &self,
        program: &Path,
        state: &Path,
        tree: &Path,
        own: &Path,
        bind: Option<&[u16]>,
    ) -> io::Result<Command> {
        let mut cmd = match self.sandbox {
            Some(s) => {
                // neochumbyd's user's home, closed to it on macOS.
                let home = std::env::var_os("HOME").map(PathBuf::from);
                s.command(program, &Confinement { state, tree, own, home: home.as_deref(), bind })?
            }
            None => Command::new(program),
        };
        // Its uid and group, and no other groups (std drops them, as
        // neochumbyd keeps CAP_SETGID), before it enters its directory,
        // which only that uid can write (isolate::Uids::ready).
        if let Some(uid) = self.uid {
            cmd.uid(uid).gid(uid);
        }
        Ok(cmd)
    }

    /// Makes `cmd` run on its own, in `own`, its app's directory: HOME and
    /// TMPDIR there (`own/tmp`), the environment [`PASSED`] and nothing
    /// else, a session of its own, and `keep`, if given, as its descriptor
    /// 3 and no other. Set what it needs of its own environment after
    /// this. Returns `own` as the program sees it, with no symbolic links
    /// on the way (which it may not be able to follow from inside the
    /// sandbox).
    pub fn prepare(&self, cmd: &mut Command, own: &Path, keep: Option<RawFd>) -> io::Result<PathBuf> {
        let own = own.canonicalize()?;
        let tmp = own.join("tmp");
        // Under a uid of its own the directory is the app's, which
        // neochumbyd can't write, and its tmp was made with it
        // (isolate::Uids::ready).
        if self.uid.is_none() {
            std::fs::create_dir_all(&tmp)?;
        }
        cmd.env_clear();
        for (k, v) in std::env::vars_os() {
            let name = k.to_string_lossy();
            if PASSED.contains(&name.as_ref()) || name.starts_with("LC_") || name.starts_with("NEOCHUMBY_") {
                cmd.env(&k, v);
            }
        }
        cmd.env("HOME", &own).env("TMPDIR", &tmp).current_dir(&own);
        process::isolate(cmd, keep);
        Ok(own)
    }
}

/// Copies what `from` writes to neochumbyd's log, a line at a time, each
/// line headed with `who`, until it closes.
pub fn log(who: String, from: impl Read + Send + 'static) {
    std::thread::spawn(move || {
        for line in BufReader::new(from).split(b'\n').map_while(Result::ok) {
            eprintln!("neochumbyd: {who}: {}", String::from_utf8_lossy(&line));
        }
    });
}
