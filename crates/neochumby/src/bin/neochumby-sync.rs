//! neochumby-sync: the receiver for `delivery=sync`. The session runs it
//! when this Chumby's settings say `delivery=sync`. It keeps the stick's
//! standby set in step with the paired neochumbyd, over the paired session
//! and nothing else, and makes the set live once it is complete
//! (neochumby_sync has how). It is a program of its own so that a Chumby
//! booting from a stick made by hand carries none of it.
//!
//!   neochumby-sync            a round now and one a minute, while this
//!                             Chumby is paired and its delivery is sync
//!   neochumby-sync once       one round; exits 0 if the live set is then
//!                             the tree
//!   neochumby-sync status     the sets on the stick
//!   neochumby-sync rollback   makes the other complete set live, and holds
//!                             syncing, which would bring the tree back
//!   neochumby-sync resume     syncs again after a rollback
//!
//! A new live set runs from the next boot: what is running stays as it is.

use std::io;
use std::path::Path;
use std::time::Duration;

use neochumby::{apps, conf, settings};
use neochumby_sync::Timing;
use neochumby_sync::receive::{self, Outcome};
use neochumby_sync::stick::{Set, Stick};

/// Where the stock firmware mounts a USB stick, and the stick's neochumby
/// directory: the only place this writes, and only while a stick is
/// mounted there.
const MOUNT: &str = "/mnt/usb";
const DIR: &str = "/mnt/usb/neochumby";
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
        Outcome::Refused => "neochumbyd does not sync this Chumby".into(),
        Outcome::Flipped { to, files, received, bytes } => {
            format!("set {} is live from the next boot: {received} of {files} files sent, {bytes} bytes", to.name())
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
/// [`EVERY`] if `forever`, until something fails. None if this Chumby is
/// not to sync: not paired, or its delivery is not sync. A round that goes
/// through clears `failed`.
fn rounds(forever: bool, log: &mut Log, failed: &mut Option<String>) -> io::Result<Option<Outcome>> {
    let stick = stick();
    stick.mounted()?;
    let Some(p) = conf::paired() else { return Ok(None) };
    let (mut s, settings) = settings::connect(&p)?;
    if settings::get(&settings, "delivery") != Some("sync") {
        return Ok(None);
    }
    s.tcp().set_write_timeout(Some(TIMING.step))?;
    loop {
        // Each round checks the stick is still there before it writes.
        let o = receive::round(&mut s, &stick, TIMING)?;
        *failed = None;
        log.say(describe(&o));
        if !forever || o == Outcome::Refused {
            return Ok(Some(o));
        }
        neochumby::sleep_ms(EVERY.as_millis() as u32);
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
            Ok(_) => {
                eprintln!("neochumby-sync: not syncing: not paired, or delivery is not sync");
                return;
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
        neochumby::sleep_ms(wait.as_millis() as u32);
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
            Ok(None) => fail("not syncing: not paired, or delivery is not sync"),
            Err(e) => fail(e),
        },
        Some("status") => print!("{}", stick().status()),
        Some("rollback") => match stick().rollback() {
            Ok(s) => {
                println!("set {} is live from the next boot; syncing is held until `neochumby-sync resume`", s.name())
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
