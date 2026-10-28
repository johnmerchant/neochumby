//! Crash reports and a sampling profiler, for a device with no debugger and
//! no perf. An app calls [`install`] first thing in main.
//!
//! - A crash (SIGSEGV, SIGBUS, SIGILL, SIGFPE) prints the signal, the
//!   faulting address and the pc, lr and sp to stderr; look them up against
//!   the unstripped binary with llvm-symbolizer.
//! - With NEOCHUMBY_PROF=file set, ITIMER_PROF samples the interrupted pc and
//!   lr every tick of CPU time, and SIGTERM or SIGINT writes them to the file
//!   as little-endian u32 pairs. In a leaf function (the soft-float routines)
//!   lr is the caller, so their time can be charged to whoever called them:
//!   tools/profile does both.

use std::ffi::c_int;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX: usize = 1 << 17;
static mut SAMPLES: [u32; 2 * MAX] = [0; 2 * MAX];
static COUNT: AtomicUsize = AtomicUsize::new(0);
static mut OUT: [u8; 256] = [0; 256];
static mut NAME: &str = "";

extern "C" fn on_prof(_: c_int, _: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    let i = COUNT.fetch_add(1, Ordering::Relaxed);
    if i < MAX {
        unsafe {
            let mc = &(*(ctx as *const libc::ucontext_t)).uc_mcontext;
            let s = std::ptr::addr_of_mut!(SAMPLES).cast::<u32>().add(2 * i);
            *s = mc.arm_pc;
            *s.add(1) = mc.arm_lr;
        }
    }
}

extern "C" fn on_fault(sig: c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    unsafe {
        let mc = &(*(ctx as *const libc::ucontext_t)).uc_mcontext;
        let msg = format!(
            "{}: signal {sig} at {:#x}: pc={:#x} lr={:#x} sp={:#x}\n",
            *std::ptr::addr_of!(NAME),
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
    unsafe { libc::_exit(128 + sig) };
}

/// Writes the profile, if one is being taken: open, write and close only,
/// so safe in a signal handler.
pub fn dump() {
    let n = COUNT.load(Ordering::Relaxed).min(MAX);
    unsafe {
        let path = std::ptr::addr_of!(OUT).cast::<libc::c_char>();
        if *path == 0 {
            return;
        }
        let fd = libc::open(path, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o644);
        if fd >= 0 {
            libc::write(fd, std::ptr::addr_of!(SAMPLES).cast(), n * 8);
            libc::close(fd);
        }
    }
}

/// The name given to [`install`], to prefix the library's own messages;
/// "neochumby" until it is called.
pub(crate) fn name() -> &'static str {
    match unsafe { *std::ptr::addr_of!(NAME) } {
        "" => "neochumby",
        n => n,
    }
}

/// Installs the crash and termination handlers, and starts the profiler if
/// NEOCHUMBY_PROF is set. `name` prefixes crash reports.
pub fn install(name: &'static str) {
    unsafe {
        *std::ptr::addr_of_mut!(NAME) = name;

        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_fault as *const () as usize;
        sa.sa_flags = libc::SA_SIGINFO;
        for sig in [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE] {
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
        let mut term: libc::sigaction = std::mem::zeroed();
        term.sa_sigaction = on_term as *const () as usize;
        for sig in [libc::SIGTERM, libc::SIGINT] {
            libc::sigaction(sig, &term, std::ptr::null_mut());
        }
    }

    let Some(path) = std::env::var_os("NEOCHUMBY_PROF") else { return };
    let bytes = path.as_encoded_bytes();
    if bytes.len() >= 255 {
        return;
    }
    unsafe {
        let out = std::ptr::addr_of_mut!(OUT).cast::<u8>();
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());

        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_prof as *const () as usize;
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        libc::sigaction(libc::SIGPROF, &sa, std::ptr::null_mut());

        // As fast as the kernel ticks (HZ): every 1 ms asked, rounded up.
        let tick = libc::timeval { tv_sec: 0, tv_usec: 1000 };
        let it = libc::itimerval { it_interval: tick, it_value: tick };
        libc::setitimer(libc::ITIMER_PROF, &it, std::ptr::null_mut());
    }
}
