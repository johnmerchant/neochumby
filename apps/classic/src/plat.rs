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
    /// None if this machine has none to read. On the Chumby the session
    /// owns the button (a press comes as a `button` line, session.rs); the
    /// host renderer scripts presses here.
    fn button(&mut self) -> Option<bool>;
    fn backlight(&mut self, night: bool);
    /// Where a setting persists (`wx_night` and so on).
    fn state_path(&self, name: &str) -> PathBuf;
    /// Where something lasts only until the Chumby restarts (`player`, what
    /// MUTE last asked the session); None if there's nowhere of our own.
    fn run_path(&self, name: &str) -> Option<PathBuf>;
    /// Monotonic time, in microseconds.
    fn now_us(&self) -> i64;
    /// Wall clock time, in seconds since the epoch.
    fn wall(&self) -> i64;
    fn sleep_us(&mut self, us: i64);
    /// A few lines of text on an otherwise blank screen (the waiting screen).
    fn message(&mut self, lines: &[&str]);
    /// A live device value (values.rs); None if it is missing.
    fn device_value(&mut self, key: crate::values::Key) -> Option<String>;
    /// Whether prefetching should finish everything it starts before the
    /// frame ends, for frames that are the same on every run (the host).
    fn sync_prefetch(&self) -> bool;
}

/// The hour, minute and second at `t` in the local time zone (TZ).
pub fn local_hms(t: i64) -> (i32, i32, i32) {
    let l = neochumby_sys::time::local_time_at(t);
    (l.hour as i32, l.minute as i32, l.second as i32)
}

/// The wall clock, in seconds since the epoch.
pub fn wall_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
