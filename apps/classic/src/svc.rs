//! The classic service: what has to keep going while classic isn't in
//! front, decided from the same files the app writes.
//!
//! **Night and the backlight.** The schedule (wx_nightmode: auto, on, off),
//! the sun lines in the manifest, and a NIGHT tap's override (wx_night,
//! wx_night_until): state::night_now, as the app decides it. The app sets
//! the backlight while it runs; this keeps it right after, for example
//! dimming at sunset during a game. The backlight is asked of the session
//! (`brightness N`, which it takes from the start app and its service
//! only, and which is the only way a jailed app has); run by hand, without
//! a session, the service writes /proc itself.
//!
//! **Music isn't classic's.** The default player plays it (its own
//! service, SomaFM's for one): Home's MUSIC opens that app, and MUTE asks
//! the session to pause or resume it (engine.rs).
//!
//! **Nothing is left behind.** On SIGTERM (CHUMBY MODE, or the session
//! stopping it) the service puts the backlight at its day level; at CHUMBY
//! MODE the session also puts back the level the Chumby booted with.
//!
//! The device's side (the backlight) is behind [`Effects`], so all of this
//! runs, and is tested, on a host.
//!
//! It reads the session's `front` notices, and logs which app is in front.
//! It never asks the session to open classic: the top button already
//! brings classic back.

use std::path::{Path, PathBuf};

use crate::state::{self, mtime, read_int, resolve_dir};

/// How often the files are looked at, in ms.
pub const POLL_MS: u32 = 500;

/// /proc/sys/sense1/brightness for night, 0..65535 (as the app sets it):
/// 12000 already looks very dark.
pub const NIGHT_BRIGHTNESS: u32 = 9000;
/// And for day: full.
pub const DAY_BRIGHTNESS: u32 = 65535;

/// The backlight level for night or day.
pub fn brightness(night: bool) -> u32 {
    if night { NIGHT_BRIGHTNESS } else { DAY_BRIGHTNESS }
}

/// What the service does to the device: behind a trait, so that it is
/// tested on a host.
pub trait Effects {
    fn backlight(&mut self, night: bool);
}

/// The backlight as the service last set it.
#[derive(Default)]
pub struct Backlight {
    pub night: Option<bool>,
}

impl Backlight {
    /// Sets the backlight for `night`, if it isn't already.
    pub fn step(&mut self, fx: &mut impl Effects, night: bool) {
        if self.night != Some(night) {
            fx.backlight(night);
            self.night = Some(night);
        }
    }

    /// On SIGTERM: the day backlight.
    pub fn shutdown(&mut self, fx: &mut impl Effects) {
        fx.backlight(false);
        self.night = Some(false);
    }
}

/// The manifest's sun lines, read again when the set changes.
#[derive(Default)]
pub struct Feed {
    dir: PathBuf,
    mtime: i64,
    len: u64,
    pub sun: Vec<(i64, i64)>,
}

impl Feed {
    /// Re-reads the manifest if `current` points elsewhere or it changed.
    pub fn refresh(&mut self, base: &Path) {
        let dir = resolve_dir(base);
        let path = dir.join("manifest.txt");
        let Ok(m) = std::fs::metadata(&path) else { return };
        // The size too: a second push within the same second has the same
        // mtime.
        if dir == self.dir && mtime(&m) == self.mtime && m.len() == self.len {
            return;
        }
        // Read with a cap: the file can grow after the look.
        if let Some(text) = state::read_nofollow(&path, crate::manifest::MAX_MANIFEST_BYTES) {
            self.sun = state::feed(&text);
            self.dir = dir;
            self.mtime = mtime(&m);
            self.len = m.len();
        }
    }
}

/// Whether the settings in `st` (the /psp directory) and the feed ask for
/// night at `now`.
pub fn night(st: &Path, feed: &Feed, now: i64) -> bool {
    let mode = read_int(&st.join("wx_nightmode"), 0).clamp(0, 2);
    let last = read_int(&st.join("wx_night"), 0) == 1;
    let until = state::read_time(&st.join("wx_night_until"));
    state::night_now(mode, &feed.sun, now, until, last)
}

#[cfg(target_arch = "arm")]
pub mod device {
    //! The service on the Chumby.

    use std::path::Path;
    use std::sync::atomic::Ordering;

    use neochumby::session::{Notice, Pipe};

    use super::{Backlight, Effects, Feed, POLL_MS, brightness, night};

    /// The real device: the backlight, through the session if it started
    /// the service.
    struct Device {
        pipe: Option<Pipe>,
    }

    impl Effects for Device {
        fn backlight(&mut self, night: bool) {
            let v = brightness(night);
            match &mut self.pipe {
                Some(p) => {
                    if let Err(e) = p.brightness(v) {
                        eprintln!("neochumby-classic-service: brightness: {e}");
                    }
                }
                None => {
                    let _ = std::fs::write("/proc/sys/sense1/brightness", format!("{v}\n"));
                }
            }
        }
    }

    pub fn run() -> i32 {
        neochumby::debug::install("neochumby-classic-service");
        // SIGTERM only sets this: the loop ends, and the service tidies up.
        let term = neochumby_sys::signal::catch_term();
        let base = crate::picture_dir(None);
        let st = Path::new("/psp");
        let mut fx = Device { pipe: Pipe::from_env() };
        let mut feed = Feed::default();
        let mut light = Backlight::default();
        while !term.load(Ordering::Relaxed) {
            if let Some(p) = &mut fx.pipe {
                let r = p.receive();
                for l in &r.lines {
                    if let Some(Notice::Front(app)) = Notice::parse(l) {
                        eprintln!("neochumby-classic-service: {app} in front");
                    }
                }
                if r.closed {
                    break; // the session has gone
                }
            }
            feed.refresh(&base);
            light.step(&mut fx, night(st, &feed, crate::plat::wall_now()));
            neochumby::sleep_ms(POLL_MS);
        }
        light.shutdown(&mut fx);
        neochumby::debug::dump();
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device that records what is done to it.
    #[derive(Default)]
    struct Log {
        done: Vec<String>,
    }

    impl Effects for Log {
        fn backlight(&mut self, night: bool) {
            self.done.push(format!("backlight {}", brightness(night)));
        }
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("classic-svc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_backlight_is_set_on_a_change_and_day_on_sigterm() {
        let mut fx = Log::default();
        let mut b = Backlight::default();
        b.step(&mut fx, false);
        b.step(&mut fx, false);
        b.step(&mut fx, true);
        b.step(&mut fx, true);
        b.shutdown(&mut fx);
        assert_eq!(fx.done, ["backlight 65535", "backlight 9000", "backlight 65535"]);
    }

    #[test]
    fn decides_night_from_the_files() {
        let pics = dir("pics");
        // An old feeder's station lines are skipped: the radio isn't
        // classic's any more.
        std::fs::write(pics.join("manifest.txt"), "sun 100 200\nstation 0 a http://example.org/a\n").unwrap();
        let st = dir("psp");
        let mut feed = Feed::default();
        feed.refresh(&pics);
        assert_eq!(feed.sun, vec![(100, 200)]);
        // Nothing set: auto, by the sun.
        assert!(!night(&st, &feed, 150));
        assert!(night(&st, &feed, 250));
        // Always on; a NIGHT tap's override wins until its end.
        state::save_int(&st.join("wx_nightmode"), 1);
        assert!(night(&st, &feed, 150));
        state::save_int(&st.join("wx_night"), 0);
        state::save_time(&st.join("wx_night_until"), 180);
        assert!(!night(&st, &feed, 150));
        assert!(night(&st, &feed, 180));
        // A new manifest is read again, even within the same second (its
        // size differs).
        std::fs::write(pics.join("manifest.txt"), "sun 300 400\n").unwrap();
        feed.refresh(&pics);
        assert_eq!(feed.sun, vec![(300, 400)]);
        std::fs::remove_dir_all(&pics).unwrap();
        std::fs::remove_dir_all(&st).unwrap();
    }
}
