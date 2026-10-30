//! The protocol's version, agreed on at the start of each connection.
//!
//! Version 0 is the protocol from before versions were numbered, and its
//! Chumbys and neochumbyds stay in service: each end must keep working
//! with the other's version-0 build. A version-0 decoder refuses a tag it
//! doesn't know and bytes after a message's last field, so a Chumby can't
//! say its version first: neither a longer `Hello` nor a new message would
//! get past a version-0 neochumbyd. neochumbyd says its version first
//! instead, as a pair in the `Config` that every Chumby already takes:
//!
//! ```text
//! Chumby      Hello { name }                     as in version 0
//! neochumbyd  Welcome
//! neochumbyd  Config([settings..., ("proto.version", "1")])
//! Chumby      Version(1)                         only if Config said >= 1
//! ```
//!
//! - A version-0 Chumby drops the pair, as it drops any pair whose key
//!   isn't all a-z and `_` (so it is never kept in its /psp either), and
//!   sends what it always sent next. neochumbyd takes a first message
//!   that isn't `Version` to mean version 0, and handles it as before.
//! - A version-0 neochumbyd sends no such pair. The Chumby stays at
//!   version 0 and sends no `Version`, which that neochumbyd couldn't
//!   decode.
//! - Otherwise both ends use the lower of the two versions ([`agree`]).
//!
//! Each end keeps the version agreed on in its [`Session`], which then
//! sends no message newer than that ([`Msg::since`]). Until the two have
//! agreed, a session is at version 0.

use std::io;
use std::time::{Duration, Instant};

use crate::settings::Settings;
use crate::{Msg, Session, VERSION, invalid};

/// The pair in `Config` that gives neochumbyd's version. Not a setting:
/// a key with a dot is one no Chumby keeps and no owner can set.
pub const KEY: &str = "proto.version";

/// The version two ends use: the lower of theirs.
pub fn agree(ours: u16, theirs: u16) -> u16 {
    ours.min(theirs)
}

/// Whether a session at version `v` may send `msg`.
pub(crate) fn sendable(msg: &Msg, v: u16) -> io::Result<()> {
    match msg.since() {
        Some(since) if since <= v => Ok(()),
        Some(since) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{msg:?} is protocol version {since}; this connection is at {v}"),
        )),
        None => Err(io::Error::new(io::ErrorKind::InvalidInput, "an unknown message is never sent")),
    }
}

/// neochumbyd, after `Welcome`: the Chumby's settings, saying its version.
pub fn config(settings: &Settings) -> Msg {
    config_as(settings, VERSION)
}

fn config_as(settings: &Settings, ours: u16) -> Msg {
    let mut pairs = settings.to_pairs();
    pairs.push((KEY.to_string(), ours.to_string()));
    Msg::Config(pairs)
}

/// neochumbyd, after sending [`config`]: the Chumby's first message after
/// it. A `Version` first sets the session's version and is read past, as
/// is any [`Msg::Unknown`]; without one, the Chumby is version 0. All of
/// it by `deadline` ([`io::ErrorKind::TimedOut`] if not), so that a Chumby
/// saying nothing doesn't hold the connection.
pub fn after_config(s: &mut Session, deadline: Instant) -> io::Result<Msg> {
    after_config_as(s, VERSION, deadline)
}

fn after_config_as(s: &mut Session, ours: u16, deadline: Instant) -> io::Result<Msg> {
    let mut first = true;
    loop {
        match s.recv_by(deadline)? {
            Msg::Version(theirs) if first => s.set_version(agree(ours, theirs)),
            Msg::Version(_) => return Err(invalid("a second Version")),
            Msg::Unknown { .. } => {}
            m => return Ok(m),
        }
        first = false;
    }
}

/// The Chumby: says `Hello` as `name`, takes `Welcome` and `Config`, each
/// within `wait`, and agrees on the version. Its settings, without the
/// version's pair. A neochumbyd that doesn't know the Chumby's key answers
/// [`io::ErrorKind::PermissionDenied`].
pub fn hello(s: &mut Session, name: String, wait: Duration) -> io::Result<Settings> {
    hello_as(s, name, wait, VERSION)
}

fn hello_as(s: &mut Session, name: String, wait: Duration, ours: u16) -> io::Result<Settings> {
    s.send(&Msg::Hello { name })?;
    let mut next = || s.recv_timeout(wait)?.ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut));
    match next()? {
        Msg::Welcome => {}
        Msg::Unpaired => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "no longer paired")),
        m => return Err(invalid(format!("unexpected {m:?}"))),
    }
    let pairs = match next()? {
        Msg::Config(pairs) => pairs,
        m => return Err(invalid(format!("unexpected {m:?}"))),
    };
    // A version-0 neochumbyd sends none: version 0.
    let theirs = pairs.iter().find(|(k, _)| k == KEY).map_or(0, |(_, v)| v.parse().unwrap_or(0));
    let v = agree(ours, theirs);
    if v >= 1 {
        s.set_version(v);
        s.send(&Msg::Version(ours))?;
    }
    Ok(Settings::from_pairs(pairs.into_iter().filter(|(k, _)| k != KEY)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Delivery;
    use crate::{Identity, Unverified};
    use std::net::{TcpListener, TcpStream};
    use std::thread::JoinHandle;

    const WAIT: Duration = Duration::from_secs(5);

    /// A connected pair: the Chumby's session, and neochumbyd's running `f`.
    fn pair<T: Send + 'static>(f: impl FnOnce(Session) -> T + Send + 'static) -> (Session, JoinHandle<T>) {
        let (server, chumby) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let server_key = server.public();
        let t = std::thread::spawn(move || {
            f(Unverified::accept(l.accept().unwrap().0, &server).unwrap().verify(|_| true).unwrap())
        });
        (Session::connect(TcpStream::connect(addr).unwrap(), &chumby, &server_key).unwrap(), t)
    }

    fn settings() -> Settings {
        Settings { start: Some("wx".into()), delivery: Some(Delivery::Sync), ..Default::default() }
    }

    /// neochumbyd at version `ours`, as api.rs runs it (version 0: as one
    /// from before versions, which sends the bare settings): the
    /// Chumby's first message after Config, and the version agreed.
    fn server(ours: u16) -> impl FnOnce(Session) -> (Msg, u16) {
        move |mut s| {
            assert!(matches!(s.recv().unwrap(), Msg::Hello { .. }));
            s.send(&Msg::Welcome).unwrap();
            if ours == 0 {
                s.send(&Msg::Config(settings().to_pairs())).unwrap();
                (s.recv().unwrap(), s.version())
            } else {
                s.send(&config_as(&settings(), ours)).unwrap();
                (after_config_as(&mut s, ours, Instant::now() + WAIT).unwrap(), s.version())
            }
        }
    }

    #[test]
    fn new_and_new_agree_on_the_lower() {
        for (chumby, server_v, agreed) in [(1, 1, 1), (1, 3, 1), (3, 1, 1), (2, 3, 2)] {
            let (mut c, t) = pair(server(server_v));
            let got = hello_as(&mut c, "den".into(), WAIT, chumby).unwrap();
            assert_eq!(got, settings(), "the version's pair is not a setting");
            assert_eq!(c.version(), agreed);
            c.send(&Msg::Dev).unwrap();
            assert_eq!(t.join().unwrap(), (Msg::Dev, agreed));
        }
    }

    #[test]
    fn a_new_chumby_and_an_old_neochumbyd_stay_at_0() {
        let (mut c, t) = pair(server(0));
        assert_eq!(hello(&mut c, "den".into(), WAIT).unwrap(), settings());
        assert_eq!(c.version(), 0);
        // Nothing newer goes out, so the old decoder never sees it.
        assert_eq!(c.send(&Msg::Version(1)).unwrap_err().kind(), io::ErrorKind::Unsupported);
        c.send(&Msg::Open { app: "logs".into() }).unwrap();
        // The old neochumbyd's first message after Config is the Open.
        assert_eq!(t.join().unwrap(), (Msg::Open { app: "logs".into() }, 0));
    }

    #[test]
    fn an_old_chumby_and_a_new_neochumbyd_stay_at_0() {
        let (mut c, t) = pair(server(VERSION));
        // A version-0 Chumby: Hello, Welcome, Config, then on.
        c.send(&Msg::Hello { name: "den".into() }).unwrap();
        assert_eq!(c.recv().unwrap(), Msg::Welcome);
        let Msg::Config(pairs) = c.recv().unwrap() else { panic!() };
        // What a version-0 Chumby keeps of them (its settings::valid): the
        // settings, and not the version's pair.
        let kept: Vec<_> = pairs
            .iter()
            .filter(|(k, v)| {
                !k.is_empty()
                    && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                    && !v.is_empty()
                    && v.bytes().all(|b| b.is_ascii_alphanumeric() || b".:/_-".contains(&b))
            })
            .cloned()
            .collect();
        assert_eq!(kept, settings().to_pairs());
        c.send(&Msg::SyncHave { live: [0; 32] }).unwrap();
        assert_eq!(t.join().unwrap(), (Msg::SyncHave { live: [0; 32] }, 0));
    }

    #[test]
    fn unknown_messages_after_config_are_skipped() {
        let (mut c, t) = pair(server(VERSION));
        hello(&mut c, "den".into(), WAIT).unwrap();
        // As a later version's skippable message would arrive.
        c.send_body(&[200, 1, 2, 3]).unwrap();
        c.send(&Msg::Dev).unwrap();
        assert_eq!(t.join().unwrap(), (Msg::Dev, 1));
    }

    /// A Chumby that says nothing after Config is given up on.
    #[test]
    fn after_config_has_a_deadline() {
        let (mut c, t) = pair(|mut s| {
            s.recv().unwrap();
            s.send(&Msg::Welcome).unwrap();
            s.send(&config(&settings())).unwrap();
            after_config(&mut s, Instant::now() + Duration::from_millis(200)).unwrap_err().kind()
        });
        hello(&mut c, "den".into(), WAIT).unwrap();
        assert_eq!(t.join().unwrap(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn unpaired() {
        let (mut c, t) = pair(|mut s| {
            s.recv().unwrap();
            s.send(&Msg::Unpaired).unwrap();
        });
        assert_eq!(hello(&mut c, "den".into(), WAIT).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        t.join().unwrap();
    }

    #[test]
    fn gating() {
        assert!(sendable(&Msg::Welcome, 0).is_ok());
        assert!(sendable(&Msg::Version(1), 0).is_err());
        assert!(sendable(&Msg::Version(1), 1).is_ok());
        assert!(sendable(&Msg::Unknown { tag: 200, body: vec![] }, u16::MAX).is_err());
        assert_eq!(agree(0, 1), 0);
        assert_eq!(agree(VERSION, VERSION), VERSION);
    }
}
