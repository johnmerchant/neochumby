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
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Duration;

use neochumby::{apps, conf, settings};
use neochumby_sync::receive::{self, Outcome};
use neochumby_sync::stick::{Set, Stick};

/// Where the stock firmware mounts a USB stick, and the stick's neochumby
/// directory: the only place this writes.
const MOUNT: &str = "/mnt/usb";
const DIR: &str = "/mnt/usb/neochumby";
const EVERY: Duration = Duration::from_secs(60);
/// How long a step of a round waits on neochumbyd.
const WAIT: Duration = Duration::from_secs(120);

/// The set the session runs from, if it runs from one on the stick.
fn running() -> Option<Set> {
    let root = apps::root().canonicalize().ok()?;
    [Set::A, Set::B].into_iter().find(|s| Path::new(DIR).join(s.name()).canonicalize().is_ok_and(|d| d == root))
}

/// Whether a stick is mounted: /mnt/usb is then on another device than
/// /mnt.
fn mounted() -> bool {
    match (std::fs::metadata(MOUNT), std::fs::metadata("/mnt")) {
        (Ok(usb), Ok(mnt)) => usb.dev() != mnt.dev(),
        _ => false,
    }
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
/// not to sync: not paired, or its delivery is not sync.
fn rounds(forever: bool, log: &mut Log) -> io::Result<Option<Outcome>> {
    if !mounted() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no stick at {MOUNT}")));
    }
    let Some(p) = conf::paired() else { return Ok(None) };
    let (mut s, settings) = settings::connect(&p)?;
    if settings::get(&settings, "delivery") != Some("sync") {
        return Ok(None);
    }
    s.tcp().set_write_timeout(Some(WAIT))?;
    let stick = Stick::new(DIR, running());
    loop {
        let o = receive::round(&mut s, &stick, WAIT)?;
        log.say(describe(&o));
        if !forever || o == Outcome::Refused {
            return Ok(Some(o));
        }
        neochumby::sleep_ms(EVERY.as_millis() as u32);
    }
}

fn run() {
    // Writing the stick competes with the screen for the one core.
    unsafe { libc::nice(19) };
    let mut log = Log::default();
    loop {
        match rounds(true, &mut log) {
            Ok(_) => {
                eprintln!("neochumby-sync: not syncing: not paired, or delivery is not sync");
                return;
            }
            Err(e) => log.say(format!("{e}; trying again in a minute")),
        }
        neochumby::sleep_ms(EVERY.as_millis() as u32);
    }
}

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("neochumby-sync: {e}");
    std::process::exit(1)
}

fn main() {
    neochumby::debug::install("neochumby-sync");
    let stick = || Stick::new(DIR, running());
    match std::env::args().nth(1).as_deref() {
        None => run(),
        Some("once") => match rounds(false, &mut Log::default()) {
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
