//! neochumby: what the Chumby boots to, run by the boot hook from the tree.
//! The session: it starts the apps' background services and runs the home
//! screen, starting it again if it fails. When the home screen exits
//! cleanly (CHUMBY MODE), it stops the services and exits, and the boot
//! carries on with the Chumby's own software.
//!
//! The home screen is an app whose manifest has `home=`: the one named by
//! `start=<app>` in this Chumby's settings from its neochumbyd
//! (neochumby::settings, kept in /psp for boots without it), else in
//! /psp/neochumby.conf, else neochumby-home (apps/home).
//! Holding the top-left corner of the screen for three seconds, whatever
//! is running, ends the home screen and whatever it started, and brings up
//! neochumby-home: the way back from a home screen that has gone wrong. A
//! home screen that keeps failing also falls back to it.
//!
//!   neochumby                 the session
//!   neochumby sensors         prints the touchscreen, squeeze and
//!                             accelerometer
//!   neochumby check-set DIR   for the boot hook: exits 0 if the synced set
//!                             in DIR is complete (neochumby_sync)
//!
//! With `delivery=sync` in its settings, the session also runs
//! neochumby-sync from the tree, which keeps a copy of the tree on the
//! stick for the boot hook to run. A tree made for a stick has no
//! neochumby-sync, and needs none.
//!
//! Apps live beside it, in apps/<name>/ (neochumby::apps); each is its own
//! binary, paged in over NFS only when run. What it runs gets
//! NEOCHUMBY_ROOT, the tree, and HOME=/tmp: the root filesystem is
//! read-only, and so is the tree.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use neochumby::apps::{self, App};
use neochumby::conf;
use neochumby::fb::{Fb, rgb};
use neochumby::input::{self, Touch};
use neochumby::settings;
use neochumby_sys::process::{Signal, kill_group};

/// The built-in home screen, apps/<this>: the fallback.
const DEFAULT_HOME: &str = "home";
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

fn command(exe: &Path, dir: &Path, root: &Path) -> Command {
    let mut c = Command::new(exe);
    c.current_dir(dir).env("NEOCHUMBY_ROOT", root).env("HOME", "/tmp");
    c
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

/// Runs the home screen in a process group of its own, so that it and
/// whatever it started end together. Its exit, or None if the escape hatch
/// ended it.
fn run_home(home: &App, root: &Path, escape: &AtomicBool) -> Option<std::io::Result<ExitStatus>> {
    let exe = home.home.as_ref()?;
    let mut child = match command(exe, &home.dir, root).process_group(0).spawn() {
        Ok(c) => c,
        Err(e) => return Some(Err(e)),
    };
    let group = child.id();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(Ok(s)),
            Err(e) => return Some(Err(e)),
            Ok(None) => {}
        }
        if escape.swap(false, Ordering::SeqCst) {
            eprintln!("neochumby: escape: ending {} and what it started", home.name);
            let _ = kill_group(group, Signal::Term);
            let deadline = Instant::now() + Duration::from_secs(2);
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                neochumby::sleep_ms(100);
            }
            let _ = kill_group(group, Signal::Kill);
            let _ = child.wait();
            return None;
        }
        neochumby::sleep_ms(100);
    }
}

fn session() {
    let root = apps::root();
    let all = apps::all(&root);
    match conf::refresh_hook(&root) {
        Ok(true) => eprintln!("neochumby: {} brought up to date from the tree", conf::HOOK),
        Ok(false) => {}
        Err(e) => eprintln!("neochumby: {}: {e}", conf::HOOK),
    }

    // Each app's service, where what it needs of a neochumbyd is here.
    let mut services: Vec<Child> = all
        .iter()
        .filter(|a| a.runnable())
        .filter_map(|a| {
            let s = a.service.as_ref()?;
            command(s, &a.dir, &root).spawn().map_err(|e| eprintln!("neochumby: {}: {e}", s.display())).ok()
        })
        .collect();

    let is_home = |name: &str| all.iter().find(|a| a.name == name && a.home.is_some());
    let fallback = is_home(DEFAULT_HOME);
    let settings = settings::load();
    if settings::get(&settings, "delivery") == Some("sync") {
        let exe = root.join("neochumby-sync");
        match command(&exe, &root, &root).spawn() {
            Ok(c) => services.push(c),
            Err(e) => eprintln!("neochumby: delivery=sync, but {}: {e}", exe.display()),
        }
    }
    // The server's `start` is a name, trusted only as far as the tree goes:
    // it runs only if that app is here and has `home=`. Otherwise the local
    // choice, then the built-in home screen.
    let chosen = settings::get(&settings, "start")
        .and_then(is_home)
        .or_else(|| conf::get(conf::CONF, "start").and_then(|n| is_home(&n)))
        .or(fallback);
    let escape = Arc::new(AtomicBool::new(false));
    watch_escape(escape.clone());

    match chosen {
        None => eprintln!("neochumby: no home screen (no app with home=)"),
        Some(mut home) => {
            // Five failures within a minute is a home screen that cannot
            // start: the built-in one, else the Chumby back.
            let mut failures: Vec<Instant> = Vec::new();
            loop {
                match run_home(home, &root, &escape) {
                    Some(Ok(s)) if s.success() => break,
                    None => {
                        if let Some(f) = fallback {
                            home = f;
                        }
                        continue;
                    }
                    Some(r) => eprintln!("neochumby: {} ended: {r:?}", home.name),
                }
                failures.retain(|t| t.elapsed() < Duration::from_secs(60));
                failures.push(Instant::now());
                if failures.len() >= 5 {
                    match fallback {
                        Some(f) if f.name != home.name => {
                            eprintln!("neochumby: {} keeps failing; {} instead", home.name, f.name);
                            home = f;
                            failures.clear();
                        }
                        _ => {
                            eprintln!("neochumby: the home screen keeps failing; back to the Chumby");
                            break;
                        }
                    }
                }
                neochumby::sleep_ms(1000);
            }
        }
    }

    for s in &mut services {
        let _ = s.kill();
        let _ = s.wait();
    }
    if let Ok(mut fb) = Fb::open() {
        fb.clear(rgb(0, 0, 0));
    }
}

/// `neochumby check-set DIR`: whether the set in DIR (the stick's
/// neochumby/a or /b) is complete. The boot hook runs it from the set
/// itself, and runs the set only on exit 0.
fn check_set(dir: Option<String>) -> ! {
    let Some(dir) = dir else {
        eprintln!("usage: neochumby check-set DIR");
        std::process::exit(2)
    };
    match neochumby_sync::stick::check_set(Path::new(&dir), &neochumby_sync::LIMITS) {
        Ok(m) => {
            eprintln!("neochumby: {dir}: complete, {} files", m.len());
            std::process::exit(0)
        }
        Err(e) => {
            eprintln!("neochumby: {dir}: not complete: {e}");
            std::process::exit(1)
        }
    }
}

fn main() {
    neochumby::debug::install("neochumby");
    match std::env::args().nth(1).as_deref() {
        None => session(),
        Some("sensors") => sensors(),
        Some("check-set") => check_set(std::env::args().nth(2)),
        Some(other) => {
            eprintln!("neochumby: unknown command {other}");
            std::process::exit(2);
        }
    }
}
