//! Socket options std doesn't have.

use std::io;
use std::net::TcpStream;
use std::os::fd::{AsRawFd, BorrowedFd};

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

/// The user id of the process at the other end of the Unix socket `sock`:
/// whoever connected, for a listener's accepted stream; whoever listened,
/// for a stream that connected (SO_PEERCRED on Linux, from 2.2; getpeereid
/// elsewhere). As the kernel recorded it then, not as it is now.
pub fn peer_uid(sock: BorrowedFd<'_>) -> io::Result<u32> {
    #[cfg(target_os = "linux")]
    {
        let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: the kernel writes at most `len` bytes into `cred`, a
        // live ucred of that size, and the length back into `len`; the
        // descriptor is open while it's borrowed.
        let r = unsafe {
            libc::getsockopt(sock.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len)
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: getpeereid(3) writes one uid and one gid through the two
        // pointers, to our two live locals.
        if unsafe { libc::getpeereid(sock.as_raw_fd(), &mut uid, &mut gid) } != 0 {
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

/// A socket for interface ioctls, closed when dropped.
#[cfg(target_os = "linux")]
struct IfSock(std::os::fd::OwnedFd);

#[cfg(target_os = "linux")]
impl IfSock {
    fn new() -> Option<IfSock> {
        use std::os::fd::FromRawFd;
        // SAFETY: socket(2) takes integers and returns a new descriptor.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        // SAFETY: a descriptor socket(2) just returned, owned by nothing else.
        (fd >= 0).then(|| IfSock(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }))
    }
}

/// An interface name as the ioctls take it: NUL-padded, under 16 bytes.
#[cfg(target_os = "linux")]
fn ifname(iface: &str) -> Option<[u8; 16]> {
    let mut n = [0u8; 16];
    (iface.len() < 16 && !iface.as_bytes().contains(&0)).then(|| {
        n[..iface.len()].copy_from_slice(iface.as_bytes());
        n
    })
}

/// The IPv4 address of an interface (SIOCGIFADDR); None if it has none.
#[cfg(target_os = "linux")]
pub fn ipv4_address(iface: &str) -> Option<[u8; 4]> {
    let s = IfSock::new()?;
    // struct ifreq: 16 bytes of name, then a sockaddr_in (family, port,
    // address). 40 bytes is more than any Linux target's ifreq.
    let mut req = [0u8; 40];
    req[..16].copy_from_slice(&ifname(iface)?);
    // SAFETY: the kernel reads the name and writes a sockaddr into `req`,
    // which is larger than the struct ifreq it expects.
    if unsafe { libc::ioctl(s.0.as_raw_fd(), libc::SIOCGIFADDR as libc::Ioctl, req.as_mut_ptr()) } < 0 {
        return None;
    }
    Some([req[20], req[21], req[22], req[23]])
}

/// A wireless interface's ESSID (SIOCGIWESSID, wireless extensions); None
/// if it isn't wireless or has none.
#[cfg(target_os = "linux")]
pub fn wireless_essid(iface: &str) -> Option<Vec<u8>> {
    const SIOCGIWESSID: u32 = 0x8B1B;
    /// struct iwreq with its union as struct iw_point, padded. The kernel
    /// reads the fields it isn't given back.
    #[repr(C)]
    #[allow(dead_code)]
    struct IwReq {
        name: [u8; 16],
        pointer: *mut u8,
        length: u16,
        flags: u16,
        pad: [u8; 16],
    }
    let s = IfSock::new()?;
    let mut buf = [0u8; 34];
    let mut req = IwReq { name: ifname(iface)?, pointer: buf.as_mut_ptr(), length: 33, flags: 0, pad: [0; 16] };
    // SAFETY: `req` is laid out as struct iwreq (and larger), and its
    // pointer is to `buf`, live and `length` + 1 bytes long; the kernel
    // writes at most `length` bytes there.
    if unsafe { libc::ioctl(s.0.as_raw_fd(), SIOCGIWESSID as libc::Ioctl, &raw mut req) } < 0 {
        return None;
    }
    let n = (req.length as usize).min(32);
    let e: Vec<u8> = buf[..n].iter().copied().take_while(|&b| b != 0).collect();
    (!e.is_empty()).then_some(e)
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn a_peer_is_this_user() {
        let (a, b) = UnixStream::pair().unwrap();
        assert_eq!(super::peer_uid(a.as_fd()).unwrap(), crate::process::uid());
        assert_eq!(super::peer_uid(b.as_fd()).unwrap(), crate::process::uid());
    }
}
