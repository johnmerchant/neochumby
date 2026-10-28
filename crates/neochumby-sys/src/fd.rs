//! File descriptors: one handed to this process by number, close-on-exec,
//! and reads from a socket that don't wait.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

/// Marks `fd` close-on-exec, or not. The Chumby's 2.6.16 kernel ignores
/// O_CLOEXEC and SOCK_CLOEXEC (musl falls back to this), so the session
/// says it outright for what it keeps.
pub fn set_cloexec(fd: BorrowedFd<'_>, on: bool) -> io::Result<()> {
    let flags = if on { libc::FD_CLOEXEC } else { 0 };
    // SAFETY: F_SETFD takes an int and touches no memory; the descriptor
    // is open while it's borrowed.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A copy of the descriptor numbered `fd` that this process was handed
/// (in an environment variable, say), owned by the caller, or None if no
/// descriptor has that number. Give it only a number this process was told
/// about (NEOCHUMBY_SESSION, NEOCHUMBY_BUTTON): it copies whatever is open
/// under that number, so the caller checks it's what was meant (a socket,
/// say). Both are marked close-on-exec, so neither
/// passes on to programs this one runs. Taking a copy means that taking it
/// twice doesn't close it twice.
pub fn handed_over(fd: RawFd) -> Option<OwnedFd> {
    // SAFETY: dup(2) takes an int and touches no memory; for a number that
    // isn't an open descriptor it fails with EBADF.
    let copy = unsafe { libc::dup(fd) };
    if copy < 0 {
        return None;
    }
    // SAFETY: dup returned a new descriptor, which nothing else owns.
    let copy = unsafe { OwnedFd::from_raw_fd(copy) };
    // SAFETY: as in set_cloexec; `fd` was open a moment ago, and if it
    // isn't now this fails with EBADF.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    let _ = set_cloexec(copy.as_fd(), true);
    Some(copy)
}

/// Reads what has arrived on the socket `sock` into `buf`, without waiting
/// (recv(2) with MSG_DONTWAIT): WouldBlock if nothing has, Ok(0) once the
/// other end has closed.
pub fn recv_nowait(sock: BorrowedFd<'_>, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: the kernel writes at most `buf.len()` bytes into `buf`, a
    // live, writable slice; the descriptor is open while it's borrowed.
    let n = unsafe { libc::recv(sock.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn handed_over_and_read() {
        let (mut a, b) = UnixStream::pair().unwrap();
        let copy = handed_over(b.as_raw_fd()).expect("a copy");
        assert_ne!(copy.as_raw_fd(), b.as_raw_fd());
        let mut buf = [0u8; 8];
        assert_eq!(recv_nowait(copy.as_fd(), &mut buf).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        a.write_all(b"hi").unwrap();
        assert_eq!(recv_nowait(copy.as_fd(), &mut buf).unwrap(), 2);
        // The other end done writing (shutdown, not drop: on macOS a child
        // another test forks can inherit a copy of `a` before it's marked
        // close-on-exec).
        a.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(recv_nowait(b.as_fd(), &mut buf).unwrap(), 0);
        assert!(handed_over(-1).is_none());
        set_cloexec(copy.as_fd(), false).unwrap();
        set_cloexec(copy.as_fd(), true).unwrap();
    }
}
