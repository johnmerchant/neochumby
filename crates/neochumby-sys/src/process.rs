//! Signals to other processes, by pid or by process group, and this
//! process's priority.

use std::io;

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

/// Lowers this process's priority by `by` (nice(2)); 19 is the lowest. A
/// process may always lower its own, so nothing is reported.
pub fn lower_priority(by: u8) {
    // SAFETY: nice(2) takes an integer and touches no memory of ours.
    unsafe { libc::nice(libc::c_int::from(by.min(19))) };
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
