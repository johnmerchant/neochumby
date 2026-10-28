//! neochumby: what the Chumby boots to, run by the boot hook from the tree.
//! The session: it starts the apps' background services, and runs one app
//! at a time in front, full-screen: first the start app (start.rs), then
//! whatever the apps ask for over their request pipe (neochumby::session),
//! going back to the start app whenever one exits (run.rs). On CHUMBY
//! MODE it stops the services and exits, and the boot carries on with the
//! Chumby's own software.
//!
//! Holding the top-left corner of the screen for three seconds, whatever
//! is running, ends the app in front and whatever it started, and opens
//! neochumby-home: the way back from an app that has gone wrong.
//!
//!   neochumby           the session
//!   neochumby sensors   prints the touchscreen, squeeze and accelerometer
//!
//! Apps live beside it, in apps/<name>/ (neochumby::apps); each is its own
//! binary, paged in over NFS only when run. What it runs is described in
//! child.rs.

mod child;
mod run;
mod start;

use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use neochumby::apps;
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

/// The Chumby, as the session's loop sees it.
struct Chumby {
    escape: Arc<AtomicBool>,
}

impl run::Device for Chumby {
    fn escaped(&mut self) -> bool {
        self.escape.swap(false, Ordering::SeqCst)
    }
}

fn session() {
    let root = apps::root();
    let all = apps::all(&root);

    // Each app's service, where what it needs of a neochumbyd is here.
    let mut services: Vec<Child> = all
        .iter()
        .filter(|a| a.runnable())
        .filter_map(|a| {
            let s = a.service.as_ref()?;
            child::command(s, &a.dir, &root).spawn().map_err(|e| eprintln!("neochumby: {}: {e}", s.display())).ok()
        })
        .collect();

    // The server's `start` is a name, trusted only as far as the tree goes
    // (start.rs). Otherwise the local choice, then neochumby-home.
    let settings = settings::load();
    let local = conf::get(conf::CONF, "start");
    let mut choices = Vec::new();
    if let Some(s) = settings::get(&settings, "start") {
        choices.push(("the server's settings", s));
    }
    if let Some(s) = local.as_deref() {
        choices.push((conf::CONF, s));
    }
    let escape = Arc::new(AtomicBool::new(false));
    watch_escape(escape.clone());

    match start::choose(&all, &choices) {
        None => eprintln!("neochumby: no start app (not even {})", start::HOME),
        Some(first) => run::Session::new(&all, &root, first, Chumby { escape }, run::Timing::default()).run(),
    }

    for s in &mut services {
        let _ = s.kill();
        let _ = s.wait();
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
