//! AirPlay's Chumby half (the companion, companion.rs, is neochumbyd's).
//!
//! AirPlay on the Chumby's speaker, as a background service: it opens this
//! app's companion on the paired neochumbyd, which runs an AirPlay receiver
//! named after this Chumby while it is connected and sends what it plays
//! here as PCM ([0] + PCM; [1] when it stops), which goes to /dev/dsp. The
//! speaker is held only while something plays, so apps can have it
//! otherwise; if something else has it, what arrives meanwhile is dropped,
//! and the connection kept.
//!
//! Connects to the paired neochumbyd (/psp/neochumby.conf) and checks its
//! key against the one pinned there; reconnects, backing off, whenever the
//! connection goes.

use std::io::{self, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use neochumby::audio::Speaker;
use neochumby::conf::{self, Paired};
use neochumby_proto::{Identity, Msg, Session, unhex_key};

/// This app, as neochumbyd knows it: its directory in the tree.
const APP: &str = "airplay";
// The companion's frames.
const AUDIO: u8 = 0;
const END: u8 = 1;

/// The master volume while AirPlay plays. The phone's volume is applied to
/// the audio itself (by shairport-sync), so this only has to be loud enough
/// for the phone's maximum to be the speaker's.
const PLAY_VOLUME: u32 = 90;

/// The speaker for AirPlay's stream: 16-bit stereo at 44.1 kHz.
fn open_speaker() -> io::Result<Speaker> {
    Speaker::open(2, 44100, PLAY_VOLUME)
}

fn session(p: &Paired, me: &Identity) -> io::Result<Session> {
    let server_key = unhex_key(&p.server_key)?;
    let addr = (p.server.as_str(), p.api_port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
    let mut s = Session::connect(tcp, me)?;
    if s.peer() != &server_key {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "not the paired neochumbyd's key"));
    }
    s.send(&Msg::Hello { name: conf::name() })?;
    match s.recv()? {
        Msg::Welcome => {}
        Msg::Unpaired => {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "neochumbyd no longer pairs with this Chumby"));
        }
        m => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected {m:?}"))),
    }
    s.send(&Msg::Open { app: APP.into() })?;
    Ok(s)
}

fn play(mut s: Session) -> io::Result<()> {
    let mut speaker: Option<Speaker> = None;
    // While the speaker is someone else's: when to try it again.
    let mut busy_until: Option<Instant> = None;
    loop {
        let frame = match s.recv()? {
            Msg::Data(f) => f,
            Msg::Closed => return Err(io::Error::other("the companion has gone")),
            _ => continue,
        };
        match frame.split_first() {
            Some((&AUDIO, pcm)) => {
                if speaker.is_none() && busy_until.is_none_or(|t| Instant::now() >= t) {
                    match open_speaker() {
                        Ok(sp) => {
                            speaker = Some(sp);
                            busy_until = None;
                        }
                        Err(e) => {
                            if busy_until.is_none() {
                                eprintln!("neochumby-airplay: /dev/dsp: {e}; dropping audio until it is free");
                            }
                            busy_until = Some(Instant::now() + Duration::from_secs(2));
                        }
                    }
                }
                if let Some(sp) = speaker.as_mut()
                    && let Err(e) = sp.write_all(pcm)
                {
                    eprintln!("neochumby-airplay: /dev/dsp: {e}");
                    speaker = None;
                }
            }
            Some((&END, _)) => {
                speaker = None;
                busy_until = None;
            }
            _ => {}
        }
    }
}

fn main() {
    neochumby::debug::install("neochumby-airplay");
    let Some(p) = conf::paired() else {
        eprintln!("neochumby-airplay: not paired");
        std::process::exit(1);
    };
    let me = match Identity::from_secret(&p.secret) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("neochumby-airplay: {}: {e}", conf::KEY);
            std::process::exit(1);
        }
    };
    let mut backoff = 2;
    loop {
        let started = Instant::now();
        let r = session(&p, &me).and_then(play);
        if let Err(e) = r {
            eprintln!("neochumby-airplay: {e}");
        }
        // A connection that lasted was a good one: start backing off again.
        if started.elapsed() > Duration::from_secs(60) {
            backoff = 2;
        }
        neochumby::sleep_ms(backoff * 1000);
        backoff = (backoff * 2).min(60);
    }
}
