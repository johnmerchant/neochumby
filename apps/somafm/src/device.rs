//! The two programs on the Chumby: the screen in front, and the player in
//! the background.

use std::path::Path;
use std::time::{Duration, Instant};

use neochumby::audio::{self, Unmuted};
use neochumby::fb::Fb;
use neochumby::input::{Touch, Touchscreen};
use neochumby::session::{Notice, Pipe, Request};

use crate::app::{App, Finger, Outcome, Redraw, night_at};
use crate::player::{self, Effects, POLL_MS, Player, Take};
use crate::screen::{H, Screen, W};
use crate::store::{self, Store};

fn night_now() -> bool {
    night_at(neochumby_sys::time::local_time().hour)
}

/// The app's storage, or None, said once.
fn storage(who: &str, stations: usize) -> Option<Store> {
    let s = store::dir().map(|d| Store::new(d, stations));
    if s.is_none() {
        eprintln!("{who}: no storage ({} unset): nothing is saved or played", store::ENV);
    }
    s
}

/// The screen, until DONE. The session runs it in the app's directory,
/// which holds the pictures (files/).
pub fn app() -> i32 {
    const ME: &str = "neochumby-somafm";
    neochumby::debug::install(ME);
    let screen = match Screen::load(Path::new(".")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{ME}: the pictures: {e}");
            return 1;
        }
    };
    let mut fb = match Fb::open() {
        Ok(fb) => fb,
        Err(e) => {
            eprintln!("{ME}: /dev/fb0: {e}");
            return 1;
        }
    };
    let n = screen.layout.stations;
    let store = storage(ME, n);
    let mut app = App::new(store.as_ref().map(Store::load).unwrap_or_default(), store.clone(), n, night_now());
    let mut touch = Touchscreen::open();
    // Drawn here, then put on the screen at once: no half-drawn frames.
    let mut back = vec![0u16; W * H];
    screen.draw(&app.view, &mut back);
    fb.pixels().copy_from_slice(&back);
    let t0 = Instant::now();
    let mut looked = Instant::now();
    loop {
        let now = t0.elapsed().as_millis() as i64;
        let mut redraw = Redraw::Nothing;
        for t in touch.poll() {
            let f = match t {
                Touch::Down { x, y, .. } => Finger::Down(x, y),
                Touch::Up => Finger::Up,
            };
            match app.touch(&screen, f, now) {
                Outcome::Done => return 0,
                Outcome::Stay(r) => redraw = redraw.max(r),
            }
        }
        // MUTE may have paused the music, and the night may have come.
        if looked.elapsed() >= Duration::from_secs(1) {
            if let Some(s) = &store {
                redraw = redraw.max(app.refresh(s.load()));
            }
            redraw = redraw.max(app.set_night(night_now()));
            looked = Instant::now();
        }
        match redraw {
            Redraw::All => {
                screen.draw(&app.view, &mut back);
                fb.pixels().copy_from_slice(&back);
            }
            Redraw::Slider => {
                screen.draw_slider(&app.view, &mut back);
                let r = screen.slider_rect();
                let px = fb.pixels();
                for y in r.y..r.y + r.h {
                    px[y * W + r.x..y * W + r.x + r.w].copy_from_slice(&back[y * W + r.x..y * W + r.x + r.w]);
                }
            }
            Redraw::Nothing => {}
        }
        neochumby::sleep_ms(20);
    }
}

/// The real device: the session plays the stream, and the speaker is held
/// through the handover.
struct Device {
    pipe: Option<Pipe>,
    speaker: Option<Unmuted>,
}

impl Device {
    fn ask(&mut self, line: &str) {
        let r = match &mut self.pipe {
            Some(p) => p.send(line),
            None => Err(std::io::Error::other("no session to ask")),
        };
        if let Err(e) = r {
            eprintln!("neochumby-somafm-service: asking for {}: {e}", line.trim_end());
        }
    }
}

impl Effects for Device {
    fn start_player(&mut self, url: &str) {
        self.ask(&format!("play {url}\n"));
    }
    fn stop_player(&mut self) {
        self.ask("play off\n");
    }
    fn set_volume(&mut self, v: u32) {
        // Jailed, only the session sets the volume.
        if neochumby_sys::process::is_root() {
            if let Err(e) = audio::set_volume(v) {
                eprintln!("neochumby-somafm-service: volume: {e}");
            }
        } else {
            self.ask(&Request::Speaker(Some(v)).line());
        }
    }
    fn take_speaker(&mut self, vol: u32) -> Take {
        match Unmuted::at(vol) {
            Ok(u) => {
                self.speaker = Some(u);
                Take::Taken
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Take::Asked,
            Err(e) => {
                eprintln!("neochumby-somafm-service: speaker: {e}");
                Take::Failed
            }
        }
    }
    fn release_speaker(&mut self) {
        self.speaker = None;
    }
    fn yield_requested(&mut self) -> bool {
        self.speaker.as_ref().is_some_and(Unmuted::yield_requested)
    }
    fn speaker_free(&mut self) -> bool {
        audio::free()
    }
    fn withdraw(&mut self) {
        audio::withdraw();
    }
}

/// The player, for as long as the session runs.
pub fn service() -> i32 {
    const ME: &str = "neochumby-somafm-service";
    neochumby::debug::install(ME);
    let store = storage(ME, crate::stations::all().len());
    let mut pipe = Pipe::from_env();
    let mut fx = Device { pipe: Pipe::from_env(), speaker: None };
    let mut player = Player::new(&mut fx);
    let t0 = Instant::now();
    loop {
        if let Some(p) = &mut pipe {
            let r = p.receive();
            for l in &r.lines {
                if let Some(Notice::Front(app)) = Notice::parse(l) {
                    eprintln!("{ME}: {app} in front");
                } else if let Some(s) = &store {
                    player::control(l, s);
                }
            }
            if r.closed {
                break; // the session has gone
            }
        }
        let settings = store.as_ref().map(Store::load);
        player.step(&mut fx, settings.as_ref().and_then(player::wants), t0.elapsed().as_millis() as i64);
        neochumby::sleep_ms(POLL_MS);
    }
    player.step(&mut fx, None, 0);
    neochumby::debug::dump();
    0
}
