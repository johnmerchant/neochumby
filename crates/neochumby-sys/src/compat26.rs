//! Stand-ins for calls a C program makes that Linux 2.6.16 doesn't have on
//! ARM: `openat`, `fstatat` and `readlinkat` (ARM's table ends at 321 in
//! 2.6.16; they came in 2.6.17) and `pselect6` (2.6.19). procps' top
//! needs all four. Without them it exits with 47 at once, as it can't
//! read /proc/self.
//!
//! An app links them with `-Wl,--wrap=NAME` for each of [`WRAPPED`]
//! (apps/top/build.rs). Its C then calls `__wrap_NAME` here. Each one makes
//! the real call first, as musl's `__real_NAME`. Only where the kernel says
//! ENOSYS does it do the same with the calls 2.6.16 has:
//!
//! - a path relative to a directory descriptor becomes that descriptor's
//!   `/proc/self/fd` link, followed by the path (the app needs /proc:
//!   `system`), and is opened with `open`, read with `readlink`, or given
//!   to `fstatat` from AT_FDCWD, which musl makes with `stat64` or
//!   `lstat64`;
//! - `pselect` becomes `poll` on the same descriptors, between two
//!   changes of the signal mask.
//!
//! Where the real call works (Linux 5.9), the stand-ins do nothing more.
//!
//! The real calls are musl's own functions, not raw system calls. That
//! way the `stat` and `timespec` the C hands over stay in musl 1.2's
//! 64-bit-time layouts, which musl converts to the kernel's. musl's
//! headers send `fstatat` and `pselect` to `__fstatat_time64` and
//! `__pselect_time64`, so those are the names wrapped. The C-facing half
//! is built only for 32-bit ARM musl, the Chumby's; the rest is plain
//! Rust, tested on the host.

use std::ffi::{CStr, CString, c_int, c_ulong};
use std::io;

/// The symbols an app wraps (`-Wl,--wrap=NAME`) to use these stand-ins.
pub const WRAPPED: [&str; 4] = ["openat", "readlinkat", "__fstatat_time64", "__pselect_time64"];

/// `path` as seen from the directory `dirfd`, through its /proc/self/fd
/// link; as it is if it is absolute or `dirfd` is AT_FDCWD.
pub fn through_proc(dirfd: c_int, path: &CStr) -> Option<CString> {
    let p = path.to_bytes();
    if dirfd == libc::AT_FDCWD || p.first() == Some(&b'/') {
        return Some(path.to_owned());
    }
    let mut v = format!("/proc/self/fd/{dirfd}/").into_bytes();
    v.extend_from_slice(p);
    CString::new(v).ok()
}

/// `real(dirfd, path)`; where that fails with ENOSYS, `fallback` on the
/// path through /proc ([`through_proc`]).
pub fn retry<T>(
    dirfd: c_int,
    path: &CStr,
    real: impl FnOnce(c_int, &CStr) -> io::Result<T>,
    fallback: impl FnOnce(&CStr) -> io::Result<T>,
) -> io::Result<T> {
    match real(dirfd, path) {
        Err(e) if e.raw_os_error() == Some(libc::ENOSYS) => {
            let p = through_proc(dirfd, path).ok_or_else(|| io::Error::from_raw_os_error(libc::ENAMETOOLONG))?;
            fallback(&p)
        }
        r => r,
    }
}

/// A timeout of `sec` and `nsec` in poll's milliseconds, rounded up.
pub fn timeout_ms(sec: i64, nsec: i64) -> c_int {
    let ms = sec.saturating_mul(1000).saturating_add(nsec.saturating_add(999_999) / 1_000_000);
    ms.clamp(0, c_int::MAX as i64) as c_int
}

const BITS: usize = c_ulong::BITS as usize;

fn is_set(set: &Option<&mut [c_ulong]>, fd: usize) -> bool {
    set.as_ref().is_some_and(|s| s.get(fd / BITS).is_some_and(|w| w & (1 << (fd % BITS)) != 0))
}

fn mark(set: &mut Option<&mut [c_ulong]>, fd: usize) -> c_int {
    match set.as_mut().and_then(|s| s.get_mut(fd / BITS)) {
        Some(w) => {
            *w |= 1 << (fd % BITS);
            1
        }
        None => 0,
    }
}

/// select(2), on the first `n` descriptors of the read, write and except
/// sets (fd_set's words), by `poll` (poll(2) on the descriptors, with the
/// timeout), as select counts: a descriptor once for each set it is ready
/// in. The sets are left holding the ready descriptors.
pub fn select_by_poll(
    n: usize,
    mut sets: [Option<&mut [c_ulong]>; 3],
    poll: impl FnOnce(&mut [libc::pollfd]) -> io::Result<c_int>,
) -> io::Result<c_int> {
    let [r, w, e] = &sets;
    let mut fds: Vec<libc::pollfd> = (0..n)
        .filter_map(|fd| {
            let events = if is_set(r, fd) { libc::POLLIN } else { 0 }
                | if is_set(w, fd) { libc::POLLOUT } else { 0 }
                | if is_set(e, fd) { libc::POLLPRI } else { 0 };
            (events != 0).then_some(libc::pollfd { fd: fd as c_int, events, revents: 0 })
        })
        .collect();
    poll(&mut fds)?;
    for s in sets.iter_mut().flatten() {
        s.fill(0);
    }
    let [r, w, e] = &mut sets;
    let mut ready = 0;
    for p in &fds {
        let fd = p.fd as usize;
        if p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 && is_wanted(p, libc::POLLIN) {
            ready += mark(r, fd);
        }
        if p.revents & (libc::POLLOUT | libc::POLLERR) != 0 && is_wanted(p, libc::POLLOUT) {
            ready += mark(w, fd);
        }
        if p.revents & libc::POLLPRI != 0 && is_wanted(p, libc::POLLPRI) {
            ready += mark(e, fd);
        }
    }
    Ok(ready)
}

fn is_wanted(p: &libc::pollfd, event: libc::c_short) -> bool {
    p.events & event != 0
}

/// The C-facing half: the `__wrap_` functions, on the Chumby's 32-bit ARM
/// musl.
#[cfg(all(target_os = "linux", target_env = "musl", target_arch = "arm"))]
mod wrap {
    use std::ffi::{CStr, c_char, c_int, c_long, c_uint, c_ulong, c_void};
    use std::io;

    unsafe extern "C" {
        fn __real_openat(dirfd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
        fn __real_readlinkat(dirfd: c_int, path: *const c_char, buf: *mut c_char, size: usize) -> isize;
        fn __real___fstatat_time64(dirfd: c_int, path: *const c_char, st: *mut c_void, flag: c_int) -> c_int;
        fn __real___pselect_time64(
            n: c_int,
            r: *mut c_void,
            w: *mut c_void,
            e: *mut c_void,
            ts: *const c_void,
            mask: *const libc::sigset_t,
        ) -> c_int;
    }

    /// musl 1.2's timespec on 32-bit little-endian ARM: a 64-bit time_t,
    /// then a long and padding.
    #[repr(C)]
    struct Timespec64 {
        sec: i64,
        nsec: c_long,
        _pad: c_long,
    }

    /// The words of an fd_set.
    const WORDS: usize = libc::FD_SETSIZE / c_ulong::BITS as usize;

    fn ok(r: c_int) -> io::Result<c_int> {
        if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
    }

    fn ok_len(r: isize) -> io::Result<isize> {
        if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
    }

    /// The result as C has it: the value, or -1 with errno set.
    fn to_c<T: From<i8>>(r: io::Result<T>) -> T {
        r.unwrap_or_else(|e| {
            // SAFETY: errno is this thread's own int, which
            // __errno_location always returns.
            unsafe { *libc::__errno_location() = e.raw_os_error().unwrap_or(libc::EIO) };
            T::from(-1)
        })
    }

    /// openat(2). C calls it with a variadic mode; on ARM's soft-float
    /// calling convention a fourth integer argument comes in the same
    /// register either way, and it is only used with O_CREAT.
    ///
    /// # Safety
    /// As openat(2): `path` is a C string.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __wrap_openat(dirfd: c_int, path: *const c_char, flags: c_int, mode: c_uint) -> c_int {
        // SAFETY: path is a C string, as openat's caller gives it.
        let path = unsafe { CStr::from_ptr(path) };
        to_c(super::retry(
            dirfd,
            path,
            // SAFETY: a C string, and the caller's flags and mode.
            |d, p| ok(unsafe { __real_openat(d, p.as_ptr(), flags, mode) }),
            // SAFETY: a C string, and the caller's flags and mode.
            |p| ok(unsafe { libc::open(p.as_ptr(), flags, mode) }),
        ))
    }

    /// readlinkat(2).
    ///
    /// # Safety
    /// As readlinkat(2): `path` is a C string and `buf` holds `size` bytes.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __wrap_readlinkat(
        dirfd: c_int,
        path: *const c_char,
        buf: *mut c_char,
        size: usize,
    ) -> isize {
        // SAFETY: path is a C string, as readlinkat's caller gives it.
        let path = unsafe { CStr::from_ptr(path) };
        to_c(super::retry(
            dirfd,
            path,
            // SAFETY: a C string, and the caller's buffer of size bytes.
            |d, p| ok_len(unsafe { __real_readlinkat(d, p.as_ptr(), buf, size) }),
            // SAFETY: a C string, and the caller's buffer of size bytes.
            |p| ok_len(unsafe { libc::readlink(p.as_ptr(), buf, size) }),
        ))
    }

    /// fstatat(2), into the caller's musl `struct stat`, which is passed on
    /// untouched.
    ///
    /// # Safety
    /// As fstatat(2): `path` is a C string and `st` a musl struct stat.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __wrap___fstatat_time64(
        dirfd: c_int,
        path: *const c_char,
        st: *mut c_void,
        flag: c_int,
    ) -> c_int {
        // SAFETY: path is a C string, as fstatat's caller gives it.
        let path = unsafe { CStr::from_ptr(path) };
        to_c(super::retry(
            dirfd,
            path,
            // SAFETY: a C string, and the caller's struct stat.
            |d, p| ok(unsafe { __real___fstatat_time64(d, p.as_ptr(), st, flag) }),
            // SAFETY: as above; from AT_FDCWD musl makes stat64 or lstat64.
            |p| ok(unsafe { __real___fstatat_time64(libc::AT_FDCWD, p.as_ptr(), st, flag) }),
        ))
    }

    /// pselect(2). Where the kernel has no pselect6, poll with `mask` in
    /// place while it waits (musl's select is pselect6 too). A signal that
    /// comes between the change of mask and poll waits for the next event
    /// or the timeout, where pselect would end at once: to top, a screen
    /// refreshed a few seconds late.
    ///
    /// # Safety
    /// As pselect(2): each set null or a musl fd_set, `ts` null or a musl
    /// timespec, `mask` null or a sigset_t.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __wrap___pselect_time64(
        n: c_int,
        r: *mut c_void,
        w: *mut c_void,
        e: *mut c_void,
        ts: *const c_void,
        mask: *const libc::sigset_t,
    ) -> c_int {
        // SAFETY: the caller's arguments, as pselect takes them.
        let rc = unsafe { __real___pselect_time64(n, r, w, e, ts, mask) };
        if rc >= 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ENOSYS) {
            return rc;
        }
        let set = |p: *mut c_void| {
            // SAFETY: p is null or an fd_set, WORDS words long, which
            // nothing else touches while pselect runs.
            (!p.is_null()).then(|| unsafe { std::slice::from_raw_parts_mut(p.cast::<c_ulong>(), WORDS) })
        };
        // SAFETY: ts is null or a musl timespec (Timespec64).
        let ms = unsafe { ts.cast::<Timespec64>().as_ref() }.map_or(-1, |t| super::timeout_ms(t.sec, t.nsec.into()));
        let n = (n.max(0) as usize).min(libc::FD_SETSIZE);
        to_c(super::select_by_poll(n, [set(r), set(w), set(e)], |fds| {
            // SAFETY: an empty set, which sigprocmask fills with the mask
            // as it was.
            let mut old: libc::sigset_t = unsafe { std::mem::zeroed() };
            // SAFETY: mask is null or a sigset_t; old is ours.
            let masked = !mask.is_null() && unsafe { libc::sigprocmask(libc::SIG_SETMASK, mask, &mut old) } == 0;
            // SAFETY: fds is a slice of pollfd of its own length.
            let rc = ok(unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) });
            if masked {
                // SAFETY: old is the mask sigprocmask gave us.
                unsafe { libc::sigprocmask(libc::SIG_SETMASK, &old, std::ptr::null_mut()) };
            }
            rc
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    fn enosys<T>(_: c_int, _: &CStr) -> io::Result<T> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    #[test]
    fn paths_through_proc() {
        assert_eq!(through_proc(5, &c("stat")), Some(c("/proc/self/fd/5/stat")));
        assert_eq!(through_proc(5, &c("/etc/passwd")), Some(c("/etc/passwd")));
        assert_eq!(through_proc(libc::AT_FDCWD, &c("stat")), Some(c("stat")));
    }

    #[test]
    fn retry_falls_back_only_on_enosys() {
        let got = retry(7, &c("task"), enosys, |p| Ok(p.to_owned()));
        assert_eq!(got.unwrap(), c("/proc/self/fd/7/task"));
        let got: io::Result<()> =
            retry(7, &c("task"), |_, _| Err(io::Error::from_raw_os_error(libc::ENOENT)), |_| panic!("fell back"));
        assert_eq!(got.unwrap_err().raw_os_error(), Some(libc::ENOENT));
        let got = retry(7, &c("task"), |d, _| Ok(d), |_| panic!("fell back"));
        assert_eq!(got.unwrap(), 7);
    }

    /// The fallback's path reaches what the directory descriptor names.
    #[cfg(target_os = "linux")]
    #[test]
    fn retry_reads_through_a_directory_descriptor() {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;
        let dir = std::fs::File::open("/proc/self").unwrap();
        let read = |p: &CStr| std::fs::read(std::ffi::OsStr::from_bytes(p.to_bytes()));
        let stat = retry(dir.as_raw_fd(), &c("stat"), enosys, read).unwrap();
        let pid = std::process::id().to_string();
        assert!(stat.starts_with(pid.as_bytes()), "{}", String::from_utf8_lossy(&stat));
    }

    #[test]
    fn timeouts() {
        assert_eq!(timeout_ms(3, 0), 3000);
        assert_eq!(timeout_ms(1, 500_000_000), 1500);
        assert_eq!(timeout_ms(0, 1), 1);
        assert_eq!(timeout_ms(i64::MAX, 0), c_int::MAX);
        assert_eq!(timeout_ms(-1, 0), 0);
    }

    fn set(fds: &[usize]) -> Vec<c_ulong> {
        let mut v = vec![0; libc::FD_SETSIZE / BITS];
        for &fd in fds {
            v[fd / BITS] |= 1 << (fd % BITS);
        }
        v
    }

    #[test]
    fn select_asks_poll_and_marks_what_is_ready() {
        let (mut r, mut w) = (set(&[0, 3]), set(&[3]));
        let ready = select_by_poll(4, [Some(&mut r), Some(&mut w), None], |fds| {
            let asked: Vec<_> = fds.iter().map(|p| (p.fd, p.events)).collect();
            assert_eq!(asked, [(0, libc::POLLIN), (3, libc::POLLIN | libc::POLLOUT)]);
            fds[0].revents = libc::POLLIN;
            fds[1].revents = libc::POLLOUT;
            Ok(2)
        })
        .unwrap();
        assert_eq!(ready, 2);
        assert_eq!(r, set(&[0]));
        assert_eq!(w, set(&[3]));
    }

    #[test]
    fn select_times_out_with_empty_sets() {
        let mut r = set(&[0]);
        let ready = select_by_poll(1, [Some(&mut r), None, None], |_| Ok(0)).unwrap();
        assert_eq!(ready, 0);
        assert_eq!(r, set(&[]));
    }

    #[test]
    fn select_by_real_poll_sees_a_pipe() {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        let fd = a.as_raw_fd() as usize;
        let mut r = set(&[fd]);
        let poll = |fds: &mut [libc::pollfd]| {
            // SAFETY: fds is a slice of pollfd of its own length.
            let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 0) };
            if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(rc) }
        };
        assert_eq!(select_by_poll(fd + 1, [Some(&mut r), None, None], poll).unwrap(), 0);
        b.write_all(b"q").unwrap();
        let mut r = set(&[fd]);
        assert_eq!(select_by_poll(fd + 1, [Some(&mut r), None, None], poll).unwrap(), 1);
        assert_eq!(r, set(&[fd]));
    }
}
