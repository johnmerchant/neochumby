//! Filesystem facts std doesn't have.

use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The bytes free to an unprivileged user on the filesystem holding
/// `path` (statvfs(3): f_bavail blocks of f_frsize bytes).
pub fn available_bytes(path: &Path) -> io::Result<u64> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in the path"))?;
    // SAFETY: libc::statvfs is plain integers, for which zero is valid.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a live statvfs that the call fills.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // The fields' widths differ between systems.
    #[allow(clippy::unnecessary_cast)]
    Ok((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_temporary_directory_has_room() {
        assert!(super::available_bytes(&std::env::temp_dir()).unwrap() > 0);
        assert!(super::available_bytes(std::path::Path::new("/no/such/place")).is_err());
    }
}
