//! Starting what the session runs, and ending it.
//!
//! Everything the session runs gets NEOCHUMBY_ROOT (the tree), HOME=/tmp
//! (the root filesystem is read-only, and so is the tree), its own
//! request pipe as NEOCHUMBY_SESSION (neochumby::session) and, when this
//! Chumby has one, its default music app as NEOCHUMBY_MUSIC (music.rs). It
//! runs in a
//! process group of its own, so that it and whatever it started end
//! together.

use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use neochumby::session::{self, Pipe};
use neochumby_sys::process::{Signal, exited, group_alive, kill_group, pass_fds};

/// How long what's left of a process group has, after SIGTERM, once its
/// leader has gone.
const LINGER: Duration = Duration::from_millis(200);

/// A program the session runs, with its working directory, from the tree
/// at `root`; `music` is the default music app, if there is one.
pub fn command(exe: &Path, dir: &Path, root: &Path, music: Option<&str>) -> Command {
    let mut c = Command::new(exe);
    c.current_dir(dir).env("NEOCHUMBY_ROOT", root).env("HOME", "/tmp");
    // Not inherited from whoever started the session: only the one chosen.
    c.env_remove(session::MUSIC_ENV);
    if let Some(m) = music {
        c.env(session::MUSIC_ENV, m);
    }
    c
}

/// Starts `cmd` in a process group of its own, with a new request pipe.
/// The descriptors in `inherit` are passed on too; nothing else the
/// session holds is.
pub fn spawn(mut cmd: Command, inherit: &[RawFd]) -> std::io::Result<(Child, Pipe)> {
    let (pipe, theirs) = Pipe::pair()?;
    let mut keep = inherit.to_vec();
    keep.push(theirs.as_raw_fd());
    cmd.env(session::ENV, theirs.as_raw_fd().to_string()).process_group(0);
    pass_fds(&mut cmd, keep);
    let child = cmd.spawn()?;
    // The child has its own copy now.
    drop(theirs);
    Ok((child, pipe))
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
pub fn end(child: &mut Child, patience: Duration) -> std::io::Result<ExitStatus> {
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
