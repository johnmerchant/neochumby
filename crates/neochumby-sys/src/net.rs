//! Socket options std doesn't have, and who is at the other end of a
//! Unix socket.

use std::io;
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// Turns on TCP keepalive for `tcp`. On Linux the connection is probed
/// after `idle` seconds without traffic, then every `interval` seconds, and
/// given up after `count` unanswered probes; elsewhere the system's timings
/// apply and these are unused. Best effort: an option the system refuses
/// is left as it was.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
pub fn keepalive(tcp: &TcpStream, idle: u32, interval: u32, count: u32) {
    set(tcp, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
    #[cfg(target_os = "linux")]
    {
        set(tcp, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, idle);
        set(tcp, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, interval);
        set(tcp, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, count);
    }
}

/// The user id of the process at the other end of the Unix socket `s`, as
/// the kernel recorded it when the connection was made (SO_PEERCRED on
/// Linux, which 2.6.16 has; getpeereid elsewhere). It can't be forged by
/// the peer, so a server can refuse other users' processes.
pub fn peer_uid(s: &UnixStream) -> io::Result<u32> {
    #[cfg(target_os = "linux")]
    {
        let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: the descriptor is the stream's, open while it is borrowed;
        // the kernel writes at most `len` bytes into `cred`, a live ucred of
        // that size, and sets `len`, a live socklen_t.
        let r = unsafe {
            libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len)
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: the descriptor is the stream's, open while it is borrowed;
        // getpeereid writes one uid_t and one gid_t, both live.
        if unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}

/// An int-valued socket option.
fn set(tcp: &TcpStream, level: libc::c_int, opt: libc::c_int, v: u32) {
    let v = v.min(libc::c_int::MAX as u32) as libc::c_int;
    let len = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: the descriptor is the stream's, open while it is borrowed;
    // the kernel reads `len` bytes from `v`, a live c_int of that size.
    unsafe { libc::setsockopt(tcp.as_raw_fd(), level, opt, (&raw const v).cast(), len) };
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_unix_socket_peer_is_this_user() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        assert_eq!(super::peer_uid(&a).unwrap(), crate::process::uid());
    }
}
