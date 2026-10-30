//! The calls procps makes that the Chumby's Linux 2.6.16 doesn't have, on
//! ARM: openat, fstatat and readlinkat (322 and on, which ARM got in
//! 2.6.17) and pselect6 (2.6.19). build.rs builds top and its library with
//! each renamed to the function here, which makes the real call and, where
//! the kernel says ENOSYS, does the same with the calls 2.6.16 has. A
//! directory descriptor becomes its /proc/self/fd link (top has /proc:
//! `system`), and pselect a poll between two changes of the signal mask.
//! Without these top exits with 47 at once, as it can't read /proc/self.

use std::ffi::{CStr, CString, c_char, c_int, c_uint};

fn enosys() -> bool {
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSYS)
}

/// `path` as seen from the directory `dirfd`, through /proc.
///
/// # Safety
/// `path` is a C string.
unsafe fn at(dirfd: c_int, path: *const c_char) -> Option<CString> {
    // SAFETY: the caller's.
    let p = unsafe { CStr::from_ptr(path) }.to_bytes();
    if dirfd == libc::AT_FDCWD || p.first() == Some(&b'/') {
        return CString::new(p).ok();
    }
    let mut v = format!("/proc/self/fd/{dirfd}/").into_bytes();
    v.extend_from_slice(p);
    CString::new(v).ok()
}

/// openat(2). procps passes a mode only with O_CREAT, which it never uses,
/// so the variadic mode is taken as a fourth argument: on ARM both come in
/// the same register.
///
/// # Safety
/// As openat(2).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn top_openat(dirfd: c_int, path: *const c_char, flags: c_int, mode: c_uint) -> c_int {
    // SAFETY: the caller's arguments, as openat takes them.
    let fd = unsafe { libc::openat(dirfd, path, flags, mode) };
    if fd >= 0 || !enosys() {
        return fd;
    }
    // SAFETY: path is a C string, as openat's.
    let Some(p) = (unsafe { at(dirfd, path) }) else { return -1 };
    // SAFETY: p is a C string; the mode as above.
    unsafe { libc::open(p.as_ptr(), flags, mode) }
}

/// fstatat(2).
///
/// # Safety
/// As fstatat(2).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn top_fstatat(dirfd: c_int, path: *const c_char, buf: *mut libc::stat, flag: c_int) -> c_int {
    // SAFETY: the caller's arguments, as fstatat takes them.
    let r = unsafe { libc::fstatat(dirfd, path, buf, flag) };
    if r == 0 || !enosys() {
        return r;
    }
    // SAFETY: path is a C string, as fstatat's.
    let Some(p) = (unsafe { at(dirfd, path) }) else { return -1 };
    // SAFETY: p is a C string, and buf a stat to fill, as fstatat's.
    unsafe {
        if flag & libc::AT_SYMLINK_NOFOLLOW != 0 { libc::lstat(p.as_ptr(), buf) } else { libc::stat(p.as_ptr(), buf) }
    }
}

/// readlinkat(2).
///
/// # Safety
/// As readlinkat(2).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn top_readlinkat(dirfd: c_int, path: *const c_char, buf: *mut c_char, size: usize) -> isize {
    // SAFETY: the caller's arguments, as readlinkat takes them.
    let r = unsafe { libc::readlinkat(dirfd, path, buf, size) };
    if r >= 0 || !enosys() {
        return r;
    }
    // SAFETY: path is a C string, as readlinkat's.
    let Some(p) = (unsafe { at(dirfd, path) }) else { return -1 };
    // SAFETY: p is a C string, and buf holds size bytes, as readlinkat's.
    unsafe { libc::readlink(p.as_ptr(), buf, size) }
}

/// pselect(2): poll(2) on the same descriptors, with `mask` in place while
/// it waits (musl's select is pselect6 too). A signal that comes between
/// the change of mask and poll waits for the next key or the timeout,
/// where pselect would end at once; to top that is a screen refreshed a
/// few seconds late.
///
/// # Safety
/// As pselect(2).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn top_pselect(
    n: c_int,
    r: *mut libc::fd_set,
    w: *mut libc::fd_set,
    e: *mut libc::fd_set,
    ts: *const libc::timespec,
    mask: *const libc::sigset_t,
) -> c_int {
    // SAFETY: the caller's arguments, as pselect takes them.
    let rc = unsafe { libc::pselect(n, r, w, e, ts, mask) };
    if rc >= 0 || !enosys() {
        return rc;
    }
    // SAFETY: each set, if not null, is an fd_set holding n descriptors.
    let has = |set: *mut libc::fd_set, fd: c_int| !set.is_null() && unsafe { libc::FD_ISSET(fd, set) };
    let mut fds: Vec<libc::pollfd> = (0..n.clamp(0, libc::FD_SETSIZE as c_int))
        .filter_map(|fd| {
            let events = if has(r, fd) { libc::POLLIN } else { 0 }
                | if has(w, fd) { libc::POLLOUT } else { 0 }
                | if has(e, fd) { libc::POLLPRI } else { 0 };
            (events != 0).then_some(libc::pollfd { fd, events, revents: 0 })
        })
        .collect();
    // SAFETY: ts, if not null, is a timespec, as pselect's.
    let ms = unsafe { ts.as_ref() }.map_or(-1, |t| {
        (t.tv_sec as i64 * 1000 + (t.tv_nsec as i64 + 999_999) / 1_000_000).min(c_int::MAX as i64) as c_int
    });
    // SAFETY: an empty set, filled by sigprocmask with the mask as it was.
    let mut old: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: mask, if not null, is a sigset_t, as pselect's.
    let masked = !mask.is_null() && unsafe { libc::sigprocmask(libc::SIG_SETMASK, mask, &mut old) } == 0;
    // SAFETY: fds is a Vec of pollfd, of its own length.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
    if masked {
        // SAFETY: old is the mask sigprocmask gave us. (top reads no
        // errno after pselect, so it needn't be kept from poll.)
        unsafe { libc::sigprocmask(libc::SIG_SETMASK, &old, std::ptr::null_mut()) };
    }
    if rc < 0 {
        return rc;
    }
    for set in [r, w, e] {
        if !set.is_null() {
            // SAFETY: set is an fd_set, as pselect's.
            unsafe { libc::FD_ZERO(set) };
        }
    }
    // As select counts: each descriptor once for each set it is ready in.
    let mut ready = 0;
    for p in &fds {
        let marks = [(r, libc::POLLIN | libc::POLLHUP | libc::POLLERR), (w, libc::POLLOUT | libc::POLLERR), (e, libc::POLLPRI)];
        for (set, bits) in marks {
            if !set.is_null() && p.revents & bits != 0 {
                // SAFETY: set is an fd_set, and p.fd is below FD_SETSIZE.
                unsafe { libc::FD_SET(p.fd, set) };
                ready += 1;
            }
        }
    }
    ready
}
