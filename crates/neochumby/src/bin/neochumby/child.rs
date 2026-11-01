//! Starting what the session runs, and ending it.
//!
//! Everything the session runs gets NEOCHUMBY_ROOT (the tree), HOME=/tmp
//! (the root filesystem is read-only, and so is the tree), its own
//! request pipe as NEOCHUMBY_SESSION (neochumby::session) and, if its app
//! has a companion, a socket to it as NEOCHUMBY_COMPANION
//! (neochumby::companion). It also gets the Chumby's name
//! (NEOCHUMBY_NAME), the stick's free space as it starts
//! (NEOCHUMBY_STICK_FREE_KIB, without a stick unset), with `storage` its
//! directory (NEOCHUMBY_STORAGE, storage.rs) and, when this Chumby
//! has one, its default player (NEOCHUMBY_PLAYER, player.rs). An app
//! with an approved frontend, and that frontend, also get what lets them
//! reach each other (frontend.rs). It runs in a
//! process group of its own, so that it and whatever it started end
//! together. An app runs jailed, as jail.rs describes, unless its manifest
//! says `access = ["root"]`; the session's own services run as root. A
//! text-mode app's exec gets its terminal (terminal.rs) as its stdin and
//! stdout; its stderr, like everything else's, is the session's log.

use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use neochumby::apps::{App, WASM_ENV, WASM_TRUSTED_ENV};
use neochumby::service;
use neochumby::session::{self, Pipe};
use neochumby_sys::jail::Process;
use neochumby_sys::process::{Signal, exited, group_alive, kill_group, pass_fds};

use crate::companion;
use crate::frontend;
use crate::jail::{self, Role};

/// How long what's left of a process group has, after SIGTERM, once its
/// leader has gone.
const LINGER: Duration = Duration::from_millis(200);
/// Where the boot mounts a stick.
const STICK: &str = "/mnt/usb";

/// The free space on the stick, in KiB; None if no stick is mounted there
/// (the directory is then on the same filesystem as its parent).
fn stick_free_kib() -> Option<u64> {
    let (stick, parent) = (Path::new(STICK).metadata().ok()?, Path::new(STICK).parent()?.metadata().ok()?);
    if stick.dev() == parent.dev() {
        return None;
    }
    neochumby_sys::fs::available_bytes(Path::new(STICK)).ok().map(|b| b / 1024)
}

/// A program the session is about to run: `exe`, of `app`, as `role`.
pub struct Launch<'a> {
    app: &'a App,
    exe: &'a Path,
    role: Role,
    root: &'a Path,
    env: Vec<(String, String)>,
    keep: Vec<RawFd>,
    storage: Option<PathBuf>,
    player: Option<&'a str>,
    terminal: Option<File>,
}

/// `exe` of `app`, as `role`, from the tree at `root`.
pub fn launch<'a>(app: &'a App, exe: &'a Path, role: Role, root: &'a Path) -> Launch<'a> {
    Launch { app, exe, role, root, env: Vec::new(), keep: Vec::new(), storage: None, player: None, terminal: None }
}

impl<'a> Launch<'a> {
    /// Adds `key`=`value` to its environment.
    pub fn env(&mut self, key: &str, value: impl Into<String>) -> &mut Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Gives it `dir`, its directory (storage.rs): bound into its
    /// jail at the same path, and named by NEOCHUMBY_STORAGE.
    pub fn storage(&mut self, dir: PathBuf) -> &mut Self {
        self.storage = Some(dir);
        self
    }

    /// Names `player` the default player (NEOCHUMBY_PLAYER); without
    /// this, the variable is unset, whatever the session was started with.
    pub fn player(&mut self, player: Option<&'a str>) -> &mut Self {
        self.player = player;
        self
    }

    /// Passes it the descriptor `fd`; nothing else the session holds is.
    pub fn keep(&mut self, fd: RawFd) -> &mut Self {
        self.keep.push(fd);
        self
    }

    /// Gives it `slave`, a terminal's far end, as its stdin and stdout.
    /// The session's copy closes once it has started, so the terminal
    /// closes when the app's do.
    pub fn terminal(&mut self, slave: File) -> &mut Self {
        self.terminal = Some(slave);
        self
    }

    /// Starts it in a process group of its own, with a new request pipe.
    pub fn spawn(mut self) -> std::io::Result<(Process, Pipe)> {
        let (pipe, theirs) = Pipe::pair()?;
        self.keep.push(theirs.as_raw_fd());
        self.env.push((session::ENV.into(), theirs.as_raw_fd().to_string()));
        self.env.push((session::NAME_ENV.into(), neochumby::conf::name()));
        if let Some(kib) = stick_free_kib() {
            self.env.push((session::STICK_FREE_ENV.into(), kib.to_string()));
        }
        if let Some(s) = self.storage.as_ref().and_then(|s| s.to_str()) {
            self.env.push((session::STORAGE_ENV.into(), s.into()));
        }
        if let Some(m) = self.player {
            self.env.push((session::PLAYER_ENV.into(), m.into()));
        }
        // A wasm app's front is the runner, told its module, and whether
        // the owner trusts it (neochumby_wasm).
        if let (Some(w), Role::Front) = (&self.app.wasm, self.role) {
            self.env.push((WASM_ENV.into(), w.to_string_lossy().into_owned()));
            if self.app.trusted() {
                self.env.push((WASM_TRUSTED_ENV.into(), "1".into()));
            }
        }
        let link = if self.app.companion { Some(companion::relay(&self.app.name)?) } else { None };
        if let Some(l) = &link {
            self.keep.push(l.as_raw_fd());
            self.env.push((neochumby::companion::ENV.into(), l.as_raw_fd().to_string()));
        }
        self.env.extend(frontend::env(&self.app.name));
        // Its service's socket, which the session bound (frontend.rs).
        if self.role == Role::Service
            && let Some(l) = frontend::listener(&self.app.name)
        {
            self.keep.push(l);
            self.env.push((service::LISTENER_ENV.into(), l.to_string()));
        }
        let terminal = self.terminal.as_ref().map(|t| t.as_raw_fd());
        let process = if jail::jailed(self.app) {
            let given = jail::Given { storage: self.storage.as_deref(), terminal };
            jail::start(self.app, self.exe, self.role, self.root, self.env, &self.keep, given)?
        } else {
            let mut cmd = Command::new(self.exe);
            cmd.current_dir(&self.app.dir).env("NEOCHUMBY_ROOT", self.root).env("HOME", "/tmp");
            cmd.env_remove(session::PLAYER_ENV).env_remove(WASM_ENV).env_remove(WASM_TRUSTED_ENV);
            cmd.env_remove(service::CLIENTS_ENV).env_remove(service::FRONTEND_ENV).env_remove(service::LISTENER_ENV);
            cmd.envs(self.env).process_group(0);
            if let Some(t) = &self.terminal {
                cmd.stdin(t.try_clone()?).stdout(t.try_clone()?);
            }
            pass_fds(&mut cmd, self.keep);
            Process::from(cmd.spawn()?)
        };
        // The child has its own copies now.
        drop((theirs, link, self.terminal));
        Ok((process, pipe))
    }
}

/// Ends `child` and the rest of its process group, and reaps `child`;
/// its exit. It works whether or not `child` has exited already, so no
/// helper an app started keeps the screen, the speaker or the button into
/// the next app.
///
/// The group gets SIGTERM, and SIGKILL if `child` hasn't exited after
/// `patience`, while `child` is still unreaped: its pid holds the group's
/// id, so no other process can have it. Once `child` is reaped, anything
/// left in the group still holds the id; it gets [`LINGER`], then SIGKILL.
pub fn end(child: &mut Process, patience: Duration) -> std::io::Result<ExitStatus> {
    let group = child.id();
    let _ = kill_group(group, Signal::Term);
    let gone = || exited(group).unwrap_or(true);
    let deadline = Instant::now() + patience;
    while !gone() && Instant::now() < deadline {
        neochumby::sleep_ms(20);
    }
    if !gone() {
        let _ = kill_group(group, Signal::Kill);
    }
    let status = child.wait();
    let deadline = Instant::now() + LINGER;
    while group_alive(group) && Instant::now() < deadline {
        neochumby::sleep_ms(20);
    }
    if group_alive(group) {
        let _ = kill_group(group, Signal::Kill);
    }
    status
}
