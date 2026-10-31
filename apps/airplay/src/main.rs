//! AirPlay's service, on the Chumby (the receiver, companion/, is
//! neochumbyd's; the screen, frontend.rs, is in front when opened).
//!
//! It opens this app's companion on the paired neochumbyd, which runs an
//! AirPlay receiver named after this Chumby while it is connected, and
//! keeps that one connection for everything AirPlay does here
//! (neochumby_airplay_proto::receiver):
//!
//! - **The audio** goes to /dev/dsp. The speaker is held only while
//!   something plays, so apps can have it otherwise. A new stream takes the
//!   speaker from whoever holds it, and yields it to a newer player (see
//!   `neochumby::audio`); what arrives while the speaker is someone else's
//!   is dropped, the connection kept, and the stream plays again once the
//!   speaker is free.
//! - **What's playing** goes to the frontends: AirPlay's own screen, or
//!   another app's that the owner approved (neochumby::service), each told
//!   everything known as it connects (hub.rs), then each change. Their
//!   controls go back to the receiver, and on to the sender.
//! - **A stream beginning** asks the session to open AirPlay, which it
//!   does over the start app only, as the approved frontend if there is
//!   one. A resume doesn't.
//! - **As the player**, the session's `pause` and `resume` become the
//!   sender's pause and play, and the audio is dropped while paused.
//!
//! The session opens the companion for it (neochumby::companion); it opens
//! it again, backing off, whenever the companion goes. A tree for testing
//! may hold a stand-in for the companion, `standin-companion` in the app's
//! directory, which is run and spoken to on its stdin and stdout as
//! neochumbyd would the real one (dev/airplay-standin).

use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use neochumby::audio::{self, Speaker};
use neochumby::companion::{self, Typed};
use neochumby::service::{self, Listener};
use neochumby::session::{Notice, Pipe};
use neochumby_airplay::hub::Hub;
use neochumby_airplay_proto::frontend::{ToFrontend, ToService};
use neochumby_airplay_proto::receiver::{Receiver, ToChumby, ToReceiver};
use neochumby_airplay_proto::{APP, Control, Play, Reader, decode, frame};
use neochumby_proto::frame as frames;
use neochumby_sys::fd;

/// The master volume while AirPlay plays. The phone's volume is applied to
/// the audio itself (by shairport-sync), so this only has to be loud enough
/// for the phone's maximum to be the speaker's.
const PLAY_VOLUME: u32 = 90;

/// Having asked for the speaker, or given it up: when to look again. Soon,
/// as a holder that was asked only has to stop its player.
const ASKED_RETRY: Duration = Duration::from_millis(250);
/// /dev/dsp busy with a program outside the handover: when to try again.
const BUSY_RETRY: Duration = Duration::from_secs(2);
/// How long each turn of the loop waits for the receiver, when nothing
/// else is due: frontends and the session are looked at in between.
const TURN: Duration = Duration::from_millis(50);
/// The most a frontend that doesn't read may have waiting for it before
/// it's dropped: a few covers' worth.
const BACKLOG: usize = 256 * 1024;
/// The stand-in for the companion, in a tree for testing.
const STANDIN: &str = "standin-companion";

/// The speaker for AirPlay's stream: 16-bit stereo at 44.1 kHz.
fn open_speaker() -> io::Result<Speaker> {
    Speaker::open(2, 44100, PLAY_VOLUME)
}

/// The receiver: the companion, through the session, or a stand-in.
enum Source {
    Companion(Typed<Receiver>),
    Standin(Child, ChildStdin, ChildStdout),
}

impl Source {
    fn open() -> io::Result<Source> {
        if Path::new(STANDIN).exists() {
            let mut child =
                Command::new(Path::new(".").join(STANDIN)).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
            let (i, o) = (child.stdin.take().expect("piped"), child.stdout.take().expect("piped"));
            eprintln!("neochumby-airplay: the stand-in companion");
            return Ok(Source::Standin(child, i, o));
        }
        Ok(Source::Companion(companion::open(neochumby::companion!())?.typed::<Receiver>()))
    }

    /// The receiver's next message if one comes within `t`; None if none
    /// did, or it was one this build doesn't know. Fails once it has gone.
    fn recv(&mut self, t: Duration) -> io::Result<Option<ToChumby>> {
        let gone = || io::Error::new(io::ErrorKind::BrokenPipe, "the companion has gone");
        match self {
            Source::Companion(link) => match link.recv_timeout(t) {
                Err(e) if e.kind() == io::ErrorKind::InvalidData => Ok(None),
                r => r,
            },
            Source::Standin(_, _, out) => {
                if !fd::readable(&[out.as_fd()], t.as_millis() as i32)?[0] {
                    return Ok(None);
                }
                // A frame is written whole: once some of it has come, the
                // rest follows.
                let f = frames::read(out)?.ok_or_else(gone)?;
                Ok(decode(&f).ok())
            }
        }
    }

    fn send(&mut self, m: &ToReceiver) -> io::Result<()> {
        match self {
            Source::Companion(link) => link.send(m),
            Source::Standin(_, i, _) => neochumby_airplay_proto::send(i, m),
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Source::Standin(child, ..) = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A connected frontend, and what waits to be written to it.
struct Frontend {
    sock: UnixStream,
    reader: Reader,
    out: Vec<u8>,
}

impl Frontend {
    fn new(sock: UnixStream, hello: &[ToFrontend]) -> io::Result<Frontend> {
        sock.set_nonblocking(true)?;
        let mut f = Frontend { sock, reader: Reader::default(), out: Vec::new() };
        for m in hello {
            f.queue(m)?;
        }
        Ok(f)
    }

    fn queue(&mut self, m: &ToFrontend) -> io::Result<()> {
        self.out.extend_from_slice(&frame(m)?);
        if self.out.len() > BACKLOG {
            return Err(io::Error::other("a frontend that doesn't read"));
        }
        Ok(())
    }

    /// Writes what it can without waiting.
    fn flush(&mut self) -> io::Result<()> {
        while !self.out.is_empty() {
            match self.sock.write(&self.out) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    self.out.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// The controls it has sent; fails once it has gone.
    fn controls(&mut self) -> io::Result<Vec<Control>> {
        let mut out = Vec::new();
        loop {
            match self.reader.next::<ToService>(&mut self.sock) {
                Ok(Some(ToService::Control(c))) => out.push(c),
                Ok(None) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(out),
                Err(e) => return Err(e),
            }
        }
    }
}

/// The audio's way to the speaker, as the stream comes and goes.
#[derive(Default)]
struct Player {
    speaker: Option<Speaker>,
    /// While the speaker is someone else's: when to try it again.
    busy_until: Option<Instant>,
    /// This stream gave the speaker up to a newer player: it takes it back
    /// only once the speaker is free, rather than asking for it.
    yielded: bool,
}

impl Player {
    fn play(&mut self, pcm: &[u8]) {
        if self.speaker.as_ref().is_some_and(Speaker::yield_requested) {
            eprintln!("neochumby-airplay: yielding the speaker to a newer player");
            self.speaker = None;
            self.yielded = true;
            self.busy_until = Some(Instant::now() + ASKED_RETRY);
        }
        if self.speaker.is_none() && self.busy_until.is_none_or(|t| Instant::now() >= t) {
            if self.yielded && !audio::free() {
                self.busy_until = Some(Instant::now() + ASKED_RETRY);
            } else {
                match open_speaker() {
                    Ok(sp) => {
                        self.speaker = Some(sp);
                        self.busy_until = None;
                        self.yielded = false;
                    }
                    Err(e) => {
                        if self.busy_until.is_none() {
                            eprintln!("neochumby-airplay: the speaker: {e}; dropping audio until it is free");
                        }
                        let wait = if e.kind() == io::ErrorKind::WouldBlock { ASKED_RETRY } else { BUSY_RETRY };
                        self.busy_until = Some(Instant::now() + wait);
                    }
                }
            }
        }
        if let Some(sp) = self.speaker.as_mut()
            && let Err(e) = sp.write_all(pcm)
        {
            eprintln!("neochumby-airplay: /dev/dsp: {e}");
            self.speaker = None;
        }
    }

    /// The audio has stopped, or is to be dropped: the speaker goes.
    fn stop(&mut self) {
        *self = Player::default();
        audio::withdraw();
    }
}

struct Service {
    hub: Hub,
    source: Option<Source>,
    /// When to try opening the receiver again, and how long the wait after
    /// that one will be.
    due: Instant,
    backoff: u64,
    opened: Instant,
    listener: Option<Listener>,
    frontends: Vec<Frontend>,
    pipe: Option<Pipe>,
    player: Player,
    /// The session's `pause`, until its `resume`.
    paused: bool,
}

impl Service {
    fn new() -> Service {
        let now = Instant::now();
        Service {
            hub: Hub::default(),
            source: None,
            due: now,
            backoff: 2,
            opened: now,
            listener: None,
            frontends: Vec::new(),
            pipe: Pipe::from_env(),
            player: Player::default(),
            paused: false,
        }
    }

    /// Tells every frontend.
    fn tell(&mut self, ms: &[ToFrontend]) {
        self.frontends.retain_mut(|f| ms.iter().all(|m| f.queue(m).is_ok()));
    }

    fn control(&mut self, c: Control) {
        eprintln!("neochumby-airplay: {c:?}");
        if let Some(s) = self.source.as_mut()
            && let Err(e) = s.send(&ToReceiver::Control(c))
        {
            eprintln!("neochumby-airplay: telling the receiver: {e}");
        }
    }

    /// The receiver is opened, if it's time to try.
    fn open(&mut self) {
        if self.source.is_some() || Instant::now() < self.due {
            return;
        }
        match Source::open() {
            Ok(s) => {
                self.source = Some(s);
                self.opened = Instant::now();
                let ms = self.hub.link(true);
                self.tell(&ms);
            }
            Err(e) => self.closed(e),
        }
    }

    /// The receiver has gone, or couldn't be opened: so has the stream,
    /// and any wish for the speaker. Tried again, backing off.
    fn closed(&mut self, e: io::Error) {
        eprintln!("neochumby-airplay: {e}");
        self.source = None;
        self.player.stop();
        let ms = self.hub.link(false);
        self.tell(&ms);
        // A connection that lasted was a good one: start backing off again.
        if self.opened.elapsed() > Duration::from_secs(60) {
            self.backoff = 2;
        }
        self.due = Instant::now() + Duration::from_secs(self.backoff);
        self.backoff = (self.backoff * 2).min(60);
    }

    /// One message from the receiver.
    fn receive(&mut self, m: ToChumby) {
        match m {
            ToChumby::Audio(pcm) if !self.paused => self.player.play(&pcm),
            ToChumby::Audio(_) => {}
            ToChumby::End => self.player.stop(),
            m => {
                let (tell, began) = self.hub.receive(&m);
                if let Some(t) = tell {
                    self.tell(&[t]);
                }
                if began
                    && !self.paused
                    && let Some(p) = self.pipe.as_mut()
                    && !p.try_send(&neochumby::session::Request::Open(APP.into()).line())
                {
                    eprintln!("neochumby-airplay: couldn't ask the session to open {APP}");
                }
            }
        }
    }

    /// The frontends: new ones, their controls, and what waits for them.
    fn serve(&mut self) {
        if self.listener.is_none() {
            match service::listen(APP) {
                Ok(l) => self.listener = Some(l),
                Err(e) if e.kind() == io::ErrorKind::AddrInUse => {}
                Err(e) => eprintln!("neochumby-airplay: listening for frontends: {e}"),
            }
        }
        if let Some(l) = &self.listener {
            loop {
                match l.accept() {
                    Ok(Some(s)) => match Frontend::new(s, &self.hub.snapshot()) {
                        Ok(f) => self.frontends.push(f),
                        Err(e) => eprintln!("neochumby-airplay: a frontend: {e}"),
                    },
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("neochumby-airplay: a frontend: {e}");
                        break;
                    }
                }
            }
        }
        let mut controls = Vec::new();
        self.frontends.retain_mut(|f| match f.controls() {
            Ok(cs) => {
                controls.extend(cs);
                f.flush().is_ok()
            }
            Err(_) => false,
        });
        for c in controls {
            // Played from a frontend: the session's pause is over.
            if matches!(c, Control::PlayPause | Control::Play) {
                self.paused = false;
            }
            self.control(c);
        }
    }

    /// The session's notices: as the player, `pause` and `resume`.
    fn notices(&mut self) {
        let Some(p) = self.pipe.as_mut() else { return };
        p.flush();
        for line in p.receive().lines {
            match Notice::parse(&line) {
                Some(Notice::Pause) => {
                    self.paused = true;
                    self.player.stop();
                    if self.hub.play() == Play::Playing {
                        self.control(Control::Pause);
                    }
                }
                Some(Notice::Resume) => {
                    self.paused = false;
                    if self.hub.play() == Play::Paused {
                        self.control(Control::Play);
                    }
                }
                _ => {}
            }
        }
    }

    fn run(mut self) -> ! {
        loop {
            self.open();
            match self.source.as_mut().map(|s| s.recv(TURN)) {
                Some(Ok(Some(m))) => self.receive(m),
                Some(Ok(None)) => {}
                Some(Err(e)) => self.closed(e),
                None => neochumby::sleep_ms(TURN.as_millis() as u32),
            }
            self.serve();
            self.notices();
        }
    }
}

fn main() {
    neochumby::debug::install("neochumby-airplay");
    Service::new().run()
}
