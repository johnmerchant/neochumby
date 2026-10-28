//! Who this process runs as, and who is at the other end of a Unix socket.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

/// A user and a group, as the kernel numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ids {
    pub uid: u32,
    pub gid: u32,
}

/// This process's effective user and group.
pub fn effective() -> Ids {
    // SAFETY: geteuid(2) and getegid(2) take nothing, touch no memory and
    // cannot fail.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    Ids { uid, gid }
}

/// This process's supplementary groups, as getgroups(2) gives them.
pub fn groups() -> io::Result<Vec<u32>> {
    // SAFETY: with a size of 0 getgroups only counts the groups, and
    // writes nothing; the pointer is not read.
    let n = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut g: Vec<libc::gid_t> = vec![0; n as usize];
    // SAFETY: the kernel writes at most `g.len()` gid_ts into `g`, which
    // holds that many; it fails (EINVAL) rather than write more.
    let n = unsafe { libc::getgroups(g.len() as libc::c_int, g.as_mut_ptr()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    g.truncate(n as usize);
    Ok(g)
}

/// The user and group of the process at the other end of `s`, as the
/// kernel recorded them when it connected (SO_PEERCRED on Linux, getpeereid
/// elsewhere). The group is its effective group only, not its
/// supplementary ones.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn peer(s: &UnixStream) -> io::Result<Ids> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the descriptor is the stream's, open while it is borrowed;
    // the kernel writes at most `len` bytes into `cred`, a live ucred of
    // that size, and updates `len`.
    let r = unsafe {
        libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len)
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Ids { uid: cred.uid, gid: cred.gid })
}

/// The user and group of the process at the other end of `s`, as the
/// kernel recorded them when it connected (SO_PEERCRED on Linux, getpeereid
/// elsewhere). The group is its effective group only, not its
/// supplementary ones.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub fn peer(s: &UnixStream) -> io::Result<Ids> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: the descriptor is the stream's, open while it is borrowed;
    // the kernel writes one uid_t and one gid_t, into live locals.
    if unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Ids { uid, gid })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ours_at_both_ends() {
        let (a, b) = UnixStream::pair().unwrap();
        let me = effective();
        assert_eq!(peer(&a).unwrap(), me);
        assert_eq!(peer(&b).unwrap(), me);
        groups().unwrap();
    }
}
