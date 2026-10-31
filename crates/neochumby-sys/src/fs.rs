//! Files, for what std leaves out or does with newer calls: link(2), the
//! no-follow flag for opens, a lock, a whole sync, and the free space on a
//! filesystem.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// For `OpenOptionsExt::custom_flags`: the open fails if the last part of
/// the path is a symbolic link, rather than following it.
pub const NO_FOLLOW: i32 = libc::O_NOFOLLOW;

/// Flushes everything written, on every filesystem (sync(2)). On Linux it
/// returns once the writes are done. On the Chumby's 2.6.16 FAT an fsync
/// costs the same (a sync of the whole device), so a batch of writes takes
/// one of these.
pub fn sync() {
    // SAFETY: sync(2) takes nothing and touches no memory of ours.
    unsafe { libc::sync() };
}

/// A second name, `to`, for the file `from` (link(2)); fails with
/// [`io::ErrorKind::AlreadyExists`] if `to` exists. std's `hard_link`
/// calls linkat(2), which the Chumby's 2.6.16 kernel lacks on ARM.
pub fn hard_link(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (cpath(from)?, cpath(to)?);
    // SAFETY: both are NUL-terminated strings that outlive the call.
    if unsafe { libc::link(from.as_ptr(), to.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Takes an exclusive lock on `f` (flock(2)) without waiting: true if this
/// has it, false if another open of the same file holds one. It goes when
/// `f` is closed, or its process ends. A directory opened for reading can be
/// locked too. std's `File::try_lock` does this from Rust 1.89, newer than
/// the Rust Buildroot builds this crate with for ncinit (1.88).
pub fn try_lock(f: &File) -> io::Result<bool> {
    // SAFETY: flock(2) takes a descriptor, which `f` keeps open for the
    // call, and plain flags; it touches no memory of ours.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.kind() == io::ErrorKind::WouldBlock { Ok(false) } else { Err(e) }
}

fn cpath(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in a path"))
}

/// The bytes an unprivileged writer may still use on the filesystem
/// holding `path` (statvfs(3): f_bavail blocks of f_frsize bytes).
pub fn available_bytes(path: &Path) -> io::Result<u64> {
    let c = cpath(path)?;
    // SAFETY: libc::statvfs is plain integers, for which zero is valid.
    let mut v: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated string that outlives the call, and
    // `v` is a live local of the type statvfs writes.
    if unsafe { libc::statvfs(c.as_ptr(), &mut v) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // The fields are 32 bits on some systems (macOS) and 64 on others.
    #[allow(clippy::unnecessary_cast)]
    Ok((v.f_bavail as u64).saturating_mul(v.f_frsize as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_name_but_not_over_a_file() {
        let d = std::env::temp_dir().join(format!("neochumby-link-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a"), b"x").unwrap();
        hard_link(&d.join("a"), &d.join("b")).unwrap();
        assert_eq!(std::fs::read(d.join("b")).unwrap(), b"x");
        assert_eq!(hard_link(&d.join("a"), &d.join("b")).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_lock_is_one_open_file_s() {
        let d = std::env::temp_dir().join(format!("neochumby-sys-lock-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let first = File::open(&d).unwrap();
        assert!(try_lock(&first).unwrap());
        // Another open of the directory, even in this process, waits.
        let second = File::open(&d).unwrap();
        assert!(!try_lock(&second).unwrap());
        drop(first);
        // Free once every copy of the descriptor is closed: a child another
        // test forks (lockdown.rs's, which don't exec) may hold one a moment.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !try_lock(&second).unwrap() {
            assert!(std::time::Instant::now() < deadline, "still locked");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(second);
        // rmdir(2): std's remove_dir_all uses calls 2.6.16 hasn't got on ARM.
        std::fs::remove_dir(d).unwrap();
    }

    #[test]
    fn free_space() {
        assert!(available_bytes(&std::env::temp_dir()).unwrap() > 0);
        assert!(available_bytes(Path::new("/no/such/place")).is_err());
        assert!(available_bytes(Path::new("a\0b")).is_err());
    }
}
