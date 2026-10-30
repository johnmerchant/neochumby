//! Who this process is, and becoming someone less: what neochumbyd needs
//! to run each app's companion under a uid of its own.
//!
//! neochumbyd started as root becomes its own user ([`become_user`]),
//! keeping only the capabilities to start a child as another uid
//! (CAP_SETUID, CAP_SETGID), to give that child a directory (CAP_CHOWN),
//! and to stop it (CAP_KILL). A child started as another uid with `CommandExt::uid` and
//! `gid` loses them at exec: it has no file capabilities, no ambient or
//! inheritable ones, and can't gain any (no_new_privs).

#[cfg(target_os = "linux")]
use std::io;

/// Adds `bits` to this process's file mode creation mask (umask(2)), for
/// it and every child it starts, and returns the mask it was.
// mode_t is u32 on Linux, and u16 on macOS.
#[allow(clippy::unnecessary_cast)]
pub fn add_to_umask(bits: u32) -> u32 {
    // SAFETY: umask(2) takes an integer, touches no memory of ours and
    // can't fail; it returns the mask it replaced, which is then put back
    // with `bits` added. mode_t is 16 bits on macOS: masks are 0o777 at
    // most.
    let old = unsafe { libc::umask(0o077) };
    // SAFETY: as above.
    unsafe { libc::umask(old | (bits & 0o777) as libc::mode_t) };
    old as u32
}

/// The capabilities [`become_user`] keeps: CAP_CHOWN (0), CAP_KILL (5),
/// CAP_SETGID (6) and CAP_SETUID (7), as bits of the kernel's capability
/// sets. CAP_KILL adds nothing to CAP_SETUID, which can become anyone.
#[cfg(target_os = "linux")]
const KEPT: u32 = 0b1110_0001;

/// capset(2)'s header and one of its two data words (version 3: 64-bit
/// sets, as two 32-bit halves), as linux/capability.h has them. Only the
/// kernel reads their fields.
#[cfg(target_os = "linux")]
#[repr(C)]
#[allow(dead_code)]
struct CapHeader {
    version: u32,
    pid: libc::c_int,
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[allow(dead_code)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

#[cfg(target_os = "linux")]
const CAPABILITY_VERSION_3: u32 = 0x2008_0522;

#[cfg(target_os = "linux")]
fn check(r: libc::c_int) -> io::Result<()> {
    if r == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Makes this process, run as root, `uid` with group `gid` and no other
/// groups, keeping CAP_SETUID, CAP_SETGID, CAP_CHOWN and CAP_KILL and
/// nothing else, and sets no_new_privs, so neither it nor anything it starts can gain
/// privileges by exec (a set-uid file, file capabilities).
///
/// The uids change for the whole process, but capabilities belong to a
/// thread: only the calling thread, and threads it starts afterwards, keep
/// the three. So call it before starting any. It fails when not root.
#[cfg(target_os = "linux")]
pub fn become_user(uid: u32, gid: u32) -> io::Result<()> {
    if uid == 0 || gid == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not root's uid or group"));
    }
    let groups = [gid];
    let header = CapHeader { version: CAPABILITY_VERSION_3, pid: 0 };
    let data = [
        CapData { effective: KEPT, permitted: KEPT, inheritable: 0 },
        CapData { effective: 0, permitted: 0, inheritable: 0 },
    ];
    // SAFETY: prctl(2), setgroups(2), setresgid(2) and setresuid(2) take
    // integers, and setgroups a pointer to `groups`, which outlives the
    // call and holds the one gid it is told of. capset(2) reads `header`
    // and the two words of `data`, both live locals laid out as the
    // kernel's structures (repr(C), version 3 taking two words).
    unsafe {
        // Kept across the change of uid below, which otherwise clears the
        // permitted set; capset then trims it to KEPT.
        check(libc::prctl(libc::PR_SET_KEEPCAPS, 1, 0, 0, 0))?;
        check(libc::setgroups(1, groups.as_ptr()))?;
        check(libc::setresgid(gid, gid, gid))?;
        check(libc::setresuid(uid, uid, uid))?;
        let r = libc::syscall(libc::SYS_capset, &header as *const CapHeader, data.as_ptr());
        check(if r == 0 { 0 } else { -1 })?;
        check(libc::prctl(libc::PR_SET_KEEPCAPS, 0, 0, 0, 0))?;
        check(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn the_umask_only_gains_bits() {
        let old = add_to_umask(0o002);
        assert_eq!(add_to_umask(0), old | 0o002);
        // SAFETY: as in add_to_umask: put back for the other tests.
        unsafe { libc::umask(old as libc::mode_t) };
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_root_becomes_someone_else() {
        assert!(become_user(0, 0).is_err());
        // As root it would work, and change who runs the other tests.
        if crate::user::effective().uid != 0 {
            assert!(become_user(1000, 1000).is_err());
        }
    }
}
