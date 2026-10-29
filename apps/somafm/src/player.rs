//! The service's player: plays what the storage says (store.rs) through
//! the session, and gives the speaker up to newer players.
//!
//! - **The stream** is played by the stock btplay, which the session runs
//!   for us as root: `play <url>` on the request pipe starts it, `play off`
//!   stops it. A jailed app can't run btplay itself (it isn't in the jail),
//!   and the session stops the stream if this process ends.
//! - **The speaker** is taken with `neochumby::audio::Unmuted`, which takes
//!   part in the handover (the newest player gets the speaker) and, jailed,
//!   asks the session to unmute it at our volume (`speaker <v>`).
//! - **A game's sound** (or AirPlay) asks for the speaker: the stream stops
//!   and the speaker is given up, and once the speaker is free again the
//!   stream starts again by itself.
//! - **MUTE** on the start app reaches this service as `pause` and `resume`
//!   lines (store.rs keeps what they did).
//!
//! The state machine is classic's (neochumby-classic's svc::Player), moved
//! here with the music: the stream is the session's to stop now, so it
//! needs no record of its own for a killed run. The device's side is
//! behind [`Effects`], so all of it is tested on a host.

use crate::store::{Settings, Store};

/// How often the storage is looked at, in ms.
pub const POLL_MS: u32 = 500;

/// The wait before trying again after a failure that isn't the handover's.
pub const RETRY_MS: i64 = 5000;

/// What the player does to the device.
pub trait Effects {
    /// Starts the stream (`play <url>`).
    fn start_player(&mut self, url: &str);
    /// Stops it (`play off`).
    fn stop_player(&mut self);
    /// Changes the volume while the speaker is held.
    fn set_volume(&mut self, v: u32);
    /// Takes the speaker at a volume (audio::Unmuted::at).
    fn take_speaker(&mut self, vol: u32) -> Take;
    /// Gives it back (drops the Unmuted).
    fn release_speaker(&mut self);
    /// Whether another player has asked for the speaker we hold.
    fn yield_requested(&mut self) -> bool;
    /// Whether no other live player holds the speaker or wants it.
    fn speaker_free(&mut self) -> bool;
    /// Withdraws our request for the speaker.
    fn withdraw(&mut self);
}

/// What taking the speaker did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    Taken,
    /// Another player holds it and has been asked to yield: try again soon.
    Asked,
    /// It failed otherwise: try again later.
    Failed,
}

/// The music, through the speaker handover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Music {
    Off,
    /// Wants to play, and asked for the speaker (or failed to take it):
    /// when to try again.
    Asked {
        retry_at: i64,
    },
    Playing {
        url: String,
        vol: u32,
    },
    /// Gave the speaker to another player; waiting until it is free.
    Yielded,
}

pub struct Player {
    pub music: Music,
}

impl Player {
    pub fn new(fx: &mut impl Effects) -> Player {
        // Started again while another player has the speaker (a restart
        // during a game): wait for it rather than take it.
        let music = if fx.speaker_free() { Music::Off } else { Music::Yielded };
        Player { music }
    }

    /// Stops the stream and gives the speaker back.
    fn stop(fx: &mut impl Effects) {
        fx.stop_player();
        fx.release_speaker();
    }

    /// One try at taking the speaker and starting `url`.
    fn start(fx: &mut impl Effects, url: &str, vol: u32, now: i64) -> Music {
        match fx.take_speaker(vol) {
            Take::Taken => {
                fx.start_player(url);
                Music::Playing { url: url.to_string(), vol }
            }
            Take::Asked => Music::Asked { retry_at: now },
            Take::Failed => Music::Asked { retry_at: now + RETRY_MS },
        }
    }

    /// One step towards what is wanted (a stream and its volume, or
    /// silence), at `now` (ms).
    pub fn step(&mut self, fx: &mut impl Effects, want: Option<(&str, u32)>, now: i64) {
        let music = std::mem::replace(&mut self.music, Music::Off);
        self.music = match (music, want) {
            (Music::Playing { .. }, None) => {
                Self::stop(fx);
                Music::Off
            }
            // Another player wants the speaker: stop and give it up, before
            // anything else (a new station included, which would ask it to
            // yield back).
            (Music::Playing { .. }, Some(_)) if fx.yield_requested() => {
                Self::stop(fx);
                Music::Yielded
            }
            (Music::Playing { url, vol }, Some((u, v))) if url == u => {
                if vol != v {
                    fx.set_volume(v);
                }
                Music::Playing { url, vol: v }
            }
            (Music::Playing { .. }, Some((u, v))) => {
                // Another station: stop, let go, and take it again.
                Self::stop(fx);
                Self::start(fx, u, v, now)
            }
            (Music::Yielded, Some((u, v))) => {
                if fx.speaker_free() {
                    Self::start(fx, u, v, now)
                } else {
                    Music::Yielded
                }
            }
            (Music::Asked { retry_at }, Some((u, v))) => {
                if now >= retry_at {
                    Self::start(fx, u, v, now)
                } else {
                    Music::Asked { retry_at }
                }
            }
            (Music::Off, Some((u, v))) => Self::start(fx, u, v, now),
            (Music::Asked { .. }, None) => {
                fx.withdraw();
                Music::Off
            }
            (Music::Yielded | Music::Off, None) => Music::Off,
        };
    }
}

/// What the settings ask the player for: station `station`'s stream at
/// `volume`, while it's on.
pub fn wants(s: &Settings) -> Option<(&'static str, u32)> {
    s.on.then(|| crate::stations::url(s.station)).flatten().map(|u| (u, s.volume))
}

/// A line from the session: `pause` and `resume` (MUTE) change the
/// storage; anything else is someone else's (`front <app>` is logged by
/// the caller).
pub fn control(line: &str, store: &Store) {
    match line {
        "pause" => store.pause(),
        "resume" => store.resume(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device that records what is done to it; the speaker's answers are
    /// set by the test.
    #[derive(Default)]
    struct Log {
        done: Vec<String>,
        /// What take_speaker answers (None: Taken).
        take: Option<Take>,
        free: bool,
        yield_asked: bool,
    }

    impl Effects for Log {
        fn start_player(&mut self, url: &str) {
            self.done.push(format!("play {url}"));
        }
        fn stop_player(&mut self) {
            self.done.push("play off".into());
        }
        fn set_volume(&mut self, v: u32) {
            self.done.push(format!("volume {v}"));
        }
        fn take_speaker(&mut self, vol: u32) -> Take {
            self.done.push(format!("take {vol}"));
            self.take.unwrap_or(Take::Taken)
        }
        fn release_speaker(&mut self) {
            self.done.push("release".into());
            self.yield_asked = false;
        }
        fn yield_requested(&mut self) -> bool {
            self.yield_asked
        }
        fn speaker_free(&mut self) -> bool {
            self.free
        }
        fn withdraw(&mut self) {
            self.done.push("withdraw".into());
        }
    }

    impl Log {
        fn take(&mut self) -> Vec<String> {
            std::mem::take(&mut self.done)
        }
    }

    const A: &str = "http://example.org/a";
    const B: &str = "http://example.org/b";

    #[test]
    fn music_plays_changes_and_stops() {
        let mut fx = Log { free: true, ..Log::default() };
        let mut p = Player::new(&mut fx);
        p.step(&mut fx, Some((A, 30)), 0);
        assert_eq!(fx.take(), ["take 30", "play http://example.org/a"]);
        p.step(&mut fx, Some((A, 30)), 0);
        assert!(fx.take().is_empty(), "nothing to do");
        p.step(&mut fx, Some((A, 40)), 0);
        assert_eq!(fx.take(), ["volume 40"]);
        p.step(&mut fx, Some((B, 40)), 0);
        assert_eq!(fx.take(), ["play off", "release", "take 40", "play http://example.org/b"]);
        p.step(&mut fx, None, 0);
        assert_eq!(fx.take(), ["play off", "release"]);
        assert_eq!(p.music, Music::Off);
    }

    #[test]
    fn music_yields_to_a_game_and_resumes_after_it() {
        let mut fx = Log { free: true, ..Log::default() };
        let mut p = Player::new(&mut fx);
        p.step(&mut fx, Some((A, 30)), 0);
        fx.take();
        // A game asks for the speaker: stop and give it up.
        fx.yield_asked = true;
        fx.free = false;
        p.step(&mut fx, Some((A, 30)), 0);
        assert_eq!(fx.take(), ["play off", "release"]);
        assert_eq!(p.music, Music::Yielded);
        // While the game has it, nothing, even for a new station.
        p.step(&mut fx, Some((B, 30)), 0);
        assert!(fx.take().is_empty());
        // The game is over: the new station plays.
        fx.free = true;
        p.step(&mut fx, Some((B, 30)), 0);
        assert_eq!(fx.take(), ["take 30", "play http://example.org/b"]);
    }

    #[test]
    fn a_yield_comes_before_a_station_change() {
        let mut fx = Log { free: true, ..Log::default() };
        let mut p = Player::new(&mut fx);
        p.step(&mut fx, Some((A, 30)), 0);
        fx.take();
        fx.yield_asked = true;
        fx.free = false;
        p.step(&mut fx, Some((B, 30)), 0);
        assert_eq!(fx.take(), ["play off", "release"], "no take: that would ask the game to yield back");
        assert_eq!(p.music, Music::Yielded);
    }

    #[test]
    fn asking_for_the_speaker_waits_backs_off_and_withdraws() {
        let mut fx = Log { free: true, take: Some(Take::Asked), ..Log::default() };
        let mut p = Player::new(&mut fx);
        p.step(&mut fx, Some((A, 30)), 0);
        assert_eq!(fx.take(), ["take 30"]);
        // Still asked: tried again at once.
        p.step(&mut fx, Some((A, 30)), 500);
        assert_eq!(fx.take(), ["take 30"]);
        // Another failure backs off.
        fx.take = Some(Take::Failed);
        p.step(&mut fx, Some((A, 30)), 1000);
        assert_eq!(fx.take(), ["take 30"]);
        p.step(&mut fx, Some((A, 30)), 1500);
        assert!(fx.take().is_empty(), "backing off");
        fx.take = Some(Take::Taken);
        p.step(&mut fx, Some((A, 30)), 1000 + RETRY_MS);
        assert_eq!(fx.take(), ["take 30", "play http://example.org/a"]);
        // No longer wanted while asking: the request is withdrawn.
        let mut fx = Log { free: true, take: Some(Take::Asked), ..Log::default() };
        let mut p = Player::new(&mut fx);
        p.step(&mut fx, Some((A, 30)), 0);
        fx.take();
        p.step(&mut fx, None, 0);
        assert_eq!(fx.take(), ["withdraw"]);
        // Started while another player has the speaker: wait for it.
        let mut fx = Log::default();
        let mut p = Player::new(&mut fx);
        p.step(&mut fx, Some((A, 30)), 0);
        assert!(fx.take().is_empty());
        assert_eq!(p.music, Music::Yielded);
    }

    #[test]
    fn the_storage_says_what_plays_and_mute_pauses_it() {
        let d = std::env::temp_dir().join(format!("somafm-player-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let store = Store::new(d.clone(), crate::stations::all().len());
        let mut fx = Log { free: true, ..Log::default() };
        let mut p = Player::new(&mut fx);
        let step = |p: &mut Player, fx: &mut Log| p.step(fx, wants(&store.load()), 0);
        step(&mut p, &mut fx);
        assert!(fx.take().is_empty(), "off until it's turned on");
        store.set_station(2);
        store.set_volume(40);
        store.set_on(true);
        step(&mut p, &mut fx);
        assert_eq!(fx.take(), ["take 40", "play http://ice6.somafm.com/deepspaceone-128-mp3"]);
        // MUTE: paused, then playing again.
        control("front doom", &store);
        control("pause", &store);
        step(&mut p, &mut fx);
        assert_eq!(fx.take(), ["play off", "release"]);
        control("resume", &store);
        step(&mut p, &mut fx);
        assert_eq!(fx.take(), ["take 40", "play http://ice6.somafm.com/deepspaceone-128-mp3"]);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
