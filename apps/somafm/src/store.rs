//! What the app and its service share: files in the app's storage, a
//! number in each. The app writes them as they're tapped, and exits; the
//! service reads them twice a second and plays what they say, so the
//! music keeps going while another app is in front.
//!
//!   station   the selected station, 0-5
//!   on        1 while the music should play
//!   volume    0-100
//!   muted     1 while the music is off because of MUTE: written by the
//!             service on a `pause` line, and turned back into on=1 by
//!             `resume`. The app clears it when it's told on or off.
//!
//! The storage is the session's: a directory of the app's own on the
//! Chumby, kept across boots, shared by its exec and its service (they run
//! as one user), and named by NEOCHUMBY_STORAGE.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// The volume until one is chosen, as classic's.
pub const DEFAULT_VOLUME: u32 = 65;

/// The settings, as they are in the storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub station: usize,
    pub on: bool,
    pub volume: u32,
    pub muted: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { station: 0, on: false, volume: DEFAULT_VOLUME, muted: false }
    }
}

/// A number from `path`, or None if it's missing or not a number. Never
/// follows a link, and reads at most a few bytes.
fn read(path: &Path) -> Option<u32> {
    let mut f = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path).ok()?;
    let mut b = Vec::new();
    Read::by_ref(&mut f).take(16).read_to_end(&mut b).ok()?;
    std::str::from_utf8(&b).ok()?.trim().parse().ok()
}

/// Writes `v` to `path` whole or not at all: a new file, renamed over the
/// old, so the service never reads half a number.
fn write(path: &Path, v: u32) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    // A leftover from a run that died mid-write.
    let _ = std::fs::remove_file(&tmp);
    let mut f =
        OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&tmp)?;
    let r = f.write_all(format!("{v}\n").as_bytes()).and_then(|()| std::fs::rename(&tmp, path));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// The app's storage.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
    stations: usize,
}

impl Store {
    /// The storage in `dir`, for a list of `stations`.
    pub fn new(dir: PathBuf, stations: usize) -> Store {
        Store { dir, stations }
    }

    /// The settings now; anything missing or out of range is its default.
    pub fn load(&self) -> Settings {
        let d = Settings::default();
        let get = |name: &str| read(&self.dir.join(name));
        Settings {
            station: get("station").map(|s| s as usize).filter(|s| *s < self.stations).unwrap_or(d.station),
            on: get("on") == Some(1),
            volume: get("volume").filter(|v| *v <= 100).unwrap_or(d.volume),
            muted: get("muted") == Some(1),
        }
    }

    fn save(&self, name: &str, v: u32) {
        if let Err(e) = write(&self.dir.join(name), v) {
            eprintln!("neochumby-somafm: saving {name}: {e}");
        }
    }

    pub fn set_station(&self, s: usize) {
        self.save("station", s as u32);
    }

    /// On or off, as the app's user chose: that ends a pause from MUTE.
    pub fn set_on(&self, on: bool) {
        self.save("on", u32::from(on));
        if self.load().muted {
            self.save("muted", 0);
        }
    }

    pub fn set_volume(&self, v: u32) {
        self.save("volume", v.min(100));
    }

    /// A `pause` line (MUTE): the music goes off, remembering that MUTE did
    /// it. Nothing if it's off already.
    pub fn pause(&self) {
        if self.load().on {
            self.save("muted", 1);
            self.save("on", 0);
        }
    }

    /// A `resume` line (MUTE again): back on, if MUTE turned it off.
    pub fn resume(&self) {
        if self.load().muted {
            self.save("on", 1);
            self.save("muted", 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("somafm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn settings_round_trip_with_defaults() {
        let d = temp("store");
        let s = Store::new(d.clone(), 6);
        assert_eq!(s.load(), Settings::default());
        s.set_station(4);
        s.set_on(true);
        s.set_volume(30);
        assert_eq!(s.load(), Settings { station: 4, on: true, volume: 30, muted: false });
        // Out of range, or not a number: the default.
        std::fs::write(d.join("station"), "6\n").unwrap();
        std::fs::write(d.join("volume"), "loud\n").unwrap();
        assert_eq!(s.load(), Settings { station: 0, on: true, volume: DEFAULT_VOLUME, muted: false });
        s.set_volume(500);
        assert_eq!(s.load().volume, 100);
        assert!(!d.join("volume.tmp").exists(), "no temporary left");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_link_is_not_followed() {
        let d = temp("link");
        let s = Store::new(d.clone(), 6);
        let elsewhere = d.join("elsewhere");
        std::fs::write(&elsewhere, "1\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, d.join("on")).unwrap();
        assert!(!s.load().on, "a link reads as missing");
        // Written by rename: the link is replaced, not written through.
        s.set_on(false);
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "1\n");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn mute_pauses_and_resumes_only_what_it_paused() {
        let d = temp("mute");
        let s = Store::new(d.clone(), 6);
        // Off already: MUTE leaves it off, and its resume doesn't turn it on.
        s.pause();
        s.resume();
        assert!(!s.load().on);
        // Playing: paused, then back on.
        s.set_on(true);
        s.pause();
        assert_eq!((s.load().on, s.load().muted), (false, true));
        s.resume();
        assert_eq!((s.load().on, s.load().muted), (true, false));
        s.resume();
        assert!(s.load().on);
        // Paused, then the user chooses: MUTE's resume no longer applies.
        s.pause();
        s.set_on(false);
        assert!(!s.load().muted);
        s.resume();
        assert!(!s.load().on);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
