//! neochumby: what the Chumby boots to, run by the boot hook from the tree.
//! The session: it starts the apps' background services, and runs one app
//! at a time in front, full-screen: first the start app (start.rs), then
//! whatever the apps ask for over their request pipe (neochumby::session),
//! going back to the start app whenever one exits (run.rs). On CHUMBY
//! MODE it stops the services and exits, and the boot carries on with the
//! Chumby's own software.
//!
//! On a paired Chumby, its output also goes to its neochumbyd, which can
//! ask it to restart, or for its screen (devlink.rs). A restart stops
//! everything as CHUMBY MODE does, then runs the tree's `neochumby` in
//! this process's place: the newest tree, deployed since boot.
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
mod devlink;
mod run;
mod start;

use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
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
    /// Set by the developer link.
    restart: Option<Arc<AtomicBool>>,
}

impl run::Device for Chumby {
    fn escaped(&mut self) -> bool {
        self.escape.swap(false, Ordering::SeqCst)
    }

    fn restart(&mut self) -> bool {
        self.restart.as_ref().is_some_and(|r| r.swap(false, Ordering::SeqCst))
    }
}

fn session() {
    // First, so that it carries everything the session logs.
    let link = devlink::Link::start();
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

    let chumby = Chumby { escape, restart: link.as_ref().map(devlink::Link::restart_flag) };
    let ended = match start::choose(&all, choice) {
        None => {
            eprintln!("neochumby: no start app (not even {})", start::HOME);
            run::Ended::ChumbyMode
        }
        Some(first) => run::Session::new(&all, &root, first, chumby, run::Timing::default()).run(),
    };

    for s in &mut services {
        let _ = s.kill();
        let _ = s.wait();
    }
    if ended == run::Ended::Restart {
        // Everything this process holds is close-on-exec (std opens files
        // and sockets so), so the new session starts with what the boot
        // hook gave this one.
        if let Some(link) = link {
            link.release();
        }
        // Just after a deploy, the NFS client may still hold the replaced
        // file's handle: the first try fails as stale, and drops it.
        let exe = root.join("neochumby");
        for _ in 0..5 {
            let e = Command::new(&exe).exec();
            eprintln!("neochumby: restart: {}: {e}", exe.display());
            neochumby::sleep_ms(1000);
        }
        eprintln!("neochumby: restart failed; back to the Chumby");
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
