//! Starting what the session runs, and ending it.
//!
//! Everything the session runs gets NEOCHUMBY_ROOT (the tree), HOME=/tmp
//! (the root filesystem is read-only, and so is the tree) and its own
//! request pipe as NEOCHUMBY_SESSION (neochumby::session). It runs in a
//! process group of its own, so that it and whatever it started end
//! together.

use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use neochumby::session::{self, Pipe};
use neochumby_sys::process::{Signal, group_alive, kill_group, pass_fds};

/// How long what's left of a process group has, after SIGTERM, once its
/// leader has gone.
const LINGER: Duration = Duration::from_millis(200);

/// A program the session runs, with its working directory, from the tree
/// at `root`.
pub fn command(exe: &Path, dir: &Path, root: &Path) -> Command {
    let mut c = Command::new(exe);
    c.current_dir(dir).env("NEOCHUMBY_ROOT", root).env("HOME", "/tmp");
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

/// Ends `child` and the rest of its process group, whether or not `child`
/// has exited already: SIGTERM to the group; SIGKILL if `child` hasn't
/// exited after `patience`, or if anything else in the group is left
/// [`LINGER`] after that. So no helper an app started keeps the screen,
/// the speaker or the button into the next app.
pub fn end(child: &mut Child, patience: Duration) {
    let group = child.id();
    if kill_group(group, Signal::Term).is_err() {
        // Nothing left in the group.
        let _ = child.wait();
        return;
    }
    let deadline = Instant::now() + patience;
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        neochumby::sleep_ms(20);
    }
    let deadline = Instant::now() + LINGER;
    while group_alive(group) && matches!(child.try_wait(), Ok(Some(_))) && Instant::now() < deadline {
        neochumby::sleep_ms(20);
    }
    if group_alive(group) {
        let _ = kill_group(group, Signal::Kill);
    }
    let _ = child.wait();
}
