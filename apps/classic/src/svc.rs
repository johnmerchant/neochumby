//! The classic service: what has to keep going while classic isn't in
//! front, decided from the same files the app writes.
//!
//! - **Night and the backlight.** The schedule (wx_nightmode: auto, on,
//!   off), the sun lines in the manifest, and a NIGHT tap's override
//!   (wx_night, wx_night_until): state::night_now, as the app decides it.
//!   The app sets the backlight while it runs; this keeps it right after,
//!   for example dimming at sunset during a game.
//! - **Music.** wx_music, wx_station and wx_volume, and the stations in the
//!   manifest: btplay plays while the speaker is held unmuted
//!   (`neochumby::audio::Unmuted`). The speaker goes to the newest player:
//!   when another app asks for it (a game's sound), the music stops and the
//!   speaker is given up; it starts again once the speaker is free.
//!
//! It reads the session's `front` notices, and logs which app is in front.
//! It never asks the session to open classic: the top button already
//! brings classic back.

use std::path::{Path, PathBuf};

use crate::state::{self, mtime, read_int, resolve_dir};

/// How often the files are looked at, in ms.
pub const POLL_MS: u32 = 500;

/// What the service should be doing now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wants {
    pub night: bool,
    /// The stream and volume (0-100) to play, or None for silence.
    pub music: Option<(String, u32)>,
}

/// The manifest's sun and station lines, read again when the set changes.
#[derive(Default)]
pub struct Feed {
    dir: PathBuf,
    mtime: i64,
    pub sun: Vec<(i64, i64)>,
    pub stations: Vec<String>,
}

impl Feed {
    /// Re-reads the manifest if `current` points elsewhere or it changed.
    pub fn refresh(&mut self, base: &Path) {
        let dir = resolve_dir(base);
        let path = dir.join("manifest.txt");
        let Ok(m) = std::fs::metadata(&path) else { return };
        if dir == self.dir && mtime(&m) == self.mtime {
            return;
        }
        if m.len() > crate::manifest::MAX_MANIFEST_BYTES {
            return;
        }
        if let Ok(text) = std::fs::read(&path) {
            (self.sun, self.stations) = state::feed(&text);
            self.dir = dir;
            self.mtime = mtime(&m);
        }
    }
}

/// What the settings in `st` (the /psp directory) and the feed ask for at
/// `now`.
pub fn wants(st: &Path, feed: &Feed, now: i64) -> Wants {
    let mode = read_int(&st.join("wx_nightmode"), 0).clamp(0, 2);
    let last = read_int(&st.join("wx_night"), 0) == 1;
    let until = state::read_time(&st.join("wx_night_until"));
    let night = state::night_now(mode, &feed.sun, now, until, last);
    let music = (read_int(&st.join("wx_music"), 0) == 1)
        .then(|| {
            let station = read_int(&st.join("wx_station"), 0);
            let url = feed.stations.get(usize::try_from(station).ok()?).filter(|u| !u.is_empty())?;
            let vol = read_int(&st.join("wx_volume"), 65);
            let vol = if (0..=100).contains(&vol) { vol } else { 65 };
            Some((url.clone(), vol as u32))
        })
        .flatten();
    Wants { night, music }
}

#[cfg(target_arch = "arm")]
pub mod device {
    //! The service on the Chumby.

    use std::path::Path;
    use std::process::Command;

    use neochumby::audio::{self, Unmuted};
    use neochumby::session::{Notice, Pipe};

    use super::{Feed, POLL_MS, Wants, wants};

    /// /proc/sys/sense1/brightness, 0..65535 (as the app sets it).
    const NIGHT_BRIGHTNESS: u32 = 9000;

    fn backlight(night: bool) {
        let v = if night { NIGHT_BRIGHTNESS } else { 65535 };
        let _ = std::fs::write("/proc/sys/sense1/brightness", format!("{v}\n"));
    }

    /// Runs a command line through sh, its arguments as $1...: a URL from
    /// the manifest is never parsed by the shell.
    fn sh(script: &str, args: &[&str]) {
        let _ = Command::new("sh").arg("-c").arg(script).arg("sh").args(args).status();
    }

    /// What is playing: the stream, its volume, and the speaker held.
    struct Playing {
        url: String,
        vol: u32,
        unmuted: Unmuted,
    }

    /// The music, through the speaker handover.
    enum Music {
        Off,
        /// Asked for the speaker; its holder is giving it up.
        Asked,
        Playing(Playing),
        /// Gave the speaker to another player; waiting for it to be free.
        Yielded,
    }

    /// Takes the speaker and starts the stream; Asked while another player
    /// is still giving the speaker up.
    fn start(url: &str, vol: u32) -> Music {
        match Unmuted::at(vol) {
            Ok(u) => {
                // The stock player's daemon, as the C engine started it.
                sh("btplay --start-daemon \"$1\" >/dev/null 2>&1 &", &[url]);
                Music::Playing(Playing { url: url.to_string(), vol, unmuted: u })
            }
            Err(e) => {
                if e.kind() != std::io::ErrorKind::WouldBlock {
                    eprintln!("neochumby-classic-service: speaker: {e}");
                }
                Music::Asked
            }
        }
    }

    fn stop() {
        sh("btplay --passthru=stop >/dev/null 2>&1", &[]);
    }

    /// One step towards what is wanted.
    fn step(music: Music, want: Option<(String, u32)>) -> Music {
        match (music, want) {
            (Music::Playing(p), Some((url, vol))) if p.url == url => {
                if p.unmuted.yield_requested() {
                    // Another player wants the speaker: stop, then give it up.
                    stop();
                    drop(p);
                    return Music::Yielded;
                }
                let mut p = p;
                if p.vol != vol {
                    if let Err(e) = audio::set_volume(vol) {
                        eprintln!("neochumby-classic-service: volume: {e}");
                    }
                    p.vol = vol;
                }
                Music::Playing(p)
            }
            (Music::Playing(p), Some((url, vol))) => {
                // Another station: stop, let go, and take it again.
                stop();
                drop(p);
                start(&url, vol)
            }
            (Music::Yielded, Some((url, vol))) => {
                if audio::free() {
                    start(&url, vol)
                } else {
                    Music::Yielded
                }
            }
            (Music::Off | Music::Asked, Some((url, vol))) => start(&url, vol),
            (Music::Playing(p), None) => {
                stop();
                drop(p);
                Music::Off
            }
            (Music::Asked, None) => {
                audio::withdraw();
                Music::Off
            }
            (Music::Yielded | Music::Off, None) => Music::Off,
        }
    }

    pub fn run() -> i32 {
        neochumby::debug::install("neochumby-classic-service");
        let base = crate::picture_dir(None);
        let st = Path::new("/psp");
        let mut pipe = Pipe::from_env();
        let mut feed = Feed::default();
        let mut night: Option<bool> = None;
        // Started again while another player has the speaker (a restart
        // during a game): wait for it rather than take it.
        let mut music = if audio::free() { Music::Off } else { Music::Yielded };
        loop {
            if let Some(p) = &mut pipe {
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
            let Wants { night: n, music: want_music } = wants(st, &feed, crate::plat::wall_now());
            if night != Some(n) {
                backlight(n);
                night = Some(n);
            }
            music = step(music, want_music);
            neochumby::sleep_ms(POLL_MS);
        }
        drop(step(music, None));
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("classic-svc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn decides_night_and_music_from_the_files() {
        let pics = dir("pics");
        std::fs::write(pics.join("manifest.txt"), "sun 100 200\nstation 0 a http://a\nstation 1 b http://b\n").unwrap();
        let st = dir("psp");
        let mut feed = Feed::default();
        feed.refresh(&pics);
        assert_eq!(feed.sun, vec![(100, 200)]);
        // Nothing set: auto, by the sun; no music.
        assert_eq!(wants(&st, &feed, 150), Wants { night: false, music: None });
        assert!(wants(&st, &feed, 250).night);
        // Always on; a NIGHT tap's override wins until its end.
        state::save_int(&st.join("wx_nightmode"), 1);
        assert!(wants(&st, &feed, 150).night);
        state::save_int(&st.join("wx_night"), 0);
        state::save_time(&st.join("wx_night_until"), 180);
        assert!(!wants(&st, &feed, 150).night);
        assert!(wants(&st, &feed, 180).night);
        // Music: station 1 at the saved volume; a bad volume is 65; a
        // station without a URL plays nothing.
        state::save_int(&st.join("wx_music"), 1);
        state::save_int(&st.join("wx_station"), 1);
        state::save_int(&st.join("wx_volume"), 30);
        assert_eq!(wants(&st, &feed, 150).music, Some(("http://b".into(), 30)));
        state::save_int(&st.join("wx_volume"), 900);
        assert_eq!(wants(&st, &feed, 150).music, Some(("http://b".into(), 65)));
        state::save_int(&st.join("wx_station"), 5);
        assert_eq!(wants(&st, &feed, 150).music, None);
        // A new manifest is read again.
        std::fs::write(pics.join("manifest.txt"), "sun 300 400\n").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(pics.join("manifest.txt")).unwrap().set_modified(later).unwrap();
        feed.refresh(&pics);
        assert_eq!(feed.sun, vec![(300, 400)]);
        std::fs::remove_dir_all(&pics).unwrap();
        std::fs::remove_dir_all(&st).unwrap();
    }
}
