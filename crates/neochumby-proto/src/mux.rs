//! Many connections over one byte stream: a Chumby's serial line to its
//! neochumbyd, which an emulated Chumby has in place of a network. The
//! Chumby opens connections and neochumbyd takes them; each carries a
//! session as a TCP connection would, TLS and all. The line only carries
//! bytes, and authenticates nothing.
//!
//! The protocol is a [`Core`], which neither waits nor does I/O: bytes in,
//! frames out, and each channel's reads and writes. [`Line`] runs one with
//! a thread each way and gives each channel as a [`Channel`], a blocking
//! [`Stream`]: that is neochumbyd's side (and the tests'). The Chumby's
//! session drives its own with poll(2), as std's blocking waits use futex
//! flags its 2.6.16 kernel doesn't have.
//!
//! On the line each frame is a kind, a channel, a length and that many
//! bytes, then the CRC-32 of all of it (big-endian):
//!
//! ```text
//! kind u8 | channel u16 | length u16 | payload | crc32
//! ```
//!
//! - `SYNC` (both): starts the line, below.
//! - `OPEN` (the Chumby): a new channel; `ACCEPT` (neochumbyd) takes it,
//!   and `CLOSE`, either way, ends it or turns it down.
//! - `DATA` (both): at most [`MAX_DATA`] bytes on a channel.
//! - `CREDIT` (both): the other side may send that many more bytes on a
//!   channel. Each side may have [`WINDOW`] bytes on a channel that the
//!   other has not yet read, so a reader that stops holds up only its own
//!   channel, and never the line.
//! - `ACK` (both): how many DATA bytes have come off the line. Each side
//!   keeps at most [`LINE_WINDOW`] of them in flight: on the Chumby, what
//!   has come and not been read waits in the kernel's tty buffers, which
//!   are small, and drop what doesn't fit.
//!
//! Each side must always take in what the other sends, never waiting on
//! its own writes to do so: an emulator blocks while its guest writes to a
//! socket that is full, so two sides that each wait to write before they
//! read would deadlock.
//!
//! **Starting, and starting again.** Each side has an incarnation, a
//! random number it takes as it starts and again whenever the line falls
//! out of step (a frame that fails its CRC, or makes no sense). A `SYNC`
//! carries the sender's incarnation, the other's as it last heard it, and
//! whether it has heard the other acknowledge its own. Until it has, a side
//! sends a `SYNC` every [`RESYNC`]; one that hears a `SYNC` that hasn't
//! heard it acknowledged answers with its own. A side out of step ignores
//! everything until a `SYNC`. When the other side's incarnation changes
//! (it restarted, or lost step), every channel ends, as TCP connections
//! end when their host goes, and the sessions on them start again. So a
//! neochumbyd that restarts, an emulator that reconnects, or a session
//! that restarts on the Chumby each bring the line back by themselves.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::Stream;

/// The most a DATA frame carries: small enough that one channel's burst
/// holds up another's little.
pub const MAX_DATA: usize = 1024;
/// What one side may send on a channel before the other has read it.
pub const WINDOW: u32 = 32 << 10;
/// What one side may have in flight on the line, all channels together:
/// with its frames' headers, well within the 4096 bytes a 2.6.16 tty holds
/// unread before it drops what comes.
pub const LINE_WINDOW: u64 = 2 << 10;
/// How often a side out of step, or not yet acknowledged, sends a `SYNC`.
pub const RESYNC: Duration = Duration::from_secs(1);
/// The most channels open at once on a line.
pub const MAX_CHANNELS: usize = 32;

/// After this many bytes read, a channel's reader sends `CREDIT`.
const CREDIT_EVERY: u32 = WINDOW / 4;
/// After this many DATA bytes in, a side sends `ACK`.
const ACK_EVERY: u64 = LINE_WINDOW / 4;

const SYNC: u8 = 0;
const OPEN: u8 = 1;
const ACCEPT: u8 = 2;
const CLOSE: u8 = 3;
const DATA: u8 = 4;
const CREDIT: u8 = 5;
const ACK: u8 = 6;

/// A `SYNC`'s payload: this, the sender's incarnation, the other's, and
/// whether the sender has heard its own acknowledged.
const MAGIC: &[u8; 8] = b"chumbyln";
const SYNC_LEN: usize = MAGIC.len() + 4 + 4 + 1;
const HEADER: usize = 5;
const TRAILER: usize = 4;

/// Where a channel is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Opened, and neochumbyd hasn't yet taken it.
    Opening,
    Open,
    /// The other side turned it down, or the line started again: it has
    /// gone without being open.
    Refused,
}

/// One side of the protocol, with no I/O and no waiting: bytes in with
/// [`Core::input`], frames out with [`Core::output`], and channels by
/// number. A channel's number is good while [`Core::epoch`] stays the same;
/// a new epoch means every channel has gone.
pub struct Core {
    /// neochumbyd's side takes channels; the Chumby's opens them.
    accepting: bool,
    me: u32,
    peer: Option<u32>,
    /// The other side has acknowledged `me`.
    established: bool,
    epoch: u64,
    chans: HashMap<u16, Chan>,
    /// Channels the Chumby opened that [`Core::accept`] hasn't taken.
    accepts: VecDeque<u16>,
    next_id: u16,
    /// Frames to send: SYNC, CREDIT and ACK first, then the rest in order
    /// (a channel's DATA before its CLOSE).
    urgent: VecDeque<Vec<u8>>,
    ordered: VecDeque<Vec<u8>>,
    next_sync: Instant,
    /// DATA bytes sent, and what the other side says it has taken.
    sent: u64,
    acked: u64,
    /// DATA bytes taken, and how many of them acknowledged.
    taken: u64,
    taken_acked: u64,
    decoder: Decoder,
    /// Why the line has gone, once it has.
    gone: Option<String>,
    /// Why it fell out of step, each time, until [`Core::losses`] takes
    /// them.
    losses: VecDeque<&'static str>,
    /// The other side's incarnation changed while it had channels.
    restarts: usize,
}

#[derive(Default)]
struct Chan {
    /// The other side has it: ACCEPT came (or it was the one to OPEN).
    open: bool,
    rbuf: VecDeque<u8>,
    /// What this side may still send.
    credit: u32,
    /// Read since the last CREDIT.
    unread: u32,
    /// CLOSE came from the other side, or was sent from this one.
    their_close: bool,
    our_close: bool,
}

impl Core {
    /// The Chumby's side: it opens channels.
    pub fn chumby() -> Core {
        Core::new(false)
    }

    /// neochumbyd's side: it takes them.
    pub fn server() -> Core {
        Core::new(true)
    }

    fn new(accepting: bool) -> Core {
        Core {
            accepting,
            me: incarnation(),
            peer: None,
            established: false,
            epoch: 0,
            chans: HashMap::new(),
            accepts: VecDeque::new(),
            next_id: 1,
            urgent: VecDeque::new(),
            ordered: VecDeque::new(),
            next_sync: Instant::now(),
            sent: 0,
            acked: 0,
            taken: 0,
            taken_acked: 0,
            decoder: Decoder::default(),
            gone: None,
            losses: VecDeque::new(),
            restarts: 0,
        }
    }

    /// Bytes that came off the line.
    pub fn input(&mut self, b: &[u8]) {
        if self.gone.is_some() {
            return;
        }
        self.decoder.push(b);
        loop {
            match self.decoder.next() {
                Ok(None) => return,
                Ok(Some((kind, id, p))) => {
                    if let Err(why) = self.take(kind, id, &p) {
                        self.lose_step(why);
                    }
                }
                Err(OutOfStep) => self.lose_step("a frame that fails its check"),
            }
        }
    }

    /// The next frame to write to the line, if there is one at `now`.
    pub fn output(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.gone.is_some() {
            return None;
        }
        if !self.established && now >= self.next_sync {
            self.next_sync = now + RESYNC;
            let f = self.sync_frame();
            self.urgent.push_back(f);
        }
        self.urgent.pop_front().or_else(|| self.ordered.pop_front())
    }

    /// When [`Core::output`] next has a frame without anything coming in:
    /// the next `SYNC`, while the other side hasn't acknowledged this one.
    pub fn wake_at(&self) -> Option<Instant> {
        (!self.established && self.gone.is_none()).then_some(self.next_sync)
    }

    /// The stream under the line has ended: every channel goes with it.
    pub fn end(&mut self, why: impl Into<String>) {
        if self.gone.is_none() {
            self.gone = Some(why.into());
            self.reset();
        }
    }

    /// What has happened to the line since last asked, to log: each time
    /// it fell out of step, and why; and each time the other side started
    /// again with channels open.
    pub fn events(&mut self) -> Vec<String> {
        let mut v: Vec<String> = self.losses.drain(..).map(|w| format!("out of step: {w}")).collect();
        for _ in 0..std::mem::take(&mut self.restarts) {
            v.push("the other side started again; its channels ended".into());
        }
        v
    }

    /// Why the line has gone, if it has.
    pub fn gone(&self) -> Option<&str> {
        self.gone.as_deref()
    }

    /// Whether the other side is there and has acknowledged this one.
    pub fn established(&self) -> bool {
        self.established
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The Chumby's side: a new channel, [`Phase::Opening`] until
    /// neochumbyd takes it.
    pub fn open(&mut self) -> io::Result<u16> {
        if self.accepting {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "neochumbyd's side takes channels"));
        }
        self.check_gone()?;
        if !self.established {
            return Err(refused("no neochumbyd on the line"));
        }
        if self.chans.len() >= MAX_CHANNELS {
            return Err(io::Error::other("too many channels open on the line"));
        }
        let mut id = self.next_id;
        while id == 0 || self.chans.contains_key(&id) {
            id = id.wrapping_add(1);
        }
        self.next_id = id.wrapping_add(1);
        self.chans.insert(id, Chan { credit: WINDOW, ..Chan::default() });
        self.ordered.push_back(frame(OPEN, id, &[]));
        Ok(id)
    }

    /// neochumbyd's side: the next channel the Chumby opened.
    pub fn accept(&mut self) -> Option<u16> {
        while let Some(id) = self.accepts.pop_front() {
            if self.chans.contains_key(&id) {
                return Some(id);
            }
        }
        None
    }

    /// Where channel `id` is; None once it has gone (or never was).
    pub fn phase(&self, id: u16) -> Option<Phase> {
        let c = self.chans.get(&id)?;
        Some(match (c.open, c.their_close) {
            (true, _) => Phase::Open,
            (false, false) => Phase::Opening,
            (false, true) => Phase::Refused,
        })
    }

    /// Whether a read of `id` has something to give: bytes, or the end.
    pub fn readable(&self, id: u16) -> bool {
        self.chans.get(&id).is_none_or(|c| !c.rbuf.is_empty() || c.their_close || c.our_close)
    }

    /// Whether a write to `id` would take something (or fail at once).
    pub fn writable(&self, id: u16) -> bool {
        self.room(id) > 0 || self.chans.get(&id).is_none_or(|c| c.our_close || c.their_close)
    }

    fn room(&self, id: u16) -> usize {
        let Some(c) = self.chans.get(&id) else { return 0 };
        let line = LINE_WINDOW.saturating_sub(self.sent - self.acked) as usize;
        (c.credit as usize).min(line).min(MAX_DATA)
    }

    /// What has come on `id`: Ok(0) at its end, and
    /// [`io::ErrorKind::WouldBlock`] if nothing has yet.
    pub fn read(&mut self, id: u16, buf: &mut [u8]) -> io::Result<usize> {
        let gone = self.gone_error_if(!self.chans.contains_key(&id));
        let c = self.chans.get_mut(&id).ok_or_else(|| gone.unwrap_or_else(reset))?;
        if c.our_close {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "the channel is closed"));
        }
        if c.rbuf.is_empty() {
            return if c.their_close { Ok(0) } else { Err(io::ErrorKind::WouldBlock.into()) };
        }
        let n = buf.len().min(c.rbuf.len());
        for (b, v) in buf.iter_mut().zip(c.rbuf.drain(..n)) {
            *b = v;
        }
        c.unread += n as u32;
        if c.unread >= CREDIT_EVERY {
            let more = std::mem::take(&mut c.unread);
            self.urgent.push_back(frame(CREDIT, id, &more.to_be_bytes()));
        }
        Ok(n)
    }

    /// Sends what of `buf` the windows have room for, at most
    /// [`MAX_DATA`]; [`io::ErrorKind::WouldBlock`] if they have none.
    pub fn write(&mut self, id: u16, buf: &[u8]) -> io::Result<usize> {
        let room = self.room(id);
        let gone = self.gone_error_if(!self.chans.contains_key(&id));
        let c = self.chans.get_mut(&id).ok_or_else(|| gone.unwrap_or_else(reset))?;
        if c.our_close || c.their_close {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the channel is closed"));
        }
        if buf.is_empty() {
            return Ok(0);
        }
        if room == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = buf.len().min(room);
        c.credit -= n as u32;
        self.sent += n as u64;
        self.ordered.push_back(frame(DATA, id, &buf[..n]));
        Ok(n)
    }

    /// This side is done with `id`: what it wrote still goes, then CLOSE.
    pub fn close(&mut self, id: u16) {
        let Some(c) = self.chans.get_mut(&id) else { return };
        if !c.our_close {
            c.our_close = true;
            if c.open || !c.their_close {
                self.ordered.push_back(frame(CLOSE, id, &[]));
            }
        }
        if c.their_close {
            self.chans.remove(&id);
        }
    }

    fn check_gone(&self) -> io::Result<()> {
        match &self.gone {
            Some(why) => Err(io::Error::new(io::ErrorKind::ConnectionAborted, format!("the line has gone: {why}"))),
            None => Ok(()),
        }
    }

    /// The error for a channel that isn't there any more.
    fn gone_error(&self) -> io::Error {
        self.check_gone().err().unwrap_or_else(reset)
    }

    fn gone_error_if(&self, missing: bool) -> Option<io::Error> {
        missing.then(|| self.gone_error())
    }

    /// Every channel ends: the other side is another incarnation, or this
    /// one is.
    fn reset(&mut self) {
        self.epoch += 1;
        self.chans.clear();
        self.accepts.clear();
        self.ordered.clear();
        self.sent = 0;
        self.acked = 0;
        self.taken = 0;
        self.taken_acked = 0;
    }

    /// This side has lost step: it becomes a new incarnation, and says so
    /// until the other has heard.
    fn lose_step(&mut self, why: &'static str) {
        self.losses.push_back(why);
        self.reset();
        self.decoder.in_step = false;
        self.me = incarnation();
        self.peer = None;
        self.established = false;
        self.next_sync = Instant::now();
    }

    fn sync_frame(&self) -> Vec<u8> {
        sync_frame(self.me, self.peer, self.established)
    }

    /// Acts on a frame that came in whole.
    fn take(&mut self, kind: u8, id: u16, p: &[u8]) -> Result<(), &'static str> {
        match kind {
            SYNC => {
                let (them, heard, acked) = parse_sync(p).ok_or("a SYNC that isn't one")?;
                if self.peer != Some(them) {
                    if !self.chans.is_empty() {
                        self.restarts += 1;
                    }
                    self.reset();
                    self.peer = Some(them);
                }
                self.established = heard == self.me;
                if !acked {
                    let f = self.sync_frame();
                    self.urgent.push_back(f);
                }
            }
            OPEN if self.accepting => {
                if self.chans.contains_key(&id) || self.chans.len() >= MAX_CHANNELS || id == 0 {
                    self.ordered.push_back(frame(CLOSE, id, &[]));
                } else {
                    self.chans.insert(id, Chan { open: true, credit: WINDOW, ..Chan::default() });
                    self.accepts.push_back(id);
                    self.ordered.push_back(frame(ACCEPT, id, &[]));
                }
            }
            OPEN => self.ordered.push_back(frame(CLOSE, id, &[])),
            ACCEPT => {
                if let Some(c) = self.chans.get_mut(&id) {
                    c.open = true;
                }
            }
            CLOSE => {
                if let Some(c) = self.chans.get_mut(&id) {
                    c.their_close = true;
                    if c.our_close {
                        self.chans.remove(&id);
                    }
                }
            }
            DATA => {
                self.taken += p.len() as u64;
                if self.taken - self.taken_acked >= ACK_EVERY {
                    self.taken_acked = self.taken;
                    self.urgent.push_back(frame(ACK, 0, &self.taken.to_be_bytes()));
                }
                if let Some(c) = self.chans.get_mut(&id)
                    && !c.our_close
                {
                    if c.rbuf.len() + p.len() > WINDOW as usize {
                        return Err("more DATA than the channel's window");
                    }
                    c.rbuf.extend(p);
                }
            }
            CREDIT => {
                let n = u32::from_be_bytes(p.try_into().map_err(|_| "a CREDIT that isn't one")?);
                if let Some(c) = self.chans.get_mut(&id) {
                    c.credit = c.credit.saturating_add(n).min(WINDOW);
                }
            }
            ACK => {
                let n = u64::from_be_bytes(p.try_into().map_err(|_| "an ACK that isn't one")?);
                if n > self.sent {
                    return Err("an ACK for more than was sent");
                }
                self.acked = self.acked.max(n);
            }
            _ => return Err("a frame of no kind known"),
        }
        Ok(())
    }
}

/// A [`Core`] run by two threads, one reading the line and one writing it:
/// neochumbyd's side, where std's blocking waits work. The line runs until
/// the stream under it ends.
#[derive(Clone)]
pub struct Line(Arc<Shared>);

struct Shared {
    core: Mutex<Core>,
    /// Woken on every change: a frame in, room to send, a channel's end.
    changed: Condvar,
}

impl Line {
    /// The Chumby's side of a line (for tests: the Chumby runs its own,
    /// with poll(2)). `r` and `w` are the line's two directions.
    pub fn chumby(r: impl Read + Send + 'static, w: impl Write + Send + 'static) -> Line {
        Line::start(Core::chumby(), r, w, |_| {})
    }

    /// neochumbyd's side of a line (a Unix socket the emulator connects).
    /// `log` is told what happens to it ([`Core::events`]).
    pub fn server(
        r: impl Read + Send + 'static,
        w: impl Write + Send + 'static,
        log: impl Fn(&str) + Send + 'static,
    ) -> Line {
        Line::start(Core::server(), r, w, log)
    }

    fn start(
        core: Core,
        r: impl Read + Send + 'static,
        w: impl Write + Send + 'static,
        log: impl Fn(&str) + Send + 'static,
    ) -> Line {
        let shared = Arc::new(Shared { core: Mutex::new(core), changed: Condvar::new() });
        let (a, b) = (shared.clone(), shared.clone());
        std::thread::spawn(move || reader(&a, r, log));
        std::thread::spawn(move || writer(&b, w));
        Line(shared)
    }

    /// The Chumby's side: a new channel to neochumbyd, once it has taken
    /// it. Fails if no neochumbyd answers by `timeout` (none is on the
    /// line, as a TCP connect is refused), or if it turns the channel down.
    pub fn open(&self, timeout: Duration) -> io::Result<Channel> {
        let deadline = Instant::now() + timeout;
        let mut core = self.0.lock();
        while !core.established() {
            core.check_gone()?;
            core = self.0.wait_until(core, Some(deadline)).ok_or_else(|| refused("no neochumbyd on the line"))?;
        }
        let id = core.open()?;
        let epoch = core.epoch();
        self.0.changed.notify_all();
        loop {
            if core.epoch() != epoch {
                return Err(core.gone_error());
            }
            match core.phase(id) {
                Some(Phase::Open) => return Ok(Channel::new(self.0.clone(), id, epoch)),
                Some(Phase::Refused) | None => {
                    core.close(id);
                    return Err(refused("neochumbyd turned the channel down"));
                }
                Some(Phase::Opening) => {}
            }
            core = match self.0.wait_until(core, Some(deadline)) {
                Some(g) => g,
                None => {
                    let mut core = self.0.lock();
                    if core.epoch() == epoch {
                        core.close(id);
                        self.0.changed.notify_all();
                    }
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "neochumbyd didn't take the channel"));
                }
            };
        }
    }

    /// neochumbyd's side: the next channel the Chumby opened, or None once
    /// the line has gone.
    pub fn accept(&self) -> Option<Channel> {
        let mut core = self.0.lock();
        loop {
            if let Some(id) = core.accept() {
                return Some(Channel::new(self.0.clone(), id, core.epoch()));
            }
            if core.gone().is_some() {
                return None;
            }
            core = self.0.changed.wait(core).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Whether the other side is there and has acknowledged this one.
    pub fn established(&self) -> bool {
        self.0.lock().established()
    }

    /// Why the line has gone, if it has.
    pub fn gone(&self) -> Option<String> {
        self.0.lock().gone().map(str::to_string)
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits for a change until `deadline` (for ever if None): the guard
    /// back, or None once it has passed.
    fn wait_until<'a>(&self, g: MutexGuard<'a, Core>, deadline: Option<Instant>) -> Option<MutexGuard<'a, Core>> {
        let Some(deadline) = deadline else {
            return Some(self.changed.wait(g).unwrap_or_else(|e| e.into_inner()));
        };
        let left = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
        Some(self.changed.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0)
    }
}

/// The reader: frames in, until the stream ends.
fn reader(shared: &Shared, mut r: impl Read, log: impl Fn(&str)) {
    let mut buf = vec![0u8; 4096];
    let why = loop {
        match r.read(&mut buf) {
            Ok(0) => break "the other end closed it".to_string(),
            Ok(n) => {
                let events = {
                    let mut core = shared.lock();
                    core.input(&buf[..n]);
                    core.events()
                };
                events.iter().for_each(|e| log(e));
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => break e.to_string(),
        }
        shared.changed.notify_all();
    };
    shared.lock().end(why);
    shared.changed.notify_all();
}

/// The writer: frames out, until the line has gone.
fn writer(shared: &Shared, mut w: impl Write) {
    loop {
        let f = {
            let mut core = shared.lock();
            loop {
                if core.gone().is_some() {
                    return;
                }
                if let Some(f) = core.output(Instant::now()) {
                    break f;
                }
                let at = core.wake_at();
                core = match shared.wait_until(core, at) {
                    Some(g) => g,
                    None => shared.lock(),
                };
            }
        };
        if let Err(e) = w.write_all(&f).and_then(|()| w.flush()) {
            shared.lock().end(e.to_string());
            shared.changed.notify_all();
            return;
        }
    }
}

/// One connection on a [`Line`]: a [`Stream`] as a TCP connection is.
/// Reads and writes work through `&Channel` too, so two threads can each
/// take a direction. Dropped, it ends the channel.
pub struct Channel {
    shared: Arc<Shared>,
    id: u16,
    epoch: u64,
    read_timeout: Mutex<Option<Duration>>,
    write_timeout: Mutex<Option<Duration>>,
}

impl Channel {
    fn new(shared: Arc<Shared>, id: u16, epoch: u64) -> Channel {
        Channel { shared, id, epoch, read_timeout: Mutex::new(None), write_timeout: Mutex::new(None) }
    }

    fn deadline(t: &Mutex<Option<Duration>>) -> Option<Instant> {
        t.lock().unwrap_or_else(|e| e.into_inner()).map(|t| Instant::now() + t)
    }

    /// Runs `op` on this channel's core until it stops saying WouldBlock,
    /// waiting for changes in between, until the deadline.
    fn wait_for<T>(
        &self,
        timeout: &Mutex<Option<Duration>>,
        why: &str,
        mut op: impl FnMut(&mut Core) -> io::Result<T>,
    ) -> io::Result<T> {
        let deadline = Channel::deadline(timeout);
        let mut core = self.shared.lock();
        loop {
            if core.epoch() != self.epoch {
                return Err(core.gone_error());
            }
            match op(&mut core) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                r => {
                    self.shared.changed.notify_all();
                    return r;
                }
            }
            core = match self.shared.wait_until(core, deadline) {
                Some(g) => g,
                None => return Err(io::Error::new(io::ErrorKind::WouldBlock, why.to_string())),
            };
        }
    }

    /// Ends the channel: what was written still goes, then CLOSE.
    pub fn close(&self) {
        let mut core = self.shared.lock();
        if core.epoch() == self.epoch {
            core.close(self.id);
            self.shared.changed.notify_all();
        }
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.close();
    }
}

impl Read for &Channel {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.wait_for(&self.read_timeout, "nothing came in time", |c| c.read(self.id, buf))
    }
}

impl Write for &Channel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.wait_for(&self.write_timeout, "the other side took nothing in time", |c| c.write(self.id, buf))
    }

    /// What was written is on its way: the line's writer sends it in order.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for Channel {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
}

impl Write for Channel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for Channel {
    fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        *self.read_timeout.lock().unwrap_or_else(|e| e.into_inner()) = t;
        Ok(())
    }

    fn read_timeout(&self) -> io::Result<Option<Duration>> {
        Ok(*self.read_timeout.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn set_write_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        *self.write_timeout.lock().unwrap_or_else(|e| e.into_inner()) = t;
        Ok(())
    }
}

fn sync_frame(me: u32, peer: Option<u32>, established: bool) -> Vec<u8> {
    let mut p = Vec::with_capacity(SYNC_LEN);
    p.extend_from_slice(MAGIC);
    p.extend_from_slice(&me.to_be_bytes());
    p.extend_from_slice(&peer.unwrap_or(0).to_be_bytes());
    p.push(u8::from(established));
    frame(SYNC, 0, &p)
}

fn reset() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionReset, "the line started again")
}

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionRefused, why.to_string())
}

/// A new incarnation, never 0 (which a `SYNC` sends for none heard): from
/// /dev/urandom where there is one, which never waits (the system's
/// randomness can, on a Chumby at boot), mixed with the clock, the process
/// and a count, so that two differ even without it.
fn incarnation() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let mut b = [0u8; 8];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let mut x = u64::from_be_bytes(b)
        ^ (t.as_nanos() as u64)
        ^ (u64::from(std::process::id()) << 32)
        ^ u64::from(COUNT.fetch_add(1, Ordering::Relaxed)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    // splitmix64's finish.
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    ((x ^ (x >> 31)) as u32).max(1)
}

fn frame(kind: u8, id: u16, p: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(HEADER + p.len() + TRAILER);
    f.push(kind);
    f.extend_from_slice(&id.to_be_bytes());
    f.extend_from_slice(&(p.len() as u16).to_be_bytes());
    f.extend_from_slice(p);
    let crc = crc32(&f);
    f.extend_from_slice(&crc.to_be_bytes());
    f
}

/// A `SYNC`'s (sender, sender's peer, sender established).
fn parse_sync(p: &[u8]) -> Option<(u32, u32, bool)> {
    let rest = p.strip_prefix(MAGIC)?;
    let (them, rest) = rest.split_first_chunk::<4>()?;
    let (heard, rest) = rest.split_first_chunk::<4>()?;
    let [acked] = rest else { return None };
    let them = u32::from_be_bytes(*them);
    (them != 0 && *acked <= 1).then_some((them, u32::from_be_bytes(*heard), *acked == 1))
}

/// The largest payload any frame has.
const MAX_PAYLOAD: usize = if MAX_DATA > SYNC_LEN { MAX_DATA } else { SYNC_LEN };

/// Frames off the line. Out of step, it looks for a `SYNC` and drops
/// everything before one.
#[derive(Default)]
struct Decoder {
    buf: Vec<u8>,
    in_step: bool,
}

/// A frame that can't be: the line is out of step.
struct OutOfStep;

impl Decoder {
    fn push(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// The next whole frame, None until one has come.
    fn next(&mut self) -> Result<Option<(u8, u16, Vec<u8>)>, OutOfStep> {
        if !self.in_step {
            return Ok(self.hunt());
        }
        let Some(head) = self.buf.first_chunk::<HEADER>() else { return Ok(None) };
        let (kind, id, len) =
            (head[0], u16::from_be_bytes([head[1], head[2]]), usize::from(u16::from_be_bytes([head[3], head[4]])));
        if len > MAX_PAYLOAD {
            self.in_step = false;
            return Err(OutOfStep);
        }
        let Some(whole) = self.buf.get(..HEADER + len + TRAILER) else { return Ok(None) };
        let (body, crc) = whole.split_at(HEADER + len);
        if crc32(body).to_be_bytes() != crc {
            self.in_step = false;
            return Err(OutOfStep);
        }
        let p = body[HEADER..].to_vec();
        self.buf.drain(..HEADER + len + TRAILER);
        Ok(Some((kind, id, p)))
    }

    /// Looks for a whole `SYNC` with a good CRC, dropping what comes
    /// before it; finding one puts the decoder back in step.
    fn hunt(&mut self) -> Option<(u8, u16, Vec<u8>)> {
        let mut prefix = [0u8; HEADER + MAGIC.len()];
        prefix[3..5].copy_from_slice(&(SYNC_LEN as u16).to_be_bytes());
        prefix[HEADER..].copy_from_slice(MAGIC);
        let whole = HEADER + SYNC_LEN + TRAILER;
        loop {
            let Some(at) = self.buf.windows(prefix.len()).position(|w| w == prefix) else {
                // Keep what could be the start of one.
                let keep = self.buf.len().min(prefix.len() - 1);
                self.buf.drain(..self.buf.len() - keep);
                return None;
            };
            self.buf.drain(..at);
            let f = self.buf.get(..whole)?;
            let (body, crc) = f.split_at(HEADER + SYNC_LEN);
            if crc32(body).to_be_bytes() == crc {
                let p = body[HEADER..].to_vec();
                self.buf.drain(..whole);
                self.in_step = true;
                return Some((SYNC, 0, p));
            }
            self.buf.drain(..1);
        }
    }
}

/// CRC-32 (IEEE 802.3, as zlib's).
fn crc32(b: &[u8]) -> u32 {
    !b.iter().fold(!0u32, |c, &x| CRC_TABLE[((c ^ u32::from(x)) & 0xff) as usize] ^ (c >> 8))
}

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

#[cfg(all(test, unix))]
mod tests;
