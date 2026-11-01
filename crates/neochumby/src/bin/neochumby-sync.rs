//! neochumby-sync: the receiver. The session runs it on a paired Chumby. It
//! keeps the stick's standby set in step with the paired neochumbyd, over the
//! paired session and nothing else, and makes the set live once it is
//! complete (neochumby_sync has how). It is a program of its own so that the
//! session stays small.
//!
//!   neochumby-sync            a round now, one a minute, and one as soon as
//!                             neochumbyd asks (`neochumbyd sync`, which the
//!                             session passes on: conf::SYNC_NOW), while this
//!                             Chumby is paired (the session's service; it
//!                             doesn't exit)
//!   neochumby-sync once       one round; exits 0 if the live set is then
//!                             the tree
//!   neochumby-sync status     the sets on the stick
//!   neochumby-sync rollback   makes the other complete set live, and holds
//!                             syncing, which would bring the tree back
//!   neochumby-sync resume     syncs again after a rollback
//!
//! A new live set runs from the next restart (`neochumbyd restart`) or boot:
//! what is running stays as it is. Its kernel and initramfs, from the next
//! boot only.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use neochumby::{apps, conf, settings};
use neochumby_sync::Timing;
use neochumby_sync::receive::{self, Outcome};
use neochumby_sync::stick::{Set, Stick};

/// Where the stock firmware mounts a USB stick, and the stick's neochumby
/// directory: the only place this writes, and only while a stick is
/// mounted there.
const MOUNT: &str = conf::STICK;
const DIR: &str = conf::STICK_TREE;
const EVERY: Duration = Duration::from_secs(60);
/// The longest wait after failures that keep coming back.
const MOST: Duration = Duration::from_secs(3600);
/// How long a round waits on neochumbyd for each message, and in all: a
/// whole set (512 MB at most) written at the stick's pace fits in the hour.
const TIMING: Timing = Timing { step: Duration::from_secs(120), round: Duration::from_secs(3600) };

/// The set the session runs from, if it runs from one on the stick.
fn running() -> Option<Set> {
    let root = apps::root().canonicalize().ok()?;
    [Set::A, Set::B].into_iter().find(|s| Path::new(DIR).join(s.name()).canonicalize().is_ok_and(|d| d == root))
}

fn stick() -> Stick {
    Stick::mounted_at(MOUNT, DIR, running())
}

fn describe(o: &Outcome) -> String {
    match o {
        Outcome::Held => "rolled back: held until `neochumby-sync resume`".into(),
        Outcome::WaitingForRestart(s) => format!("a new set is live; set {} is running, until a restart", s.name()),
        Outcome::UpToDate => "up to date".into(),
        Outcome::Flipped { to, files, received, bytes } => {
            format!("set {} is live from the next restart: {received} of {files} files sent, {bytes} bytes", to.name())
        }
    }
}

/// Says each new state once, not every minute.
#[derive(Default)]
struct Log(String);

impl Log {
    fn say(&mut self, s: String) {
        if s != self.0 {
            eprintln!("neochumby-sync: {s}");
            self.0 = s;
        }
    }
}

/// Rounds over one session with neochumbyd: one now, then one every
/// [`EVERY`], or sooner if neochumbyd asks, if `forever`, until something
/// fails. None if this Chumby is not paired. A round that goes through clears
/// `failed`.
fn rounds(forever: bool, log: &mut Log, failed: &mut Option<String>) -> io::Result<Option<Outcome>> {
    let stick = stick();
    stick.mounted()?;
    let Some(p) = conf::paired() else { return Ok(None) };
    let (mut s, _) = settings::connect(&p)?;
    s.stream().set_write_timeout(Some(TIMING.step))?;
    loop {
        // Each round checks the stick is still there before it writes.
        let o = receive::round(&mut s, &stick, TIMING)?;
        *failed = None;
        log.say(describe(&o));
        if !forever {
            return Ok(Some(o));
        }
        wait(EVERY);
    }
}

/// Waits for `d`, or until neochumbyd asks for a round.
fn wait(d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if std::fs::remove_file(conf::SYNC_NOW).is_ok() {
            return;
        }
        neochumby::sleep_ms(1000);
    }
}

fn run() {
    // Writing the stick competes with the screen for the one core.
    neochumby_sys::process::lower_priority(19);
    let mut log = Log::default();
    let mut failed: Option<String> = None;
    let mut wait = EVERY;
    loop {
        match rounds(true, &mut log, &mut failed) {
            // The session supervises this and would start it again at
            // once: it waits, and looks again in an hour.
            Ok(_) => {
                log.say("not syncing: not paired; looking again in an hour".into());
                failed = None;
                wait = MOST;
            }
            Err(e) => {
                // The same failure twice running (the same file for the same
                // reason, a full stick) will most likely come again: wait
                // twice as long each time it does, up to an hour.
                let e = e.to_string();
                wait = if failed.as_ref() == Some(&e) { (wait * 2).min(MOST) } else { EVERY };
                log.say(format!("{e}; trying again in {} s", wait.as_secs()));
                failed = Some(e);
            }
        }
        self::wait(wait);
    }
}

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("neochumby-sync: {e}");
    std::process::exit(1)
}

fn main() {
    neochumby::debug::install("neochumby-sync");
    match std::env::args().nth(1).as_deref() {
        None => run(),
        Some("once") => match rounds(false, &mut Log::default(), &mut None) {
            Ok(Some(Outcome::UpToDate | Outcome::Flipped { .. })) => {}
            Ok(Some(_)) => std::process::exit(1),
            Ok(None) => fail("not syncing: not paired"),
            Err(e) => fail(e),
        },
        Some("status") => print!("{}", stick().status()),
        Some("rollback") => match stick().rollback() {
            Ok(s) => {
                println!(
                    "set {} is live from the next restart; syncing is held until `neochumby-sync resume`",
                    s.name()
                )
            }
            Err(e) => fail(e),
        },
        Some("resume") => stick().resume().unwrap_or_else(|e| fail(e)),
        Some(_) => {
            eprintln!("usage: neochumby-sync [once|status|rollback|resume]");
            std::process::exit(2)
        }
    }
}
