//! Locking a process down for good, as far as its kernel can: no new
//! privileges, Landlock, and a seccomp-bpf allowlist of system calls.
//!
//! These are the one exception to this crate's rule of nothing newer than
//! 2.6.16: each is asked of the kernel at run time, and a kernel without it
//! says so (`PR_SET_NO_NEW_PRIVS` fails with `EINVAL` before 3.5, Landlock's
//! calls with `ENOSYS` or `EOPNOTSUPP` before 5.13 or where it is off), so
//! the same binary runs on the Chumby's stock kernel, where [`lock_down`]
//! does nothing and says so, and on the dist's, where it does all it can.
//! Nothing here goes through rustix, which needs 3.2 to run at all.
//!
//! What [`lock_down`] leaves a process:
//!
//! - **Landlock** (5.13 and later): every file system access its ABI
//!   knows denied, everywhere, from then on: no open, create, remove,
//!   rename, truncate or exec of anything, though what is already open
//!   stays open. From ABI 4, no TCP bind or connect; from ABI 6, no
//!   signals to processes outside it and no abstract Unix sockets.
//! - **seccomp** (3.5, and 4.14 for killing the whole process): the system
//!   calls in the allowlist it is given, for this architecture only;
//!   anything else kills the process (SIGSYS). [`Rule::NoExec`] allows a
//!   call only without `PROT_EXEC` in one of its arguments.

use std::io;

/// `prctl(2)` options and seccomp's values (linux/prctl.h, linux/seccomp.h).
const PR_SET_NO_NEW_PRIVS: libc::c_int = 38;
const PR_SET_SECCOMP: libc::c_int = 22;
const SECCOMP_MODE_FILTER: libc::c_ulong = 2;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_TRAP: u32 = 0x0003_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;

/// This architecture, as seccomp reports it (linux/audit.h).
#[cfg(target_arch = "arm")]
const AUDIT_ARCH: u32 = 0x4000_0028;
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

/// Classic BPF's opcodes, as the filter uses them.
const LD_W_ABS: u16 = 0x20;
const JEQ_K: u16 = 0x15;
const JSET_K: u16 = 0x45;
const RET_K: u16 = 0x06;

/// struct seccomp_data's offsets: the call's number, the architecture, and
/// the low 32 bits of each argument (little-endian).
const DATA_NR: u32 = 0;
const DATA_ARCH: u32 = 4;
const fn data_arg(i: u8) -> u32 {
    16 + 8 * i as u32
}

/// Landlock's calls, the same number on every architecture.
const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;

/// struct landlock_ruleset_attr, as of ABI 6; earlier ABIs take a prefix.
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

/// One system call the filter lets through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// The call, whatever its arguments.
    Allow(libc::c_long),
    /// The call, unless argument `arg` (from 0) has `PROT_EXEC` set: `mmap`
    /// and `mprotect`, so nothing new can be made executable.
    NoExec(libc::c_long, u8),
}

/// What the filter does with a call it doesn't allow. Either way the call
/// is never made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Violation {
    /// Kills the process (SIGSYS).
    Kill,
    /// Says which call it was on stderr, then exits with 159 (128 +
    /// SIGSYS): for finding what a filter is missing.
    Report,
}

/// What [`lock_down`] managed: each is false (or None) where the kernel
/// hasn't it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Locked {
    pub no_new_privs: bool,
    /// The Landlock ABI applied.
    pub landlock: Option<u32>,
    pub seccomp: bool,
}

impl std::fmt::Display for Locked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if let Some(abi) = self.landlock {
            parts.push(format!("landlock (abi {abi})"));
        }
        if self.seccomp {
            parts.push("seccomp".to_string());
        }
        if parts.is_empty() {
            f.write_str("nothing (this kernel has neither seccomp nor Landlock)")
        } else {
            f.write_str(&parts.join(", "))
        }
    }
}

/// The filter for `rules`: the architecture checked first, then each rule
/// in turn; anything else is `deny`, a seccomp return value.
fn filter(rules: &[Rule], deny: u32) -> Vec<libc::sock_filter> {
    let stmt = |code, k| libc::sock_filter { code, jt: 0, jf: 0, k };
    let jump = |code, k, jt, jf| libc::sock_filter { code, jt, jf, k };
    let mut p = vec![
        stmt(LD_W_ABS, DATA_ARCH),
        jump(JEQ_K, AUDIT_ARCH, 1, 0),
        stmt(RET_K, SECCOMP_RET_KILL_PROCESS),
        stmt(LD_W_ABS, DATA_NR),
    ];
    for r in rules {
        match *r {
            Rule::Allow(nr) => {
                p.push(jump(JEQ_K, nr as u32, 0, 1));
                p.push(stmt(RET_K, SECCOMP_RET_ALLOW));
            }
            // Its argument is loaded over the number, so each branch ends
            // in a return.
            Rule::NoExec(nr, arg) => {
                p.push(jump(JEQ_K, nr as u32, 0, 4));
                p.push(stmt(LD_W_ABS, data_arg(arg)));
                p.push(jump(JSET_K, libc::PROT_EXEC as u32, 1, 0));
                p.push(stmt(RET_K, SECCOMP_RET_ALLOW));
                p.push(stmt(RET_K, deny));
            }
        }
    }
    p.push(stmt(RET_K, deny));
    p
}

/// SIGSYS's handler for [`Violation::Report`]: the call's number on stderr,
/// then out. Only write(2) and _exit(2), both async-signal-safe and both
/// allowed by any filter worth having.
extern "C" fn on_sigsys(_: libc::c_int, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    // siginfo_t's _sigsys: after si_signo, si_errno and si_code (padded to
    // a pointer's alignment), _call_addr, then _syscall.
    let at = if size_of::<usize>() == 8 { 24 } else { 16 };
    // SAFETY: the kernel passes a live siginfo_t for SIGSYS from seccomp,
    // whose _syscall is an int at that offset.
    let nr = unsafe { info.cast::<u8>().add(at).cast::<libc::c_int>().read_unaligned() };
    let mut buf = *b"seccomp: forbidden system call           \n";
    let mut n = nr.unsigned_abs();
    let mut i = buf.len() - 2;
    loop {
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
        i -= 1;
    }
    let head = b"seccomp: forbidden system call ".len();
    let end = buf.len();
    buf.copy_within(i..end, head);
    let len = head + (end - i);
    // SAFETY: write(2) from a live buffer, then _exit(2): nothing else.
    unsafe {
        libc::write(2, buf.as_ptr().cast(), len);
        libc::_exit(128 + libc::SIGSYS);
    }
}

/// Installs [`on_sigsys`] for SIGSYS.
fn report_violations() -> io::Result<()> {
    // SAFETY: all-zero is a valid sigaction: no flags and an empty mask.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = on_sigsys as *const () as usize;
    sa.sa_flags = libc::SA_SIGINFO;
    // SAFETY: a live sigaction whose handler only writes and exits; the old
    // action isn't asked for.
    if unsafe { libc::sigaction(libc::SIGSYS, &sa, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sets no_new_privs; false where the kernel hasn't it (before 3.5).
fn no_new_privs() -> io::Result<bool> {
    // SAFETY: prctl(2) with integer arguments only.
    if unsafe {
        libc::prctl(PR_SET_NO_NEW_PRIVS, 1 as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong)
    } == 0
    {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::EINVAL) { Ok(false) } else { Err(e) }
}

/// Landlock's fs rights for ABI `abi`: 13 at first, then REFER (2),
/// TRUNCATE (3) and IOCTL_DEV (5).
fn fs_rights(abi: u32) -> u64 {
    let mut n = 13;
    if abi >= 2 {
        n += 1;
    }
    if abi >= 3 {
        n += 1;
    }
    if abi >= 5 {
        n += 1;
    }
    (1 << n) - 1
}

/// Applies a Landlock ruleset that handles everything this kernel's ABI
/// knows and allows none of it; the ABI, or None where there's no
/// Landlock. Needs no_new_privs (or CAP_SYS_ADMIN) first.
fn landlock() -> io::Result<Option<u32>> {
    // SAFETY: landlock_create_ruleset(NULL, 0, VERSION) reads nothing and
    // returns the ABI; a kernel without it returns ENOSYS.
    let abi = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi < 0 {
        let e = io::Error::last_os_error();
        return match e.raw_os_error() {
            Some(libc::ENOSYS | libc::EOPNOTSUPP) => Ok(None),
            _ => Err(e),
        };
    }
    let abi = abi as u32;
    let attr = RulesetAttr {
        handled_access_fs: fs_rights(abi),
        handled_access_net: if abi >= 4 { 0b11 } else { 0 },
        scoped: if abi >= 6 { 0b11 } else { 0 },
    };
    // The size tells the kernel which fields the ABI has.
    let size = match abi {
        1..=3 => 8usize,
        4 | 5 => 16,
        _ => size_of::<RulesetAttr>(),
    };
    // SAFETY: `attr` is a live landlock_ruleset_attr of at least `size`
    // bytes; the call returns a new descriptor or -1.
    let fd = unsafe { libc::syscall(SYS_LANDLOCK_CREATE_RULESET, &attr as *const RulesetAttr, size, 0u32) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = fd as libc::c_int;
    // SAFETY: `fd` is the ruleset just made; restrict_self takes it and no
    // pointers.
    let r = unsafe { libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, fd, 0u32) };
    let e = io::Error::last_os_error();
    // SAFETY: `fd` is ours, and used no more.
    unsafe { libc::close(fd) };
    if r < 0 {
        return Err(e);
    }
    Ok(Some(abi))
}

/// Installs the filter for `rules`.
fn seccomp(rules: &[Rule], violation: Violation) -> io::Result<()> {
    let deny = match violation {
        Violation::Kill => SECCOMP_RET_KILL_PROCESS,
        Violation::Report => {
            report_violations()?;
            SECCOMP_RET_TRAP
        }
    };
    let p = filter(rules, deny);
    let prog = libc::sock_fprog { len: p.len() as libc::c_ushort, filter: p.as_ptr() as *mut libc::sock_filter };
    // SAFETY: `prog` points at `p`, live through the call; the kernel copies
    // the program.
    if unsafe { libc::prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog as *const libc::sock_fprog) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Locks this process (this thread, and any it starts later) down: no new
/// privileges, then Landlock, then the seccomp filter allowing only
/// `rules` (anything else as `violation` says), each as far as the kernel
/// goes. Irreversible. On a kernel without no_new_privs (2.6.16) it
/// changes nothing and returns [`Locked::default`]; an error means the
/// kernel has the feature but refused it, and the process should not go
/// on.
pub fn lock_down(rules: &[Rule], violation: Violation) -> io::Result<Locked> {
    let mut l = Locked::default();
    if !no_new_privs()? {
        return Ok(l);
    }
    l.no_new_privs = true;
    l.landlock = landlock()?;
    seccomp(rules, violation)?;
    l.seccomp = true;
    Ok(l)
}

/// What a process needs that only computes and uses the descriptors and
/// mappings it already has: reading and writing them, waiting on them,
/// sleeping, clocks, anonymous memory (never executable), random bytes,
/// its own signal mask and returns from handlers, and exiting. Not open,
/// socket, ioctl, exec, kill, ptrace or anything else. Both the 32-bit and
/// the 64-bit time calls are allowed: musl tries the 64-bit ones first.
pub fn compute_only() -> Vec<Rule> {
    use Rule::{Allow, NoExec};
    let mut v = vec![
        Allow(libc::SYS_read),
        Allow(libc::SYS_write),
        Allow(libc::SYS_readv),
        Allow(libc::SYS_writev),
        Allow(libc::SYS_close),
        Allow(libc::SYS_ppoll),
        Allow(libc::SYS_clock_gettime),
        Allow(libc::SYS_clock_nanosleep),
        Allow(libc::SYS_gettimeofday),
        Allow(libc::SYS_exit),
        Allow(libc::SYS_exit_group),
        Allow(libc::SYS_brk),
        Allow(libc::SYS_munmap),
        Allow(libc::SYS_mremap),
        Allow(libc::SYS_madvise),
        NoExec(libc::SYS_mprotect, 2),
        Allow(libc::SYS_rt_sigreturn),
        Allow(libc::SYS_rt_sigprocmask),
        Allow(libc::SYS_getrandom),
        Allow(libc::SYS_sched_yield),
        Allow(libc::SYS_futex),
        Allow(libc::SYS_getpid),
        Allow(libc::SYS_gettid),
    ];
    #[cfg(target_arch = "arm")]
    v.extend([
        NoExec(libc::SYS_mmap2, 2),
        Allow(libc::SYS_poll),
        Allow(libc::SYS_nanosleep),
        Allow(libc::SYS_sigreturn),
        // The time64 calls, by number (libc doesn't name them for ARM):
        // clock_gettime64, clock_nanosleep_time64, ppoll_time64 and
        // futex_time64.
        Allow(403),
        Allow(407),
        Allow(414),
        Allow(422),
    ]);
    #[cfg(target_arch = "x86_64")]
    v.extend([NoExec(libc::SYS_mmap, 2), Allow(libc::SYS_poll), Allow(libc::SYS_nanosleep)]);
    #[cfg(target_arch = "aarch64")]
    v.extend([NoExec(libc::SYS_mmap, 2), Allow(libc::SYS_nanosleep)]);
    v
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    /// Runs `f` in a forked child and returns how it ended: its exit
    /// status, or the signal that killed it, negated.
    fn in_child(f: impl FnOnce() -> i32) -> i32 {
        // SAFETY: fork(2); the child runs only `f` and _exit, with no locks
        // held by other threads that `f` takes (the tests' bodies make
        // system calls and nothing else).
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            let code = f();
            // SAFETY: _exit(2) ends the child without running the parent's
            // atexit handlers or flushing its buffers.
            unsafe { libc::_exit(code) };
        }
        let mut st = 0;
        // SAFETY: waitpid(2) on our own child, into a live c_int.
        assert_eq!(unsafe { libc::waitpid(pid, &mut st, 0) }, pid);
        if libc::WIFSIGNALED(st) { -libc::WTERMSIG(st) } else { libc::WEXITSTATUS(st) }
    }

    /// What the tests' children need to get as far as their own calls.
    const BASE: &[Rule] = &[
        Rule::Allow(libc::SYS_exit_group),
        Rule::Allow(libc::SYS_exit),
        Rule::Allow(libc::SYS_write),
        Rule::NoExec(libc::SYS_mmap, 2),
        Rule::Allow(libc::SYS_getpid),
    ];

    fn kernel_has_seccomp() -> bool {
        in_child(|| if no_new_privs().unwrap() { 0 } else { 1 }) == 0
    }

    #[test]
    fn allowed_calls_pass_and_others_kill() {
        if !kernel_has_seccomp() {
            eprintln!("no seccomp here; skipped");
            return;
        }
        let got = in_child(|| {
            lock_down(BASE, Violation::Kill).unwrap();
            // SAFETY: getpid(2), allowed.
            unsafe { libc::getpid() };
            7
        });
        assert_eq!(got, 7);
        let got = in_child(|| {
            lock_down(BASE, Violation::Kill).unwrap();
            // SAFETY: getppid(2), not allowed: the filter kills us here.
            unsafe { libc::getppid() };
            0
        });
        assert_eq!(got, -libc::SIGSYS);
        // An executable mapping, and a plain one.
        let map = |prot| {
            move || {
                lock_down(BASE, Violation::Kill).unwrap();
                // SAFETY: an anonymous private mapping, never used.
                let p = unsafe {
                    libc::mmap(std::ptr::null_mut(), 4096, prot, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0)
                };
                if p == libc::MAP_FAILED { 1 } else { 0 }
            }
        };
        assert_eq!(in_child(map(libc::PROT_READ | libc::PROT_WRITE)), 0);
        assert_eq!(in_child(map(libc::PROT_READ | libc::PROT_EXEC)), -libc::SIGSYS);
        // Reported: the same, as an exit, after saying which call.
        let got = in_child(|| {
            lock_down(BASE, Violation::Report).unwrap();
            // SAFETY: getppid(2), not allowed.
            unsafe { libc::getppid() };
            0
        });
        assert_eq!(got, 128 + libc::SIGSYS);
    }

    #[test]
    fn landlock_leaves_no_file_system() {
        if !kernel_has_seccomp() {
            return;
        }
        let got = in_child(|| {
            let rules = [BASE, &[Rule::Allow(libc::SYS_openat), Rule::Allow(libc::SYS_close)]].concat();
            match lock_down(&rules, Violation::Kill).unwrap().landlock {
                None => 2,
                Some(_) => match std::fs::File::open("/") {
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => 0,
                    _ => 1,
                },
            }
        });
        assert!(got == 0 || got == 2, "{got}");
        if got == 2 {
            eprintln!("no Landlock here; skipped");
        }
    }

    #[test]
    fn the_filter_checks_the_architecture_first() {
        let p = filter(&[Rule::Allow(1)], SECCOMP_RET_KILL_PROCESS);
        assert_eq!((p[0].code, p[0].k), (LD_W_ABS, DATA_ARCH));
        assert_eq!((p[1].code, p[1].k), (JEQ_K, AUDIT_ARCH));
        assert_eq!((p[2].code, p[2].k), (RET_K, SECCOMP_RET_KILL_PROCESS));
        assert_eq!(p.last().unwrap().k, SECCOMP_RET_KILL_PROCESS);
        assert_eq!(fs_rights(1), 0x1fff);
        assert_eq!(fs_rights(6), 0xffff);
    }
}
