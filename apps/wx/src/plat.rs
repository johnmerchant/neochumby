//! What the engine needs of the machine it runs on: the Chumby
//! (device.rs) or a headless renderer on the host (host.rs).

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Touch {
    /// Pressed, or moved while pressed, at screen coordinates.
    Move(i32, i32),
    /// Released, at the last position.
    Up(i32, i32),
}

pub trait Plat {
    /// The screen: 320x240 RGB565, rows of 320 pixels.
    fn fb(&mut self) -> &mut [u16];
    /// The end of a frame. False: stop (the host renderer is done).
    fn present(&mut self) -> bool;
    /// The next touch event this frame, if any.
    fn touch(&mut self) -> Option<Touch>;
    /// The top button's level (true while pressed), read once a frame;
    /// None if it can't be read.
    fn button(&mut self) -> Option<bool>;
    fn backlight(&mut self, night: bool);
    /// Plays a stream at a volume (0-100), or stops (None).
    fn music(&mut self, url: Option<&str>, vol: i32);
    fn volume(&mut self, vol: i32);
    /// On exit, so the Chumby's own software starts clean.
    fn music_quit(&mut self);
    /// Where a setting persists (`wx_music` and so on).
    fn state_path(&self, name: &str) -> PathBuf;
    /// Monotonic time, in microseconds.
    fn now_us(&self) -> i64;
    /// Wall clock time, in seconds since the epoch.
    fn wall(&self) -> i64;
    fn sleep_us(&mut self, us: i64);
    /// Lets go of the inputs (false) for an app that takes them, or takes
    /// them back (true), forgetting what happened meanwhile.
    fn inputs(&mut self, ours: bool);
    /// Whether prefetching should finish everything it starts before the
    /// frame ends, for frames that are the same on every run (the host).
    fn sync_prefetch(&self) -> bool;
}

/// The hour, minute and second at `t` in the local time zone (TZ).
// libc marks musl's 32-bit time_t deprecated (it becomes 64-bit with musl
// 1.2); either width holds the time until 2038.
#[allow(deprecated)]
pub fn local_hms(t: i64) -> (i32, i32, i32) {
    let now = t as libc::time_t;
    // SAFETY: localtime_r writes only the tm it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    (tm.tm_hour, tm.tm_min, tm.tm_sec)
}

pub fn wall_now() -> i64 {
    // SAFETY: time(NULL) only returns the time.
    unsafe { libc::time(std::ptr::null_mut()) as i64 }
}
