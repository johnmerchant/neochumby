//! The engine on the Chumby: the screen and touch through the neochumby
//! crate, settings in /psp, and the session's pipe (opening apps, CHUMBY
//! MODE, the top button, MUTE's `music toggle`, and the backlight, which
//! it sets as `brightness N`; without a session, through /proc).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use neochumby::fb::{Fb, rgb};
use neochumby::input::{self, Touchscreen};
use neochumby::session::{Music, Notice, Pipe};

use crate::engine::Engine;
use crate::plat::{Plat, Touch, wall_now};
use crate::session::{Ask, Heard, Session};

pub struct Device {
    /// The session's pipe, for the backlight only (the engine has its own
    /// copy for the rest); None without a session.
    light: Option<Pipe>,
    fb: Fb,
    touch: Touchscreen,
    events: VecDeque<Touch>,
    polled: bool,
    last: (i32, i32),
    /// The monotonic clock's origin.
    t0: std::time::Instant,
}

impl Plat for Device {
    fn fb(&mut self) -> &mut [u16] {
        self.fb.pixels()
    }

    fn present(&mut self) -> bool {
        // The framebuffer is the screen.
        self.polled = false;
        true
    }

    fn touch(&mut self) -> Option<Touch> {
        // The touchscreen once a frame, so a held finger can't keep a frame
        // from ending.
        if self.events.is_empty() && !self.polled {
            self.polled = true;
            for t in self.touch.poll() {
                self.events.push_back(match t {
                    input::Touch::Down { x, y, .. } => {
                        self.last = (x, y);
                        Touch::Move(x, y)
                    }
                    input::Touch::Up => Touch::Up(self.last.0, self.last.1),
                });
            }
        }
        self.events.pop_front()
    }

    fn button(&mut self) -> Option<bool> {
        // The session owns the top button; a press arrives on its pipe.
        None
    }

    fn backlight(&mut self, night: bool) {
        let v = crate::svc::brightness(night);
        match &mut self.light {
            Some(p) => {
                if let Err(e) = p.brightness(v) {
                    eprintln!("neochumby-classic: brightness: {e}");
                }
            }
            None => {
                let _ = std::fs::write("/proc/sys/sense1/brightness", format!("{v}\n"));
            }
        }
    }

    fn state_path(&self, name: &str) -> PathBuf {
        Path::new("/psp").join(name)
    }

    fn run_path(&self, name: &str) -> Option<PathBuf> {
        crate::state::run_dir().map(|d| d.join(name))
    }

    fn now_us(&self) -> i64 {
        // Monotonic (std's Instant is CLOCK_MONOTONIC), from an origin
        // far enough back that times are never negative.
        self.t0.elapsed().as_micros() as i64 + 1_000_000_000
    }

    fn wall(&self) -> i64 {
        wall_now()
    }

    fn sleep_us(&mut self, us: i64) {
        neochumby::sleep_ms((us / 1000).clamp(0, u32::MAX as i64) as u32);
    }

    fn message(&mut self, lines: &[&str]) {
        self.fb.clear(rgb(0, 0, 0));
        let mut y = 96;
        for (i, l) in lines.iter().enumerate() {
            // The first line large; the rest (a path) small, wrapped.
            let (scale, per) = if i == 0 { (2, 19) } else { (1, 38) };
            let chars: Vec<char> = l.chars().collect();
            for part in chars.chunks(per) {
                let s: String = part.iter().collect();
                self.fb.text(8, y, &s, scale, rgb(220, 220, 220));
                y += 10 * scale;
            }
        }
    }

    fn device_value(&mut self, key: crate::values::Key) -> Option<String> {
        crate::values::read(Path::new("/"), key)
    }

    fn sync_prefetch(&self) -> bool {
        false
    }
}

pub fn run(dir: PathBuf) -> i32 {
    neochumby::debug::install("neochumby-classic");
    let fb = match Fb::open() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("neochumby-classic: /dev/fb0: {e}");
            return 1;
        }
    };
    let d = Device {
        light: Pipe::from_env(),
        fb,
        touch: Touchscreen::open(),
        events: VecDeque::new(),
        polled: false,
        last: (0, 0),
        t0: std::time::Instant::now(),
    };
    let mut e = Engine::new(d, dir);
    if let Some(p) = Pipe::from_env() {
        let button = neochumby::session::reason().as_deref() == Some("button");
        e.set_session(Box::new(SessionPipe(p)), button);
    } else {
        eprintln!("neochumby-classic: not started by the session: no apps can be opened");
    }
    e.start();
    e.run();
    0
}

/// The session's pipe, as the engine uses it.
struct SessionPipe(Pipe);

impl Session for SessionPipe {
    fn heard(&mut self) -> Vec<Heard> {
        // Without waiting; lines this end doesn't know are ignored.
        self.0
            .receive()
            .lines
            .iter()
            .filter_map(|l| match Notice::parse(l) {
                Some(Notice::Button) => Some(Heard::Button),
                _ => None,
            })
            .collect()
    }

    fn ask(&mut self, a: &Ask) -> bool {
        match a {
            Ask::Open(app) => self.0.open(app),
            Ask::ChumbyMode => self.0.chumby_mode(),
            Ask::MusicToggle => self.0.music(Music::Toggle),
        }
        .is_ok()
    }
}
