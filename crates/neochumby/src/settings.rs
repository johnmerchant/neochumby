//! This Chumby's settings as its owner chose them on the neochumbyd it is
//! paired with (`neochumbyd set NAME start=wx`, later its web page): sent
//! after `Welcome` as `Msg::Config`. The session asks once as it starts,
//! and keeps the answer in /psp, so a boot without the server uses the last
//! settings it had.
//!
//!   /psp/neochumby.settings   key=value lines, as neochumbyd last sent them

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use neochumby_proto::{Identity, Session, Settings, unhex_key, version};

use crate::conf::{self, Paired};

pub const SETTINGS: &str = "/psp/neochumby.settings";

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
    let kept = std::fs::read_to_string(SETTINGS).unwrap_or_default();
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
    let addr = (p.server.as_str(), p.api_port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))?;
    let tcp = TcpStream::connect_timeout(&addr, CONNECT)?;
    tcp.set_read_timeout(Some(ANSWER))?;
    tcp.set_write_timeout(Some(ANSWER))?;
    let mut s = Session::connect(tcp, &me)?;
    if s.peer() != &server_key {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "not the paired neochumbyd's key"));
    }
    // A neochumbyd from before Msg::Config sends nothing after Welcome:
    // that times out, and the kept settings are used.
    let settings = version::hello(&mut s, conf::name(), ANSWER)?;
    Ok((s, settings))
}

/// Writes SETTINGS only when they changed: /psp is a small flash.
fn keep(s: &Settings) -> io::Result<()> {
    let body: String = s.to_pairs().iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    if std::fs::read_to_string(SETTINGS).is_ok_and(|old| old == body) {
        return Ok(());
    }
    conf::write_atomic(Path::new(SETTINGS), body.as_bytes(), 0o644)
}
