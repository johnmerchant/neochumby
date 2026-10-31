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

#[cfg(all(target_os = "linux", target_arch = "arm"))]
use neochumby_sys::crash;

/// Elsewhere (the host, where the library's tests run) there are no crash
/// reports and no profiler: neochumby_sys::crash is the Chumby's alone.
#[cfg(not(all(target_os = "linux", target_arch = "arm")))]
mod crash {
    pub fn dump() {}
    pub fn name() -> &'static str {
        ""
    }
    pub fn install(_: &'static str) {}
    pub fn profile(_: &[u8]) {}
}

/// Writes the profile, if one is being taken: open, write and close only,
/// so safe in a signal handler.
pub fn dump() {
    crash::dump();
}

/// The name given to [`install`], to prefix the library's own messages;
/// "neochumby" until it is called.
pub(crate) fn name() -> &'static str {
    match crash::name() {
        "" => "neochumby",
        n => n,
    }
}

/// Installs the crash and termination handlers, and starts the profiler if
/// NEOCHUMBY_PROF is set. `name` prefixes crash reports.
pub fn install(name: &'static str) {
    crash::install(name);
    let Some(path) = std::env::var_os("NEOCHUMBY_PROF") else { return };
    crash::profile(path.as_encoded_bytes());
}
