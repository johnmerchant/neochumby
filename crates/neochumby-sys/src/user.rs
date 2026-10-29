//! Who this process runs as.

use std::io;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ours() {
        assert_eq!(effective().uid, crate::process::uid());
        groups().unwrap();
    }
}
