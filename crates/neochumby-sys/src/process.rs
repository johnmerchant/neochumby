//! Signals to other processes, by pid or by process group, and the
//! descriptors a child is given.

use std::io;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// A signal [`kill`] and [`kill_group`] can send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// SIGTERM: asks the process to stop.
    Term,
    /// SIGKILL: ends it.
    Kill,
}

impl Signal {
    fn raw(self) -> libc::c_int {
        match self {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        }
    }
}

/// Sends `sig` to the process `pid` (as from `Child::id`). A pid of 0, or
/// one too large for a pid_t, is refused without a system call: kill(2)
/// would read it as a process group or every process.
pub fn kill(pid: u32, sig: Signal) -> io::Result<()> {
    send(positive(pid)?, sig)
}

/// Sends `sig` to every process in the group `pgid` (the group a child
/// leads after `process_group(0)` has its pid as the group's id). 0 or a
/// value too large for a pid_t is refused, as for [`kill`].
pub fn kill_group(pgid: u32, sig: Signal) -> io::Result<()> {
    send(-positive(pgid)?, sig)
}

fn positive(id: u32) -> io::Result<libc::pid_t> {
    match libc::pid_t::try_from(id) {
        Ok(p) if p > 0 => Ok(p),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "not a process id")),
    }
}

fn send(target: libc::pid_t, sig: Signal) -> io::Result<()> {
    // SAFETY: kill(2) takes plain integers and touches no memory of ours;
    // `target` is a single process or a single group, never 0 or -1.
    if unsafe { libc::kill(target, sig.raw()) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Passes the descriptors `fds` on to the program `cmd` runs, though they
/// are close-on-exec here: in the child, between fork and exec, each is
/// marked not close-on-exec. Starting the program fails if one isn't open.
pub fn pass_fds(cmd: &mut Command, fds: Vec<RawFd>) {
    // SAFETY: the closure runs in the forked child, where only
    // async-signal-safe calls are allowed: it calls fcntl(2), which is,
    // reads a Vec allocated before the fork, and allocates nothing.
    unsafe {
        cmd.pre_exec(move || {
            for &fd in &fds {
                if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_what_is_not_one_process_or_group() {
        for id in [0, u32::MAX, 1 << 31] {
            assert_eq!(kill(id, Signal::Term).unwrap_err().kind(), io::ErrorKind::InvalidInput);
            assert_eq!(kill_group(id, Signal::Term).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
    }
}
