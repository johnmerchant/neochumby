//! This Chumby's settings as its owner chose them on the neochumbyd it is
//! paired with (`neochumbyd set NAME start=wx`): sent after `Welcome` as
//! `Msg::Config`. The session asks once as it starts, and keeps the answer on
//! the stick, so a boot without the server uses the last settings it had.
//!
//!   /mnt/usb/neochumby/state/neochumby.settings   key=value lines, as
//!                                                 neochumbyd last sent them

use std::io;
use std::time::Duration;

use neochumby_proto::{Identity, Session, Settings, Stream as _, unhex_key, version};

use crate::conf::{self, Paired};

const FILE: &str = "neochumby.settings";
pub const SETTINGS: &str = "/mnt/usb/neochumby/state/neochumby.settings";

/// How long the session waits on the server for each step. It waits as the
/// Chumby boots, so not long: a server that is down costs a few seconds.
const CONNECT: Duration = Duration::from_secs(3);
const ANSWER: Duration = Duration::from_secs(5);

/// The settings: fresh from the paired neochumbyd if it answers (and kept),
/// else the last ones kept, else none.
pub fn load() -> Settings {
    if let Some(p) = conf::paired() {
        match fetch(&p) {
            Ok(s) => {
                if let Err(e) = keep(&s) {
                    eprintln!("neochumby: {SETTINGS}: {e}");
                }
                return s;
            }
            Err(e) => eprintln!("neochumby: settings from {}: {e}; using the last ones", p.server),
        }
    }
    let kept = conf::read_state(SETTINGS).unwrap_or_default();
    Settings::from_pairs(kept.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())))
}

fn fetch(p: &Paired) -> io::Result<Settings> {
    connect(p).map(|(_, settings)| settings)
}

/// A session with the paired neochumbyd, past `Welcome` and `Config`, at
/// the protocol version the two agree on, and this Chumby's settings from
/// it (only pairs it keeps: neochumby_proto::settings::valid).
/// neochumby-sync carries on with it.
pub fn connect(p: &Paired) -> io::Result<(Session, Settings)> {
    let me = Identity::from_secret(&p.secret)?;
    let server_key = unhex_key(&p.server_key)?;
    let stream = crate::line::connect(&p.server, p.api_port, CONNECT)?;
    stream.set_read_timeout(Some(ANSWER))?;
    stream.set_write_timeout(Some(ANSWER))?;
    let mut s = Session::connect(stream, &me, &server_key)?;
    // A neochumbyd from before Msg::Config sends nothing after Welcome:
    // that times out, and the kept settings are used.
    let (settings, clock) = version::hello_clock(&mut s, conf::name(), ANSWER)?;
    if let Some(t) = clock {
        set_clock(t);
    }
    Ok((s, settings))
}

/// How far the Chumby's clock may be from neochumbyd's before the session
/// sets it: more than the time a Config takes to arrive.
const CLOCK_SLACK: i64 = 30;

/// The Chumby's clock from neochumbyd's (`t`, seconds since the epoch), when
/// it is off by more than [`CLOCK_SLACK`]: a Chumby Classic keeps no time
/// while off, and its kernel finds no RTC, so it boots at 1970. Only as root (the session); anyone else leaves it.
fn set_clock(t: i64) {
    let now = neochumby_sys::time::wall_clock();
    if (now - t).abs() <= CLOCK_SLACK {
        return;
    }
    match neochumby_sys::time::set_wall_clock(t) {
        Ok(()) => eprintln!("neochumby: the clock, from neochumbyd's: {t} (it was {now})"),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {}
        Err(e) => eprintln!("neochumby: setting the clock from neochumbyd's: {e}"),
    }
}

/// Writes SETTINGS only when they changed: a stick's writes wear it, and each
/// is synced.
fn keep(s: &Settings) -> io::Result<()> {
    let body: String = s.to_pairs().iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    if conf::read_state(SETTINGS).is_some_and(|old| old == body) {
        return Ok(());
    }
    conf::write_state(FILE, body.as_bytes())
}
