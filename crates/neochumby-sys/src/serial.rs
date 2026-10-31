//! Serial lines, and waiting on several descriptors at once.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;

use crate::fd;

/// Opens the serial device at `path` as a raw byte stream: no echo, no
/// line editing, no characters taken for signals or flow control, no CR
/// or NL translated, eight bits, the modem lines ignored. Not this
/// process's controlling terminal, and closed on exec.
pub fn open_raw(path: &str) -> io::Result<File> {
    let f = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOCTTY).open(path)?;
    fd::set_cloexec(f.as_fd(), true)?;
    // SAFETY: termios is plain integers and arrays, for which all zeroes
    // is a value; tcgetattr then fills it in.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: the descriptor is open while `f` lives; tcgetattr writes
    // one termios into `t`.
    if unsafe { libc::tcgetattr(f.as_raw_fd(), &mut t) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: cfmakeraw only changes the fields of the termios it is given.
    unsafe { libc::cfmakeraw(&mut t) };
    t.c_cflag |= libc::CLOCAL | libc::CREAD;
    t.c_cflag &= !libc::CRTSCTS;
    t.c_cc[libc::VMIN] = 1;
    t.c_cc[libc::VTIME] = 0;
    // SAFETY: as for tcgetattr; tcsetattr reads the termios.
    if unsafe { libc::tcsetattr(f.as_raw_fd(), libc::TCSANOW, &t) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(f)
}

/// What [`poll`] is to wait for on one descriptor.
pub struct Want<'a> {
    pub fd: BorrowedFd<'a>,
    pub read: bool,
    pub write: bool,
}

/// What [`poll`] found on one descriptor. `hup` and `error` count as
/// readable: a read then says what happened.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ready {
    pub read: bool,
    pub write: bool,
}

/// Waits until one of `fds` is ready as asked, or `timeout_ms` passes
/// (negative: for ever): poll(2), which any kernel has, where std's
/// blocking waits need futex flags newer than 2.6.16's. One [`Ready`] for
/// each of `fds`, all false if the time ran out or a signal came.
pub fn poll(fds: &[Want], timeout_ms: i32) -> io::Result<Vec<Ready>> {
    let mut p: Vec<libc::pollfd> = fds
        .iter()
        .map(|w| libc::pollfd {
            fd: w.fd.as_raw_fd(),
            events: (if w.read { libc::POLLIN } else { 0 }) | (if w.write { libc::POLLOUT } else { 0 }),
            revents: 0,
        })
        .collect();
    // SAFETY: `p` is a live array of `p.len()` pollfds, of descriptors
    // that are open while `fds` borrows them; poll writes only revents.
    let n = unsafe { libc::poll(p.as_mut_ptr(), p.len() as libc::nfds_t, timeout_ms) };
    if n < 0 {
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
        p.iter_mut().for_each(|p| p.revents = 0);
    }
    Ok(p.iter()
        .map(|p| Ready {
            read: p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0,
            write: p.revents & (libc::POLLOUT | libc::POLLERR) != 0,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    #[test]
    fn poll_sees_what_is_ready() {
        let (mut a, b) = UnixStream::pair().unwrap();
        let r = poll(&[Want { fd: b.as_fd(), read: true, write: false }], 0).unwrap();
        assert!(!r[0].read);
        a.write_all(b"x").unwrap();
        let r = poll(&[Want { fd: b.as_fd(), read: true, write: true }], 1000).unwrap();
        assert!(r[0].read && r[0].write);
    }
}
