//! The developer link (neochumby_proto::dev), on a paired Chumby: the
//! session's output also goes to its neochumbyd, which can ask it to
//! restart or for its screen, so working on a Chumby needs no shell on it.
//!
//! Everything the session, its apps and its services write to stdout and
//! stderr (they inherit both) arrives on a socket put in their place. Two
//! threads take it from there, with no lock between them: the kernel's
//! socket buffers are all they share, so nothing waits on a futex.
//!
//! - **The tee** writes it all to where stderr went before (the boot
//!   hook's log file) at once, and queues it a line at a time
//!   (neochumby_proto::dev::LogQueue). It hands lines to the link through a
//!   second socket that it never waits on: what the link isn't taking
//!   waits in the queue, and past its size the oldest lines are dropped,
//!   with a line saying how many. So no writer ever waits on the network.
//! - **The link** connects to the paired neochumbyd, checks its key, sends
//!   `Msg::Dev` and the lines as `Msg::Log`. It answers `Msg::Shot` with
//!   the framebuffer, and turns `Msg::Restart` into a flag the session's
//!   loop reads. When the connection drops it tries again, waiting longer
//!   each time up to five minutes.
//!
//! Only descriptors 1 and 2 are replaced. The link's own sockets are
//! close-on-exec, so programs the session runs get stdout and stderr (the
//! capture socket) and their request pipe, as child.rs passes them, and
//! nothing else of the link's.
//!
//! On a restart, the session ends its apps and services as for CHUMBY
//! MODE, puts stdout and stderr back ([`Link::release`]), and replaces
//! itself with the tree's `neochumby` (main.rs), so a tree deployed since
//! boot is what runs. The boot hook waits on the same process throughout,
//! and needs no change.

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use neochumby::conf::{self, Paired};
use neochumby::fb;
use neochumby_proto::dev::{BATCH_LINES, Backoff, Lines, LogQueue, shot_chunks};
use neochumby_proto::{Identity, Msg, PublicKey, Session, unhex_key};
use neochumby_sys::fd::{self, Stdio};

const CONNECT: Duration = Duration::from_secs(5);
/// A neochumbyd that takes nothing for this long is gone.
const WRITE: Duration = Duration::from_secs(10);
const FIRST_RETRY: Duration = Duration::from_secs(2);
const LAST_RETRY: Duration = Duration::from_secs(300);

/// The link, running.
pub struct Link {
    restart: Arc<AtomicBool>,
    /// Where stdout and stderr went before.
    out: OwnedFd,
    err: OwnedFd,
}

impl Link {
    /// Starts the link if this Chumby is paired; None if it isn't, or the
    /// link can't start (and then output goes where it always did).
    pub fn start() -> Option<Link> {
        let p = conf::paired()?;
        start(p).map_err(|e| eprintln!("neochumby: developer link: {e}")).ok()
    }

    /// The flag a `Msg::Restart` sets.
    pub fn restart_flag(&self) -> Arc<AtomicBool> {
        self.restart.clone()
    }

    /// Puts stdout and stderr back where they were, before the session
    /// replaces itself: the threads don't survive that.
    pub fn release(self) {
        let _ = io::stdout().flush();
        let _ = fd::redirect(Stdio::Out, self.out.as_fd());
        let _ = fd::redirect(Stdio::Err, self.err.as_fd());
    }
}

fn start(p: Paired) -> io::Result<Link> {
    let me = Identity::from_secret(&p.secret)?;
    let server_key = unhex_key(&p.server_key)?;
    let out = io::stdout().as_fd().try_clone_to_owned()?;
    let err = io::stderr().as_fd().try_clone_to_owned()?;
    let log = File::from(err.try_clone()?);
    let (capture, output) = UnixStream::pair()?;
    let (to_link, from_tee) = UnixStream::pair()?;
    // None of these may reach a program the session runs, which gets only
    // stdin, stdout, stderr and its request pipe (child.rs). The Chumby's
    // kernel predates SOCK_CLOEXEC and F_DUPFD_CLOEXEC; musl falls back to
    // fcntl, and so does this, as for the request pipes.
    for f in [out.as_fd(), err.as_fd(), log.as_fd(), capture.as_fd(), output.as_fd(), to_link.as_fd(), from_tee.as_fd()]
    {
        // With 0, 1 or 2 closed at start, a socket could take its number,
        // and redirecting would close it.
        if f.as_raw_fd() <= 2 {
            return Err(io::Error::other("stdin, stdout or stderr wasn't open"));
        }
        fd::set_cloexec(f, true)?;
    }
    to_link.set_nonblocking(true)?;
    capture.set_read_timeout(Some(Duration::from_millis(200)))?;
    from_tee.set_read_timeout(Some(Duration::from_millis(100)))?;

    std::thread::spawn(move || tee(capture, log, to_link));
    let _ = io::stdout().flush();
    fd::redirect(Stdio::Out, output.as_fd())?;
    if let Err(e) = fd::redirect(Stdio::Err, output.as_fd()) {
        let _ = fd::redirect(Stdio::Out, out.as_fd());
        return Err(e);
    }
    let restart = Arc::new(AtomicBool::new(false));
    let flag = restart.clone();
    std::thread::spawn(move || link(p, me, server_key, from_tee, flag));
    Ok(Link { restart, out, err })
}

/// The tee: never waits on anything but its input.
fn tee(mut capture: UnixStream, mut log: File, mut to_link: UnixStream) {
    let mut lines = Lines::default();
    let mut queue = LogQueue::default();
    // A line the link's socket took only part of.
    let mut part: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match capture.read(&mut buf) {
            // Every writer has gone: stdout and stderr were put back.
            Ok(0) => return,
            Ok(n) => {
                let _ = log.write_all(&buf[..n]);
                lines.push(&buf[..n], |l| queue.push(l));
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                ) => {}
            // Not expected; never stop reading, or every writer would wait.
            Err(_) => neochumby::sleep_ms(100),
        }
        loop {
            if part.is_empty() {
                let Some(l) = queue.pop() else { break };
                part = l.into_bytes();
                part.push(b'\n');
            }
            match to_link.write(&part) {
                Ok(n) => drop(part.drain(..n)),
                // Full (the link is away): the rest waits in the queue.
                Err(_) => break,
            }
        }
    }
}

/// The link: connects, serves, and connects again.
fn link(p: Paired, me: Identity, server_key: PublicKey, mut from_tee: UnixStream, restart: Arc<AtomicBool>) {
    let mut backoff = Backoff::new(FIRST_RETRY, LAST_RETRY);
    let mut lines = Lines::default();
    // Taken from the tee and not yet sent.
    let mut batch: Vec<String> = Vec::new();
    let mut failing = false;
    loop {
        match connect(&p, &me, &server_key) {
            Ok(mut s) => {
                backoff.reset();
                failing = false;
                let e = serve(&mut s, &mut from_tee, &mut lines, &mut batch, &restart);
                eprintln!("neochumby: developer link to {}: {e}", p.server);
            }
            // Said once each time it stops working, not on every try.
            Err(e) if !failing => {
                failing = true;
                eprintln!("neochumby: developer link to {}: {e}; trying again", p.server);
            }
            Err(_) => {}
        }
        neochumby::sleep_ms(backoff.next_wait().as_millis() as u32);
    }
}

fn connect(p: &Paired, me: &Identity, server_key: &PublicKey) -> io::Result<Session> {
    let addr = (p.server.as_str(), p.api_port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))?;
    let tcp = TcpStream::connect_timeout(&addr, CONNECT)?;
    // A neochumbyd gone without a word is noticed in about a minute.
    neochumby_sys::net::keepalive(&tcp, 30, 10, 3);
    tcp.set_read_timeout(Some(WRITE))?;
    tcp.set_write_timeout(Some(WRITE))?;
    let mut s = Session::connect(tcp, me)?;
    if s.peer() != server_key {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "not the paired neochumbyd's key"));
    }
    s.send(&Msg::Hello { name: conf::name() })?;
    match s.recv_timeout(WRITE)? {
        Some(Msg::Welcome) => {}
        Some(Msg::Unpaired) => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "no longer paired")),
        m => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected {m:?}"))),
    }
    // The settings: the session has them already (settings.rs).
    match s.recv_timeout(WRITE)? {
        Some(Msg::Config(_)) => {}
        m => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected {m:?}"))),
    }
    s.send(&Msg::Dev)?;
    Ok(s)
}

/// Sends the log and answers neochumbyd until the connection fails.
fn serve(
    s: &mut Session,
    from_tee: &mut UnixStream,
    lines: &mut Lines,
    batch: &mut Vec<String>,
    restart: &AtomicBool,
) -> io::Error {
    let mut buf = [0u8; 4096];
    loop {
        if batch.len() < BATCH_LINES
            && let Ok(n) = from_tee.read(&mut buf)
        {
            lines.push(&buf[..n], |l| batch.push(l));
        }
        while !batch.is_empty() {
            let n = batch.len().min(BATCH_LINES);
            if let Err(e) = s.send(&Msg::Log(batch[..n].to_vec())) {
                return e;
            }
            batch.drain(..n);
        }
        let r = match s.recv_timeout(Duration::from_millis(50)) {
            Ok(Some(Msg::Restart)) => {
                eprintln!("neochumby: neochumbyd asks for a restart");
                restart.store(true, Ordering::SeqCst);
                Ok(())
            }
            Ok(Some(Msg::Shot)) => shot(s),
            // Anything else is for a newer session.
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        };
        if let Err(e) = r {
            return e;
        }
    }
}

/// The framebuffer, as neochumby_proto::dev says.
fn shot(s: &mut Session) -> io::Result<()> {
    let screen = || -> io::Result<Vec<u8>> {
        let mut px = vec![0u8; fb::W * fb::H * 2];
        File::open("/dev/fb0")?.read_exact(&mut px)?;
        Ok(px)
    };
    match screen() {
        Ok(px) => {
            s.send(&Msg::Screen { width: fb::W as u16, height: fb::H as u16 })?;
            for c in shot_chunks(&px) {
                s.send(&Msg::Data(c.to_vec()))?;
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("neochumby: screenshot: /dev/fb0: {e}");
            s.send(&Msg::Screen { width: 0, height: 0 })
        }
    }
}
