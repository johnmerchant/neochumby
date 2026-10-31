//! Signals to other processes, by pid or by process group, whether a
//! process is alive, the descriptors a child is given, what else a child
//! is started with, and this process's priority and address space.

use std::io;
use std::os::fd::RawFd;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// A signal [`kill`] and [`kill_group`] can send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// SIGTERM: asks the process to stop.
    Term,
    /// SIGKILL: ends it.
    Kill,
}

impl Signal {
    fn raw(self) -> libc::c_int {
        match self {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        }
    }
}

/// Sends `sig` to the process `pid` (as from `Child::id`). A pid of 0, or
/// one too large for a pid_t, is refused without a system call: kill(2)
/// would read it as a process group or every process.
pub fn kill(pid: u32, sig: Signal) -> io::Result<()> {
    send(positive(pid)?, sig)
}

/// Sends `sig` to every process in the group `pgid` (the group a child
/// leads after `process_group(0)` has its pid as the group's id). 0 or a
/// value too large for a pid_t is refused, as for [`kill`].
pub fn kill_group(pgid: u32, sig: Signal) -> io::Result<()> {
    send(-positive(pgid)?, sig)
}

/// The process group of the process `pid`; None if there's no such
/// process, or for an id [`kill`] would refuse.
pub fn group_of(pid: u32) -> Option<u32> {
    let pid = positive(pid).ok()?;
    // SAFETY: getpgid(2) takes a plain integer and touches no memory of
    // ours.
    let g = unsafe { libc::getpgid(pid) };
    u32::try_from(g).ok()
}

/// Whether this process runs as root (its effective uid is 0).
pub fn is_root() -> bool {
    // SAFETY: geteuid(2) takes nothing and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// Lowers this process's priority by `by` (nice(2)); 19 is the lowest. A
/// process may always lower its own, so nothing is reported.
pub fn lower_priority(by: u8) {
    // SAFETY: nice(2) takes an integer and touches no memory of ours.
    unsafe { libc::nice(libc::c_int::from(by.min(19))) };
}

/// Whether any process is left in the group `pgid` (kill(2) with signal
/// 0, which sends nothing). False for an id [`kill_group`] would refuse.
pub fn group_alive(pgid: u32) -> bool {
    let Ok(p) = positive(pgid) else { return false };
    // SAFETY: as in `send`; signal 0 only checks that the group exists and
    // may be signalled.
    unsafe { libc::kill(-p, 0) == 0 }
}

/// Whether the process `pid` exists (a zombie, not yet reaped, counts), as
/// kill(2) with no signal says: it does unless the kernel finds no such
/// process. 0, or a value too large for a pid_t, is not a process.
pub fn alive(pid: u32) -> bool {
    let Ok(pid) = positive(pid) else { return false };
    // SAFETY: kill(2) takes plain integers and touches no memory of ours;
    // signal 0 sends nothing, and `pid` is a single process.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    // EPERM: it exists, but is another user's.
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// When the process `pid` started, in clock ticks since the boot (field 22
/// of `/proc/<pid>/stat`), or None if that can't be read: no such process,
/// or no /proc (a system other than Linux). With its pid, it names a
/// process for good: a pid is given again once its process is gone, but
/// not with the same start.
pub fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command's name, in parentheses (field 2), may hold anything, so
    // count from the last ')': field 3 is the first after it.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(22 - 3)?.parse().ok()
}

/// Whether our child `pid` has exited, without reaping it (waitid(2) with
/// WNOWAIT). Until it's reaped (`Child::wait`), its pid, and the id of the
/// process group it leads, can't be given to another process, so its group
/// can still be signalled safely.
pub fn exited(pid: u32) -> io::Result<bool> {
    let pid = positive(pid)?;
    // SAFETY: siginfo_t is plain data, for which all zeroes is valid.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    // SAFETY: `info` is a live siginfo_t the kernel writes into; the other
    // arguments are integers.
    if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: waitid filled in `info`; with WNOHANG and nothing to report,
    // it left si_pid as the zero it was given.
    Ok(unsafe { info.si_pid() } != 0)
}

/// Has the kernel send the child `cmd` starts SIGTERM when the thread that
/// started it exits, which for a thread that waits on it is when its parent
/// dies, however that happens (prctl PR_SET_PDEATHSIG, Linux 2.1.57 on).
/// Elsewhere it does nothing.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
pub fn term_with_parent(cmd: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: the closure runs in the child between fork and exec,
        // where only async-signal-safe calls are allowed. prctl(2) is a
        // plain system call; the closure allocates nothing, takes no lock
        // and touches only an error built from errno.
        unsafe {
            cmd.pre_exec(|| {
                // prctl's arguments are unsigned longs, passed as such.
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

/// Starts the program `cmd` runs on its own: in a session of its own
/// (setsid(2): no controlling terminal, so it can't type into one, and a
/// process group of its own whose id is its pid), with `keep`, if given, as
/// its descriptor 3, and every other descriptor from 3 up closed as it
/// starts. Its stdin, stdout and stderr are what `cmd` says. Don't also
/// call `process_group`: setsid fails in a group's leader.
pub fn isolate(cmd: &mut Command, keep: Option<RawFd>) {
    // SAFETY: the closure runs in the forked child, where only
    // async-signal-safe calls are allowed: setsid, dup2, fcntl, getrlimit
    // and close_range are plain system calls, and it allocates nothing.
    // dup2 replaces whatever the child had as 3; only descriptors std
    // opened close-on-exec can be there, and they're closed at exec anyway.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            let first = match keep {
                Some(3) => {
                    if libc::fcntl(3, libc::F_SETFD, 0) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    4
                }
                Some(fd) => {
                    if libc::dup2(fd, 3) != 3 {
                        return Err(io::Error::last_os_error());
                    }
                    4
                }
                None => 3,
            };
            close_on_exec_from(first);
            Ok(())
        });
    }
}

/// Marks every descriptor from `first` up close-on-exec: close_range(2)
/// where the kernel has it (Linux 5.11), else one at a time up to the
/// limit. Async-signal-safe.
fn close_on_exec_from(first: libc::c_int) {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let (first, last): (libc::c_uint, libc::c_uint) = (first as libc::c_uint, libc::c_uint::MAX);
        // SAFETY: close_range takes integers and touches no memory.
        if unsafe { libc::syscall(libc::SYS_close_range, first, last, libc::CLOSE_RANGE_CLOEXEC) } == 0 {
            return;
        }
    }
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit writes one rlimit into `lim`, a live local.
    let max = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } == 0 { lim.rlim_cur } else { 1024 };
    for fd in first..max.min(65536) as libc::c_int {
        // SAFETY: F_SETFD takes integers; a number that isn't open fails
        // with EBADF, which is ignored.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
}

fn positive(id: u32) -> io::Result<libc::pid_t> {
    match libc::pid_t::try_from(id) {
        Ok(p) if p > 0 => Ok(p),
        _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "not a process id")),
    }
}

fn send(target: libc::pid_t, sig: Signal) -> io::Result<()> {
    // SAFETY: kill(2) takes plain integers and touches no memory of ours;
    // `target` is a single process or a single group, never 0 or -1.
    if unsafe { libc::kill(target, sig.raw()) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// This process's real user id (getuid(2)).
pub fn uid() -> u32 {
    // SAFETY: getuid(2) takes nothing, touches no memory and can't fail.
    unsafe { libc::getuid() }
}

/// Passes the descriptors `fds` on to the program `cmd` runs, though they
/// are close-on-exec here: in the child, between fork and exec, each is
/// marked not close-on-exec. Starting the program fails if one isn't open.
pub fn pass_fds(cmd: &mut Command, fds: Vec<RawFd>) {
    // SAFETY: the closure runs in the forked child, where only
    // async-signal-safe calls are allowed: it calls fcntl(2), which is,
    // reads a Vec allocated before the fork, and allocates nothing.
    unsafe {
        cmd.pre_exec(move || {
            for &fd in &fds {
                if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

/// What [`limit_child`] gives a program it starts: rlimits, soft and hard,
/// so it can't raise them again, and, where the kernel lets a user do it
/// unprivileged, no network.
#[derive(Clone, Copy, Debug)]
pub struct ChildLimits {
    /// CPU time, in seconds (RLIMIT_CPU): past it the kernel kills it.
    pub cpu_secs: u64,
    /// Its address space, in bytes (RLIMIT_AS).
    pub memory: u64,
    /// The largest file it may write, in bytes (RLIMIT_FSIZE).
    pub file_size: u64,
    /// On Linux, a user namespace and a network namespace of its own
    /// (unshare(2)), whose only interface is a loopback that is down: it
    /// can reach nothing. A kernel or a distribution that refuses
    /// unprivileged user namespaces refuses this, and the program then
    /// runs with the network it would have had; [`own_network`] says
    /// which. Elsewhere, nothing.
    pub no_network: bool,
}

/// Has `cmd`'s program start with `limits`, and no core dumps: in the
/// child, between fork and exec, each is set with setrlimit(2), and the
/// namespaces made. Starting the program fails if an rlimit can't be set.
pub fn limit_child(cmd: &mut Command, limits: ChildLimits) {
    // SAFETY: the closure runs in the forked child, where only
    // async-signal-safe calls are allowed: setrlimit(2) and unshare(2) are
    // system calls, each reading only a local rlimit, and nothing is
    // allocated. (musl's setrlimit applies itself to every thread; its
    // fork has already left the child knowing it has only the one.)
    unsafe {
        cmd.pre_exec(move || {
            let set = |resource, value: u64| {
                let r = libc::rlimit { rlim_cur: value as libc::rlim_t, rlim_max: value as libc::rlim_t };
                if libc::setrlimit(resource, &r) == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
            };
            set(libc::RLIMIT_CPU, limits.cpu_secs)?;
            set(libc::RLIMIT_AS, limits.memory)?;
            set(libc::RLIMIT_FSIZE, limits.file_size)?;
            set(libc::RLIMIT_CORE, 0)?;
            #[cfg(target_os = "linux")]
            if limits.no_network {
                // Refused is not an error: own_network says it.
                libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET);
            }
            Ok(())
        });
    }
}

/// Whether the running process `pid` has a network namespace other than
/// this process's (as [`ChildLimits::no_network`] asks), from /proc: None
/// where that can't be told (not Linux, or it has exited).
pub fn own_network(pid: u32) -> Option<bool> {
    let theirs = std::fs::read_link(format!("/proc/{pid}/ns/net")).ok()?;
    let ours = std::fs::read_link("/proc/self/ns/net").ok()?;
    Some(theirs != ours)
}

/// Limits this process's address space to `bytes` (RLIMIT_AS, soft and
/// hard), so that it can't raise it again: an allocation past it fails.
pub fn limit_memory(bytes: u32) -> io::Result<()> {
    let r = libc::rlimit { rlim_cur: bytes as libc::rlim_t, rlim_max: bytes as libc::rlim_t };
    // SAFETY: setrlimit(2) reads one rlimit, `r`, live through the call.
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &r) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_s_start_time() {
        let me = start_time(std::process::id()).unwrap();
        assert_eq!(start_time(std::process::id()), Some(me));
        let mut c = Command::new("sleep").arg("1").spawn().unwrap();
        assert!(start_time(c.id()).unwrap() >= me);
        c.kill().unwrap();
        c.wait().unwrap();
        assert_eq!(start_time(c.id()), None);
    }

    #[test]
    fn refuses_what_is_not_one_process_or_group() {
        for id in [0, u32::MAX, 1 << 31] {
            assert_eq!(kill(id, Signal::Term).unwrap_err().kind(), io::ErrorKind::InvalidInput);
            assert_eq!(kill_group(id, Signal::Term).unwrap_err().kind(), io::ErrorKind::InvalidInput);
            assert!(!group_alive(id));
            assert!(!alive(id));
        }
    }

    #[test]
    fn a_limited_child_has_its_limits() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "ulimit -t; ulimit -c"]).stdout(std::process::Stdio::piped());
        limit_child(&mut cmd, ChildLimits { cpu_secs: 7, memory: 1 << 30, file_size: 1 << 20, no_network: true });
        let out = cmd.output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "7\n0\n");
    }

    #[test]
    fn the_group_of_a_process() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep").arg("5").process_group(0).spawn().unwrap();
        assert_eq!(group_of(child.id()), Some(child.id()));
        assert_ne!(group_of(std::process::id()), Some(child.id()));
        assert_eq!(group_of(0), None);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(group_of(child.id()), None);
    }

    #[test]
    fn alive_until_reaped() {
        assert!(alive(std::process::id()));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!alive(pid));
    }

    #[test]
    fn exited_leaves_the_zombie_to_be_reaped() {
        let mut child = std::process::Command::new("sh").args(["-c", "exit 3"]).spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !exited(child.id()).unwrap() {
            assert!(std::time::Instant::now() < deadline, "never exited");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Not reaped: its pid is still taken, and its status still there.
        assert!(alive(child.id()));
        assert_eq!(child.wait().unwrap().code(), Some(3));
        assert!(exited(0).is_err());
    }

    /// The child leads a process group of its own, has no terminal, has
    /// the kept descriptor as 3, and not the one it would have inherited.
    #[test]
    fn isolated() {
        use std::os::fd::AsRawFd;
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        // Not 3, which isolate gives `b`: another test may have just closed
        // it. Those opened on the way are kept open until the end.
        let mut held = Vec::new();
        let other = loop {
            let f = std::fs::File::open("/dev/null").unwrap();
            if f.as_raw_fd() != 3 {
                break f;
            }
            held.push(f);
        };
        crate::fd::set_cloexec(std::os::fd::AsFd::as_fd(&other), false).unwrap();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "[ \"$(ps -o pgid= -p $$ | tr -d ' ')\" = $$ ] || exit 2; case $(ps -o tty= -p $$) in *[!?\\ ]*) exit 5;; esac; echo hi >&3 || exit 3; [ -e /dev/fd/$N ] && exit 4; exit 0"]);
        cmd.env("N", other.as_raw_fd().to_string());
        isolate(&mut cmd, Some(b.as_raw_fd()));
        let status = cmd.status().unwrap();
        drop(b);
        drop(other);
        assert_eq!(status.code(), Some(0));
        let mut got = String::new();
        std::io::Read::read_to_string(&mut &a, &mut got).unwrap();
        assert_eq!(got, "hi\n");
    }

    #[test]
    fn term_with_parent_still_runs() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        term_with_parent(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        assert!(alive(child.id()));
        kill(child.id(), Signal::Term).unwrap();
        child.wait().unwrap();
        assert!(!alive(child.id()));
    }
}
