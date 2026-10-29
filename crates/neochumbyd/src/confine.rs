//! How neochumbyd runs apps' programs on the server, their server programs
//! (server.rs) and companions (companion.rs): sandboxed, so that under the
//! state directory each may read only the tree and read and write only its
//! app's own directory (neochumby_sys::sandbox). Not `identity`, the
//! pairings or `owner-token`.
//!
//! Where there's no sandbox (Linux without Landlock, the BSDs), neochumbyd
//! runs none of them, and says why, unless its owner starts it with
//! `--unsandboxed`. Then they run with all of neochumbyd's access, and it
//! says so at every start.
//!
//! Sandboxed or not, each also runs on its own ([`prepare`]): in a session
//! of its own with no terminal, with only the descriptors it's given, its
//! stderr (and a server program's stdout) through neochumbyd's log, and
//! only the environment listed in [`PASSED`], plus HOME and TMPDIR in its
//! app's own directory.

use std::io::{self, BufRead, BufReader, Read};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::process::Command;

use neochumby_sys::process;
use neochumby_sys::sandbox::{self, Confinement, Sandbox};

/// What a program gets of neochumbyd's environment: these, any `LC_`
/// variable, and any `NEOCHUMBY_` one (what the host sets for apps, such
/// as AirPlay's port range). Nothing else, so a token in the shell that
/// started neochumbyd stays there.
pub const PASSED: &[&str] = &["PATH", "LANG", "LANGUAGE", "TZ", "USER", "LOGNAME"];

/// What neochumbyd can do on this system to confine apps' programs, and
/// what its owner said.
#[derive(Clone, Debug)]
pub enum Confine {
    Sandboxed(Sandbox),
    /// No sandbox here, and the owner passed `--unsandboxed`.
    Unsandboxed,
    /// Nothing runs, and why not.
    Refused(String),
}

/// How one app's programs run: the decision [`Confine::plan`] makes for
/// each app.
#[derive(Clone, Debug)]
pub enum Plan {
    Sandboxed(Sandbox),
    Unsandboxed,
}

impl std::fmt::Display for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Plan::Sandboxed(s) => write!(f, "sandboxed ({})", s.describe()),
            Plan::Unsandboxed => write!(f, "UNSANDBOXED (--unsandboxed)"),
        }
    }
}

impl Confine {
    /// What this system offers, and what the owner said; logged. A sandbox
    /// is always used where there is one. Where there isn't, nothing runs
    /// unless the owner passes `--unsandboxed`, and not even then as root.
    pub fn choose(unsandboxed: bool) -> Confine {
        let root = neochumby_sys::user::effective().uid == 0;
        let c = match sandbox::detect() {
            Ok(s) => {
                if unsandboxed {
                    eprintln!("neochumbyd: --unsandboxed does nothing here: there's a sandbox");
                }
                Confine::Sandboxed(s)
            }
            Err(why) if unsandboxed && root => {
                Confine::Refused(format!("{why}, and neochumbyd runs as root, so not even --unsandboxed"))
            }
            Err(_) if unsandboxed => Confine::Unsandboxed,
            Err(why) => Confine::Refused(format!("{why}; `neochumbyd serve --unsandboxed` runs them without one")),
        };
        match &c {
            Confine::Sandboxed(s) => {
                eprintln!("neochumbyd: apps' server programs and companions run sandboxed ({})", s.describe());
                eprintln!("neochumbyd: the sandbox doesn't confine: {}", s.gaps().join("; "));
            }
            Confine::Unsandboxed => eprintln!(
                "neochumbyd: --unsandboxed: apps' server programs and companions run UNSANDBOXED, able to read neochumbyd's key, pairings and owner token"
            ),
            Confine::Refused(why) => {
                eprintln!("neochumbyd: apps' server programs and companions won't run: {why}")
            }
        }
        c
    }

    /// How `app`'s programs run, or why they don't: one decision per app,
    /// where a per-app sandbox (a uid of its own) would join this one.
    pub fn plan(&self, _app: &str) -> Result<Plan, String> {
        match self {
            Confine::Sandboxed(s) => Ok(Plan::Sandboxed(*s)),
            Confine::Unsandboxed => Ok(Plan::Unsandboxed),
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
        match self {
            Plan::Sandboxed(s) => {
                // neochumbyd's user's home, closed to it on macOS.
                let home = std::env::var_os("HOME").map(PathBuf::from);
                s.command(program, &Confinement { state, tree, own, home: home.as_deref(), bind })
            }
            Plan::Unsandboxed => Ok(Command::new(program)),
        }
    }
}

/// Makes `cmd` run on its own, in `own`, its app's directory: HOME and
/// TMPDIR there (`own/tmp`), the environment [`PASSED`] and nothing else,
/// a session of its own, and `keep`, if given, as its descriptor 3 and no
/// other. Set what it needs of its own environment after this. Returns
/// `own` as the program sees it, with no symbolic links on the way (which
/// it may not be able to follow from inside the sandbox).
pub fn prepare(cmd: &mut Command, own: &Path, keep: Option<RawFd>) -> io::Result<PathBuf> {
    let own = own.canonicalize()?;
    let tmp = own.join("tmp");
    std::fs::create_dir_all(&tmp)?;
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

/// Copies what `from` writes to neochumbyd's log, a line at a time, each
/// line headed with `who`, until it closes.
pub fn log(who: String, from: impl Read + Send + 'static) {
    std::thread::spawn(move || {
        for line in BufReader::new(from).split(b'\n').map_while(Result::ok) {
            eprintln!("neochumbyd: {who}: {}", String::from_utf8_lossy(&line));
        }
    });
}
