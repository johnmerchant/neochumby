//! What ncinit, the dist image's /init, needs of the kernel: mounting,
//! loading a module from its image, the kernel's device events, a child in
//! a session of its own on a terminal, and reaping whatever child exits.
//!
//! Only the dist image runs these, on Linux 5.9 and later; the stock
//! 2.6.16 never runs ncinit. Built only with the `boot` feature, which
//! ncinit alone turns on.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// How a file system is mounted, beyond the defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MountFlags {
    pub nosuid: bool,
    pub nodev: bool,
    pub noexec: bool,
}

/// Mounts `source` (a device, or a name for a kernel file system) of type
/// `fstype` on `target` (mount(2), no data).
pub fn mount(source: &str, target: &Path, fstype: &str, flags: MountFlags) -> io::Result<()> {
    let source = cstring(source.as_bytes())?;
    let target = cstring(target.as_os_str().as_bytes())?;
    let fstype = cstring(fstype.as_bytes())?;
    let mut f: libc::c_ulong = 0;
    if flags.nosuid {
        f |= libc::MS_NOSUID as libc::c_ulong;
    }
    if flags.nodev {
        f |= libc::MS_NODEV as libc::c_ulong;
    }
    if flags.noexec {
        f |= libc::MS_NOEXEC as libc::c_ulong;
    }
    // SAFETY: mount(2) reads the three NUL-terminated strings, which
    // outlive the call, and no data (a null pointer); it touches no memory
    // of ours.
    if unsafe { libc::mount(source.as_ptr(), target.as_ptr(), fstype.as_ptr(), f, std::ptr::null()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Loads the kernel module in `file`, with no parameters
/// (finit_module(2): the kernel reads the file itself). EEXIST if a module
/// of its name is loaded already.
pub fn load_module(file: &File) -> io::Result<()> {
    let params = c"";
    // SAFETY: finit_module(2) reads the open descriptor, which `file` owns
    // for the call, and the NUL-terminated parameter string; it writes no
    // memory of ours. The kernel checks the module itself.
    let r = unsafe { libc::syscall(libc::SYS_finit_module, file.as_raw_fd(), params.as_ptr(), 0) };
    if r != 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// A socket on which the kernel announces devices (NETLINK_KOBJECT_UEVENT,
/// its multicast group 1), close-on-exec, reads not waiting.
pub fn uevent_socket() -> io::Result<OwnedFd> {
    // SAFETY: socket(2) takes integers and touches no memory of ours.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_KOBJECT_UEVENT,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socket returned a new descriptor, which nothing else owns.
    let sock = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: sockaddr_nl is plain data, for which all zeroes is valid.
    let mut addr: libc::sockaddr_nl = unsafe { mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    addr.nl_groups = 1;
    // SAFETY: bind(2) reads the sockaddr_nl we pass, of the size we give;
    // the descriptor is open, owned by `sock`.
    let r = unsafe {
        libc::bind(
            sock.as_raw_fd(),
            (&raw const addr).cast(),
            mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    // A burst of events (every device at a module's load) shouldn't
    // overflow it: 1 MiB, as udev asks for. Failing that, the default.
    let size: libc::c_int = 1 << 20;
    // SAFETY: setsockopt(2) reads the int we pass, of the size we give.
    unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUFFORCE,
            (&raw const size).cast(),
            mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    Ok(sock)
}

/// Reads the next message on `sock` (from [`uevent_socket`]) into `buf`
/// without waiting: Some(length) for one the kernel sent, None for one
/// from a process (dropped: only the kernel's are trusted), WouldBlock
/// when there are none. A message longer than `buf` is cut short.
pub fn recv_uevent(sock: BorrowedFd<'_>, buf: &mut [u8]) -> io::Result<Option<usize>> {
    // SAFETY: sockaddr_nl is plain data, for which all zeroes is valid.
    let mut from: libc::sockaddr_nl = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
    // SAFETY: the kernel writes at most `buf.len()` bytes into `buf`, a
    // live, writable slice, and at most `len` bytes into `from`, whose
    // size `len` is; the descriptor is open while it's borrowed.
    let n = unsafe {
        libc::recvfrom(
            sock.as_raw_fd(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            libc::MSG_DONTWAIT,
            (&raw mut from).cast(),
            &mut len,
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((from.nl_pid == 0).then_some(n as usize))
}

/// Waits up to `ms` milliseconds for `fd` to have something to read
/// (poll(2)); false when the time runs out or a signal interrupts.
pub fn readable(fd: BorrowedFd<'_>, ms: u16) -> io::Result<bool> {
    let mut p = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: poll(2) reads and writes the one pollfd we pass, for the
    // count we give; the descriptor is open while it's borrowed.
    match unsafe { libc::poll(&mut p, 1, ms.into()) } {
        -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => Ok(false),
        -1 => Err(io::Error::last_os_error()),
        n => Ok(n > 0),
    }
}

/// Runs the program `cmd` starts in a session of its own, with its
/// standard input (a terminal) as its controlling terminal if it can be,
/// as `setsid -c` does: a shell there gets job control and ^C.
pub fn new_session(cmd: &mut Command) {
    // SAFETY: the closure runs in the forked child, where only
    // async-signal-safe calls are allowed: setsid(2) and ioctl(2) are, and
    // it allocates nothing. A terminal that won't be the controlling one
    // (the console, if it isn't a terminal) leaves the child without one.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
}

/// Reaps a child that has exited, any child, without waiting
/// (waitpid(2), -1, WNOHANG): its pid, or None if none has. Init reaps
/// every orphan the kernel hands it this way.
pub fn reap() -> Option<u32> {
    let mut status = 0;
    // SAFETY: waitpid(2) writes the one int we pass; the other arguments
    // are integers.
    let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
    u32::try_from(pid).ok().filter(|&p| p > 0)
}

fn cstring(b: &[u8]) -> io::Result<CString> {
    CString::new(b).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a string with a NUL in it"))
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use super::*;

    #[test]
    fn uevents_open_and_wait() {
        // Any process may listen; nothing arrives in a test, as a rule.
        let sock = uevent_socket().unwrap();
        let mut buf = [0u8; 64];
        match recv_uevent(sock.as_fd(), &mut buf) {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::WouldBlock),
            Ok(_) => {}
        }
        let _ = readable(sock.as_fd(), 1).unwrap();
    }

    /// In a child of its own: reaping any child would take other tests'.
    #[test]
    fn reaps_a_child() {
        if std::env::var_os("NEOCHUMBY_SYS_REAP").is_none() {
            let me = std::env::current_exe().unwrap();
            let ok = Command::new(me)
                .args(["--exact", "boot::tests::reaps_a_child", "--nocapture", "--test-threads=1"])
                .env("NEOCHUMBY_SYS_REAP", "1")
                .status()
                .unwrap();
            assert!(ok.success());
            return;
        }
        let child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        let mut got = None;
        for _ in 0..200 {
            if let Some(p) = reap() {
                got = Some(p);
                if p == pid {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got, Some(pid));
    }

    #[test]
    fn refuses_a_nul() {
        assert_eq!(mount("a\0b", Path::new("/x"), "tmpfs", MountFlags::default()).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }
}
