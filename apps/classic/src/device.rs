//! The engine on the Chumby: the screen and inputs through the neochumby
//! crate, music through the stock firmware's btplay, the backlight through
//! /proc, settings in /psp, and other apps run from the neochumby tree.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;

use neochumby::fb::{Fb, rgb};
use neochumby::input::{self, Squeeze, Touchscreen};

use crate::engine::Engine;
use crate::launch::DirectExec;
use crate::plat::{Plat, Touch, wall_now};

/// /proc/sys/sense1/brightness, 0..65535; 12000 already looks very dark.
const NIGHT_BRIGHTNESS: u32 = 9000;

/// CLOCK_MONOTONIC in microseconds.
// The casts matter: time_t and long are 32 or 64 bits, by target and libc.
#[allow(clippy::unnecessary_cast)]
fn monotonic_us() -> i64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: writes only t.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as i64 * 1_000_000 + t.tv_nsec as i64 / 1000
}

pub struct Device {
    fb: Fb,
    touch: Touchscreen,
    /// None while an app has it: /dev/switch is opened exclusively.
    squeeze: Option<Squeeze>,
    events: VecDeque<Touch>,
    polled: bool,
    last: (i32, i32),
}

/// Runs a command line through sh in the background, its arguments as
/// $1, $2...: nothing from the manifest is ever parsed by the shell.
fn sh(script: &str, args: &[&str]) {
    let _ = Command::new("sh").arg("-c").arg(script).arg("sh").args(args).status();
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
        // Level-triggered: every read is the current state. Read once a
        // frame; the engine acts on the 0 -> 1 edge.
        self.squeeze.as_mut().map(Squeeze::squeezed)
    }

    fn backlight(&mut self, night: bool) {
        let v = if night { NIGHT_BRIGHTNESS } else { 65535 };
        let _ = std::fs::write("/proc/sys/sense1/brightness", format!("{v}\n"));
    }

    fn music(&mut self, url: Option<&str>, vol: i32) {
        match url {
            Some(u) => sh(
                "amixer -q sset Master \"$1\"%; btplay --start-daemon \"$2\" >/dev/null 2>&1 &",
                &[&vol.to_string(), u],
            ),
            None => sh("btplay --passthru=stop >/dev/null 2>&1 &", &[]),
        }
    }

    fn volume(&mut self, vol: i32) {
        sh("amixer -q sset Master \"$1\"% &", &[&vol.to_string()]);
    }

    fn music_quit(&mut self) {
        sh("btplay --passthru=quit >/dev/null 2>&1", &[]);
    }

    fn state_path(&self, name: &str) -> PathBuf {
        Path::new("/psp").join(name)
    }

    fn now_us(&self) -> i64 {
        monotonic_us()
    }

    fn wall(&self) -> i64 {
        wall_now()
    }

    fn sleep_us(&mut self, us: i64) {
        neochumby::sleep_ms((us / 1000).clamp(0, u32::MAX as i64) as u32);
    }

    fn inputs(&mut self, ours: bool) {
        if ours {
            self.squeeze = Some(Squeeze::open());
            // The touches meanwhile were the app's.
            self.touch.poll();
            self.events.clear();
        } else {
            self.squeeze = None;
        }
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
        fb,
        touch: Touchscreen::open(),
        squeeze: Some(Squeeze::open()),
        events: VecDeque::new(),
        polled: false,
        last: (0, 0),
    };
    let mut e = Engine::new(d, Box::new(DirectExec), dir);
    e.start();
    e.run();
    0
}
