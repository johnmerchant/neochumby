//! neochumby: the Chumby Classic's screen, inputs and speaker, for the
//! launcher, the apps and the services (stock 2.6.16 kernel, static musl,
//! ARM926).

pub mod apps;
pub mod audio;
pub mod conf;
pub mod debug;
pub mod fb;
pub mod input;
pub mod settings;

// Linked for its symbols: see neochumby-softfloat.
#[cfg(feature = "softfloat")]
use neochumby_softfloat as _;

/// Sleeps about `ms` milliseconds; use it instead of std::thread::sleep,
/// which panics when this kernel fails an interrupted nanosleep with EINVAL.
pub fn sleep_ms(ms: u32) {
    unsafe { libc::poll(std::ptr::null_mut(), 0, ms.min(i32::MAX as u32) as i32) };
}
