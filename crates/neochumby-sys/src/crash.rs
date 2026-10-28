//! The signal handlers behind neochumby's crash reports and sampling
//! profiler (neochumby::debug), for the Chumby's ARM: a fault prints the
//! registers it happened at, SIGTERM and SIGINT write the profile, and
//! SIGPROF records a sample.
//!
//! What the handlers share with the rest of the process is written once
//! and published through atomics: the name by [`install`], the profile's
//! path by [`profile`] (only the first call of each counts), and each
//! sample into a slot that one SIGPROF claimed by an atomic index.

use std::ffi::c_int;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

const MAX: usize = 1 << 17;
static mut SAMPLES: [u32; 2 * MAX] = [0; 2 * MAX];
static COUNT: AtomicUsize = AtomicUsize::new(0);
// The profile's path, NUL-terminated: written once by the call to
// [`profile`] that set CLAIMED, then read only after READY.
static mut OUT: [u8; 256] = [0; 256];
static CLAIMED: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
// The name given to [`install`]: null until then, else a leaked &'static str.
static NAME: AtomicPtr<&'static str> = AtomicPtr::new(std::ptr::null_mut());

extern "C" fn on_prof(_: c_int, _: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    let i = COUNT.fetch_add(1, Ordering::Relaxed);
    if i < MAX {
        // SAFETY: with SA_SIGINFO the kernel passes the interrupted
        // context as a valid ucontext_t; slot `i` was claimed by this call
        // alone and is inside SAMPLES (i < MAX).
        unsafe {
            let mc = &(*(ctx as *const libc::ucontext_t)).uc_mcontext;
            let s = std::ptr::addr_of_mut!(SAMPLES).cast::<u32>().add(2 * i);
            *s = mc.arm_pc;
            *s.add(1) = mc.arm_lr;
        }
    }
}

extern "C" fn on_fault(sig: c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    // SAFETY: with SA_SIGINFO the kernel passes a valid siginfo_t and
    // ucontext_t; write(2) reads `msg.len()` bytes of the live String;
    // _exit(2) does not return.
    unsafe {
        let mc = &(*(ctx as *const libc::ucontext_t)).uc_mcontext;
        let msg = format!(
            "{}: signal {sig} at {:#x}: pc={:#x} lr={:#x} sp={:#x}\n",
            name(),
            (*info).si_addr() as usize,
            mc.arm_pc,
            mc.arm_lr,
            mc.arm_sp
        );
        libc::write(2, msg.as_ptr().cast(), msg.len());
        libc::_exit(128 + sig);
    }
}

extern "C" fn on_term(sig: c_int) {
    dump();
    // SAFETY: _exit(2) ends the process at once; nothing of ours is left
    // to observe.
    unsafe { libc::_exit(128 + sig) };
}

/// Writes the profile, if one is being taken: open, write and close only,
/// so safe in a signal handler.
pub fn dump() {
    let n = COUNT.load(Ordering::Relaxed).min(MAX);
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let path = std::ptr::addr_of!(OUT).cast::<libc::c_char>();
    // SAFETY: READY means OUT holds the path, written once and never again,
    // and NUL-terminated ([`profile`] leaves at least its last byte 0).
    let fd = unsafe { libc::open(path, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o644) };
    if fd >= 0 {
        // SAFETY: the first n <= MAX sample pairs, 8 bytes each, are inside
        // SAMPLES; `fd` is the descriptor just opened, closed once.
        unsafe {
            libc::write(fd, std::ptr::addr_of!(SAMPLES).cast(), n * 8);
            libc::close(fd);
        }
    }
}

/// The name given to [`install`]; empty until it is called.
pub fn name() -> &'static str {
    let p = NAME.load(Ordering::Acquire);
    if p.is_null() {
        return "";
    }
    // SAFETY: a non-null NAME is a leaked Box, never freed or written after
    // it was published.
    unsafe { *p }
}

/// Installs the crash handlers (SIGSEGV, SIGBUS, SIGILL, SIGFPE: the signal,
/// the faulting address and pc, lr and sp to stderr, prefixed by `name`, then
/// exit) and the termination handlers (SIGTERM, SIGINT: [`dump`], then
/// exit). Call it first thing in main; a second call changes nothing.
pub fn install(name: &'static str) {
    let p = Box::into_raw(Box::new(name));
    if NAME.compare_exchange(std::ptr::null_mut(), p, Ordering::AcqRel, Ordering::Acquire).is_err() {
        // SAFETY: `p` was never published; it is still this call's Box.
        drop(unsafe { Box::from_raw(p) });
        return;
    }

    // SAFETY: all-zero is a valid sigaction (no flags, an empty mask).
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = on_fault as *const () as usize;
    sa.sa_flags = libc::SA_SIGINFO;
    for sig in [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE] {
        // SAFETY: `sa` names a handler of the SA_SIGINFO signature and is
        // live for the call; the old action isn't asked for.
        unsafe { libc::sigaction(sig, &sa, std::ptr::null_mut()) };
    }
    // SAFETY: as above.
    let mut term: libc::sigaction = unsafe { std::mem::zeroed() };
    term.sa_sigaction = on_term as *const () as usize;
    for sig in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: `term` names a handler of the plain signature (no
        // SA_SIGINFO) and is live for the call.
        unsafe { libc::sigaction(sig, &term, std::ptr::null_mut()) };
    }
}

/// Starts the profiler, writing to `path` on [`dump`]: ITIMER_PROF samples
/// the interrupted pc and lr every tick of CPU time. Does nothing if the
/// path is 255 bytes or longer; a second call changes nothing. Call it
/// after [`install`].
pub fn profile(path: &[u8]) {
    if path.len() >= 255 || CLAIMED.swap(true, Ordering::AcqRel) {
        return;
    }
    // SAFETY: only the call that set CLAIMED gets here, and [`dump`] reads
    // OUT only after READY, set below; the path fits with OUT's last byte
    // still 0 (the terminator).
    unsafe {
        let out = std::ptr::addr_of_mut!(OUT).cast::<u8>();
        std::ptr::copy_nonoverlapping(path.as_ptr(), out, path.len());
    }
    // An empty path takes samples but writes none.
    if !path.is_empty() {
        READY.store(true, Ordering::Release);
    }

    // SAFETY: all-zero is a valid sigaction.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = on_prof as *const () as usize;
    sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
    // SAFETY: `sa` names a handler of the SA_SIGINFO signature and is live
    // for the call.
    unsafe { libc::sigaction(libc::SIGPROF, &sa, std::ptr::null_mut()) };

    // As fast as the kernel ticks (HZ): every 1 ms asked, rounded up.
    let tick = libc::timeval { tv_sec: 0, tv_usec: 1000 };
    let it = libc::itimerval { it_interval: tick, it_value: tick };
    // SAFETY: `it` is live for the call; the old timer isn't asked for.
    unsafe { libc::setitimer(libc::ITIMER_PROF, &it, std::ptr::null_mut()) };
}
