//! Socket options std doesn't have.

use std::net::TcpStream;
use std::os::fd::AsRawFd;

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

/// An int-valued socket option.
fn set(tcp: &TcpStream, level: libc::c_int, opt: libc::c_int, v: u32) {
    let v = v.min(libc::c_int::MAX as u32) as libc::c_int;
    let len = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: the descriptor is the stream's, open while it is borrowed;
    // the kernel reads `len` bytes from `v`, a live c_int of that size.
    unsafe { libc::setsockopt(tcp.as_raw_fd(), level, opt, (&raw const v).cast(), len) };
}
