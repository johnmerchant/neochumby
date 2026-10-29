//! The app in front: what a touch does to the screen and the storage. It
//! behaves as classic's Music screen did:
//! - a station's row selects it and turns the music on;
//! - play/pause turns the music on or off;
//! - skip moves to the next station (and plays it, if the music is on);
//! - dragging the slider sets the volume, saved when the finger lifts;
//! - DONE leaves: the session opens the start app again.
//!
//! The app only writes the storage; the service hears it and plays.

use crate::screen::{Hit, Screen, View};
use crate::store::{Settings, Store};

/// One tap can arrive as two on a Chumby's touchscreen: a second release
/// within this many ms of the last is ignored (classic's TAP_ECHO_MS).
pub const TAP_ECHO_MS: i64 = 350;

/// What the touchscreen reported (neochumby::input::Touch, without the
/// device).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finger {
    /// Down, or moved, at (x, y).
    Down(i32, i32),
    Up,
}

/// What needs drawing again: the most of several is enough.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Redraw {
    Nothing,
    Slider,
    All,
}

/// What a touch led to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay(Redraw),
    /// DONE: exit.
    Done,
}

pub struct App {
    pub view: View,
    store: Option<Store>,
    stations: usize,
    /// A finger is down.
    down: bool,
    /// It came down on the slider: it sets the volume until it lifts.
    dragging: bool,
    /// What it's on, if it isn't dragging.
    pressed: Option<Hit>,
    last_tap: i64,
}

impl App {
    /// Showing `s`, saving to `store` (None: nowhere, and the service
    /// can't hear it).
    pub fn new(s: Settings, store: Option<Store>, stations: usize, night: bool) -> App {
        let view = View { station: s.station, on: s.on, volume: s.volume, night };
        App { view, store, stations, down: false, dragging: false, pressed: None, last_tap: i64::MIN / 2 }
    }

    /// A touch at `now` (ms).
    pub fn touch(&mut self, screen: &Screen, f: Finger, now: i64) -> Outcome {
        match f {
            Finger::Down(x, y) => {
                if !self.down {
                    self.down = true;
                    self.dragging = screen.on_slider(x, y);
                }
                if self.dragging {
                    let v = screen.volume_at(x);
                    if v != self.view.volume {
                        self.view.volume = v;
                        return Outcome::Stay(Redraw::Slider);
                    }
                } else {
                    self.pressed = screen.hit(x, y);
                }
                Outcome::Stay(Redraw::Nothing)
            }
            Finger::Up => {
                self.down = false;
                if std::mem::take(&mut self.dragging) {
                    self.save(|s, v| s.set_volume(v.volume));
                    return Outcome::Stay(Redraw::Nothing);
                }
                let Some(hit) = self.pressed.take() else { return Outcome::Stay(Redraw::Nothing) };
                if now - self.last_tap < TAP_ECHO_MS {
                    return Outcome::Stay(Redraw::Nothing);
                }
                self.last_tap = now;
                self.act(hit)
            }
        }
    }

    fn act(&mut self, hit: Hit) -> Outcome {
        let v = &mut self.view;
        match hit {
            Hit::Done => return Outcome::Done,
            Hit::Station(i) => {
                v.station = i.min(self.stations - 1);
                v.on = true;
                self.save(|s, v| {
                    s.set_station(v.station);
                    s.set_on(true);
                });
            }
            Hit::Play => {
                v.on = !v.on;
                self.save(|s, v| s.set_on(v.on));
            }
            Hit::Skip => {
                v.station = (v.station + 1) % self.stations;
                self.save(|s, v| s.set_station(v.station));
            }
        }
        Outcome::Stay(Redraw::All)
    }

    fn save(&self, f: impl FnOnce(&Store, &View)) {
        if let Some(s) = &self.store {
            f(s, &self.view);
        }
    }

    /// The storage as it is now, which the service may have changed (MUTE
    /// pauses): shown, unless a finger is down.
    pub fn refresh(&mut self, s: Settings) -> Redraw {
        let v = View { station: s.station, on: s.on, volume: s.volume, ..self.view };
        if self.down || v == self.view {
            return Redraw::Nothing;
        }
        self.view = v;
        Redraw::All
    }

    /// Day or night, as the clock says.
    pub fn set_night(&mut self, night: bool) -> Redraw {
        if night == self.view.night {
            return Redraw::Nothing;
        }
        self.view.night = night;
        Redraw::All
    }
}

/// Whether the dark screen shows at `hour` (0-23, local time): from 19:00
/// until 07:00, as the clock in the Chumby's time zone (TZ) says.
pub fn night_at(hour: u8) -> bool {
    !(7..19).contains(&hour)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn screen() -> Screen {
        Screen::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("files")).unwrap()
    }

    fn tap(app: &mut App, s: &Screen, x: i32, y: i32, now: i64) -> Outcome {
        app.touch(s, Finger::Down(x, y), now);
        app.touch(s, Finger::Up, now)
    }

    #[test]
    fn taps_select_play_skip_and_leave() {
        let s = screen();
        let d = std::env::temp_dir().join(format!("somafm-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let store = Store::new(d.clone(), 6);
        let mut a = App::new(store.load(), Some(store.clone()), 6, false);
        // A station: selected, and on.
        assert_eq!(tap(&mut a, &s, 30, 43 + 27 * 2 + 5, 1000), Outcome::Stay(Redraw::All));
        assert_eq!((a.view.station, a.view.on), (2, true));
        assert_eq!(store.load(), Settings { station: 2, on: true, ..Settings::default() });
        // An echo of that tap does nothing.
        assert_eq!(tap(&mut a, &s, 20, 225, 1100), Outcome::Stay(Redraw::Nothing));
        assert!(a.view.on);
        // Play/pause: off.
        tap(&mut a, &s, 20, 225, 2000);
        assert!(!a.view.on && !store.load().on);
        // Skip, off the end and round.
        for t in [3000, 4000, 5000, 6000] {
            tap(&mut a, &s, 52, 225, t);
        }
        assert_eq!((a.view.station, store.load().station), (0, 0));
        // Moving off a button before lifting: nothing.
        a.touch(&s, Finger::Down(20, 225), 9000);
        a.touch(&s, Finger::Down(240, 100), 9000);
        assert_eq!(a.touch(&s, Finger::Up, 9000), Outcome::Stay(Redraw::Nothing));
        // DONE.
        assert_eq!(tap(&mut a, &s, 290, 225, 10_000), Outcome::Done);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_slider_drags_and_saves_on_lifting() {
        let s = screen();
        let d = std::env::temp_dir().join(format!("somafm-slider-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let store = Store::new(d.clone(), 6);
        let mut a = App::new(store.load(), Some(store.clone()), 6, false);
        assert_eq!(a.touch(&s, Finger::Down(70 + 10, 225), 0), Outcome::Stay(Redraw::Slider));
        assert_eq!(a.view.volume, 0);
        // Dragged past the end, and below the slider: still the slider's.
        assert_eq!(a.touch(&s, Finger::Down(300, 239), 0), Outcome::Stay(Redraw::Slider));
        assert_eq!(a.view.volume, 100);
        assert_eq!(store.load().volume, crate::store::DEFAULT_VOLUME, "not saved until it lifts");
        a.touch(&s, Finger::Down(70 + 10 + 83, 225), 0);
        assert_eq!(a.touch(&s, Finger::Up, 0), Outcome::Stay(Redraw::Nothing));
        assert_eq!(store.load().volume, 50);
        // A drag that starts on a button isn't the slider's.
        a.touch(&s, Finger::Down(52, 225), 5000);
        assert_eq!(a.touch(&s, Finger::Down(150, 225), 5000), Outcome::Stay(Redraw::Nothing));
        assert_eq!(a.view.volume, 50);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_storage_and_the_clock_change_what_shows() {
        let mut a = App::new(Settings::default(), None, 6, false);
        let playing = Settings { on: true, ..Settings::default() };
        assert_eq!(a.refresh(playing), Redraw::All);
        assert_eq!(a.refresh(playing), Redraw::Nothing);
        assert!(a.view.on);
        assert_eq!(a.set_night(true), Redraw::All);
        assert_eq!(a.set_night(true), Redraw::Nothing);
        assert!(night_at(19) && night_at(0) && night_at(6));
        assert!(!night_at(7) && !night_at(12) && !night_at(18));
        // Without storage, taps still show.
        let s = screen();
        tap(&mut a, &s, 20, 225, 0);
        assert!(!a.view.on);
    }
}
