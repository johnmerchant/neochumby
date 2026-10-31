//! Sleeping, the monotonic clock and the local time.

use std::time::Duration;

/// Sleeps about `ms` milliseconds, as poll(2) with no descriptors. On the
/// Chumby's kernel, std::thread::sleep panics when an interrupted nanosleep
/// fails with EINVAL; this returns early instead.
pub fn sleep_ms(ms: u32) {
    let ms = ms.min(i32::MAX as u32) as libc::c_int;
    // SAFETY: with no descriptors, poll(2) reads nothing through its null
    // pointer; it only waits.
    unsafe { libc::poll(std::ptr::null_mut(), 0, ms) };
}

/// The time since a fixed point (the boot, on Linux), from CLOCK_MONOTONIC
/// (clock_gettime(2), in 2.6.16): the same clock in every process, and not
/// moved when the wall clock is set. std's `Instant` reads it too, but
/// can't be written down for another process to compare.
// As for `local`: 32-bit musl's time_t.
#[allow(deprecated)]
pub fn monotonic() -> Duration {
    // SAFETY: libc::timespec is integers (and, on some systems, padding),
    // for which zero is valid.
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: `ts` is a live local of the type clock_gettime writes, and
    // CLOCK_MONOTONIC is a clock every kernel we run on has.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    Duration::new(ts.tv_sec.max(0) as u64, ts.tv_nsec.clamp(0, 999_999_999) as u32)
}

/// The wall clock in the local time zone, from the C library's time zone
/// (TZ).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    /// 0-23.
    pub hour: u8,
    /// 0-59.
    pub minute: u8,
    /// 0-60 (a leap second).
    pub second: u8,
}

/// The time now (time(2), then localtime_r(3)); midnight if the C library
/// can't convert it.
pub fn local_time() -> LocalTime {
    // SAFETY: time(2) with a null pointer only returns the time.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    local(now)
}

/// The local time at `t`, seconds since the epoch (localtime_r(3));
/// midnight if the C library can't convert it.
// libc marks 32-bit musl's time_t deprecated (it becomes 64-bit with musl
// 1.2); either width holds the time until 2038.
#[allow(deprecated)]
pub fn local_time_at(t: i64) -> LocalTime {
    local(t as libc::time_t)
}

#[allow(deprecated)]
fn local(now: libc::time_t) -> LocalTime {
    // SAFETY: libc::tm is integers and, on some systems, a pointer to the
    // zone's name, for which null is a valid value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are to live locals of the right types;
    // localtime_r writes only `tm`, and on failure leaves it as it was.
    unsafe { libc::localtime_r(&now, &mut tm) };
    let field = |v: libc::c_int| v.clamp(0, 60) as u8;
    LocalTime { hour: field(tm.tm_hour), minute: field(tm.tm_min), second: field(tm.tm_sec) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_monotonic_clock_goes_on() {
        let a = monotonic();
        sleep_ms(20);
        let b = monotonic();
        assert!(b >= a + Duration::from_millis(15), "{a:?} then {b:?}");
    }
}
