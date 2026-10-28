//! neochumby: what the Chumby boots to, run by the boot hook from the tree.
//! The session: it runs the apps' background services, restarting them
//! when they exit (services.rs), and runs one app at a time in front,
//! full-screen: first the start app (start.rs), then whatever the apps ask
//! for over their request pipe (neochumby::session), going back to the
//! start app whenever one exits (run.rs). On CHUMBY MODE it stops the
//! services and exits, and the boot carries on with the Chumby's own
//! software.
//!
//! Holding the top-left corner of the screen for three seconds, whatever
//! is running, ends the app in front and whatever it started, and opens
//! neochumby-home: the way back from an app that has gone wrong. The top
//! button is the session's, to hand over or to return to the start app
//! (button.rs).
//!
//!   neochumby           the session
//!   neochumby sensors   prints the touchscreen, squeeze and accelerometer
//!
//! Apps live beside it, in apps/<name>/ (neochumby::apps); each is its own
//! binary, paged in over NFS only when run. What it runs is described in
//! child.rs.

mod button;
mod child;
mod run;
mod services;
mod start;

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use neochumby::apps;
use neochumby::audio;
use neochumby::conf;
use neochumby::fb::{Fb, rgb};
use neochumby::input::{self, Touch};
use neochumby::settings;

/// The escape hatch: a touch held this long in this corner. The resistive
/// touchscreen reports brief releases during a steady hold; one shorter
/// than ESCAPE_GRACE does not end it.
const ESCAPE_HOLD: Duration = Duration::from_secs(3);
const ESCAPE_GRACE: Duration = Duration::from_millis(400);
const ESCAPE_CORNER: i32 = 60;

fn sensors() {
    let mut touch = input::Touchscreen::open();
    let mut squeeze = input::Squeeze::open();
    let mut accel = input::Accel::open();
    let mut tick = 0u32;
    loop {
        for t in touch.poll() {
            println!("{t:?}");
        }
        if tick.is_multiple_of(10) {
            println!("squeeze={} accel={:?}", squeeze.squeezed(), accel.read());
        }
        tick += 1;
        neochumby::sleep_ms(30);
    }
}

/// Sets the flag when a touch is held in the top-left corner for
/// [`ESCAPE_HOLD`]. Input devices give every reader every event, so this
/// takes nothing from whatever else is reading the touchscreen.
fn watch_escape(flag: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut touch = input::Touchscreen::open();
        let mut held_since: Option<Instant> = None;
        let mut released_at: Option<Instant> = None;
        loop {
            for t in touch.poll() {
                match t {
                    Touch::Down { x, y, .. } if x < ESCAPE_CORNER && y < ESCAPE_CORNER => {
                        if held_since.is_none() {
                            eprintln!("neochumby: escape: held at ({x}, {y})");
                            held_since = Some(Instant::now());
                        }
                        released_at = None;
                    }
                    Touch::Down { .. } => {
                        held_since = None;
                        released_at = None;
                    }
                    Touch::Up => {
                        if held_since.is_some() {
                            released_at.get_or_insert_with(Instant::now);
                        }
                    }
                }
            }
            if released_at.is_some_and(|t| t.elapsed() > ESCAPE_GRACE) {
                held_since = None;
                released_at = None;
            }
            if released_at.is_none() && held_since.is_some_and(|t| t.elapsed() >= ESCAPE_HOLD) {
                eprintln!("neochumby: escape");
                flag.store(true, Ordering::SeqCst);
                held_since = None;
            }
            neochumby::sleep_ms(50);
        }
    });
}

/// The top button, opened once for the whole session (button.rs).
fn open_button() -> Option<File> {
    let open = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open("/dev/switch");
    // Passed on only to an app with `button = true`.
    match open.and_then(|f| neochumby_sys::fd::set_cloexec(f.as_fd(), true).map(|()| f)) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("neochumby: /dev/switch: {e}; the top button does nothing");
            None
        }
    }
}

/// The Chumby, as the session's loop sees it.
struct Chumby {
    escape: Arc<AtomicBool>,
    boot_volume: Option<u32>,
    /// A thread is putting the volume back.
    restoring: Arc<AtomicBool>,
    button: Option<File>,
}

impl run::Device for Chumby {
    fn escaped(&mut self) -> bool {
        self.escape.swap(false, Ordering::SeqCst)
    }

    /// Mutes at once (a write to /proc). The volume is put back by a
    /// thread, since audio::set_volume runs the stock chumby_set_volume and
    /// the loop mustn't wait on it; one at a time.
    fn reset_speaker(&mut self) {
        if let Err(e) = audio::mute(true) {
            eprintln!("neochumby: muting the speaker: {e}");
        }
        let Some(v) = self.boot_volume else { return };
        if self.restoring.swap(true, Ordering::SeqCst) {
            return;
        }
        let restoring = self.restoring.clone();
        std::thread::spawn(move || {
            if let Err(e) = audio::set_volume(v) {
                eprintln!("neochumby: setting the volume: {e}");
            }
            restoring.store(false, Ordering::SeqCst);
        });
    }

    fn button(&mut self) -> Option<bool> {
        let mut b = [0u8; 1];
        match self.button.as_mut()?.read(&mut b) {
            Ok(1) => Some(b[0] != 0),
            _ => None,
        }
    }

    fn button_fd(&self) -> Option<RawFd> {
        self.button.as_ref().map(|f| f.as_raw_fd())
    }

    fn close_button(&mut self) {
        self.button = None;
    }
}

fn session() {
    let root = apps::root();
    let all = apps::all(&root);
    // The master volume as the boot set it from /psp/volume: nothing has
    // played yet.
    let boot_volume = audio::volume();

    // The server's `start`, else the local one: a name, trusted only as far
    // as the tree goes (start.rs).
    let settings = settings::load();
    let local = conf::get(conf::CONF, "start");
    let choice = match settings::get(&settings, "start") {
        Some(s) => Some(("the server's settings", s)),
        None => local.as_deref().map(|s| (conf::CONF, s)),
    };
    let escape = Arc::new(AtomicBool::new(false));
    watch_escape(escape.clone());

    match start::choose(&all, choice) {
        None => eprintln!("neochumby: no start app (not even {})", start::HOME),
        Some(first) => {
            let chumby = Chumby { escape, boot_volume, restoring: Arc::default(), button: open_button() };
            run::Session::new(&all, &root, first, chumby, run::Timing::default()).run()
        }
    }

    if let Ok(mut fb) = Fb::open() {
        fb.clear(rgb(0, 0, 0));
    }
}

fn main() {
    neochumby::debug::install("neochumby");
    match std::env::args().nth(1).as_deref() {
        None => session(),
        Some("sensors") => sensors(),
        Some(other) => {
            eprintln!("neochumby: unknown command {other}");
            std::process::exit(2);
        }
    }
}
