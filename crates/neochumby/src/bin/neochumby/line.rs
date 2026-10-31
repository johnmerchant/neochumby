//! The serial line to neochumbyd, on a Chumby whose conf says
//! `server=serial` (an emulated Chumby, which has no network; the device
//! is its second UART, neochumby_proto::server). The session owns the line
//! and shares it out: every connection to neochumby::line::SOCKET (from
//! the session itself, neochumby-home pairing, neochumby-sync) becomes a
//! channel on it (neochumby_proto::mux), and neochumbyd handles each as a
//! TCP connection, with the same TLS session inside. The line carries
//! bytes and nothing else: it needs no key, and adds no trust.
//!
//! One thread, around poll(2): it reads the line whenever there is
//! anything, so neochumbyd is never held up, and moves bytes between each
//! connection and its channel as the channel's window allows. std's
//! blocking waits use futex flags the stock 2.6.16 kernel lacks, so it
//! uses none. The socket and the device are close-on-exec, so a restart
//! (which replaces this process) starts the line afresh, and neochumbyd
//! sees a new incarnation and ends the old sessions.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::{Duration, Instant};

use neochumby::conf;
use neochumby::line::{DIR, SOCKET};
use neochumby_proto::mux::{Core, Phase};
use neochumby_proto::server::{SERIAL_DEVICES, Server};
use neochumby_sys::fd;
use neochumby_sys::serial::{self, Want};

/// How long a connection waits for neochumbyd to be on the line and take
/// its channel; then it is closed, as a TCP connect would be refused.
const OPEN_WAIT: Duration = Duration::from_secs(5);
/// What is read from a connection before its channel takes it.
const CHUNK: usize = 8192;

/// Starts the line if this Chumby's server is `serial` (as the boot hook
/// passed it on, else as /psp/neochumby.conf says). Logs why not if it
/// can't.
pub fn start() {
    let server =
        std::env::var("NEOCHUMBY_SERVER").ok().filter(|s| !s.is_empty()).or_else(|| conf::get(conf::CONF, "server"));
    let Some(Server::Serial(dev)) = server.as_deref().and_then(Server::parse) else { return };
    let dev = dev.or_else(|| SERIAL_DEVICES.iter().find(|d| Path::new(d).exists()).map(|d| d.to_string()));
    let Some(dev) = dev else {
        eprintln!("neochumby: serial line: none of {} is here", SERIAL_DEVICES.join(", "));
        return;
    };
    match open(&dev) {
        Ok((tty, listener)) => {
            eprintln!("neochumby: neochumbyd over the serial line {dev}");
            std::thread::spawn(move || run(tty, listener));
        }
        Err(e) => eprintln!("neochumby: serial line {dev}: {e}"),
    }
}

fn open(dev: &str) -> io::Result<(File, UnixListener)> {
    let tty = serial::open_raw(dev)?;
    std::fs::DirBuilder::new().mode(0o700).create(DIR).or_else(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => Ok(()),
        _ => Err(e),
    })?;
    // A session before this one (replaced by a restart) left its socket.
    let _ = std::fs::remove_file(SOCKET);
    let listener = UnixListener::bind(SOCKET)?;
    fd::set_cloexec(listener.as_fd(), true)?;
    listener.set_nonblocking(true)?;
    Ok((tty, listener))
}

/// One connection to the socket, and its channel once it has one.
struct Conn {
    sock: UnixStream,
    chan: Option<(u16, u64)>,
    since: Instant,
    /// From the connection, not yet taken by the channel.
    up: Vec<u8>,
    /// From the channel, not yet taken by the connection.
    down: Vec<u8>,
    /// The connection has nothing more to send.
    eof: bool,
}

/// What to do with a connection after looking at it.
enum Keep {
    Yes,
    No,
}

fn run(mut tty: File, listener: UnixListener) {
    let mut core = Core::chumby();
    let mut conns: Vec<Conn> = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let now = Instant::now();
        conns.retain_mut(|c| matches!(pump(&mut core, c, now, &mut buf), Keep::Yes));
        while let Some(f) = core.output(now) {
            if let Err(e) = tty.write_all(&f) {
                eprintln!("neochumby: serial line: {e}");
                neochumby::sleep_ms(1000);
                break;
            }
        }

        let mut wants = vec![
            Want { fd: tty.as_fd(), read: true, write: false },
            Want { fd: listener.as_fd(), read: true, write: false },
        ];
        for c in &conns {
            let open = c.chan.is_some_and(|(id, _)| core.phase(id) == Some(Phase::Open));
            wants.push(Want { fd: c.sock.as_fd(), read: open && c.up.is_empty() && !c.eof, write: !c.down.is_empty() });
        }
        // Wake for the next SYNC, a connection's wait running out, or a
        // channel whose window has opened (looked at every 50 ms).
        let mut wake = core.wake_at();
        for c in &conns {
            if c.chan.is_none_or(|(id, _)| core.phase(id) == Some(Phase::Opening)) {
                wake = Some(wake.map_or(c.since + OPEN_WAIT, |w| w.min(c.since + OPEN_WAIT)));
            }
        }
        let busy = conns.iter().any(|c| !c.up.is_empty());
        let mut ms = wake.map_or(1000, |w| w.saturating_duration_since(Instant::now()).as_millis().min(1000) as i32);
        if busy {
            ms = ms.min(50);
        }
        let ready = match serial::poll(&wants, ms) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("neochumby: serial line: poll: {e}");
                neochumby::sleep_ms(100);
                continue;
            }
        };
        drop(wants);

        if ready[0].read {
            match tty.read(&mut buf) {
                Ok(n) => {
                    core.input(&buf[..n]);
                    for e in core.events() {
                        eprintln!("neochumby: serial line: {e}");
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    eprintln!("neochumby: serial line: {e}");
                    neochumby::sleep_ms(1000);
                }
            }
        }
        if ready[1].read {
            while let Ok((sock, _)) = listener.accept() {
                if fd::set_cloexec(sock.as_fd(), true).is_ok() && sock.set_nonblocking(true).is_ok() {
                    conns.push(Conn {
                        sock,
                        chan: None,
                        since: Instant::now(),
                        up: Vec::new(),
                        down: Vec::new(),
                        eof: false,
                    });
                }
            }
        }
        for (c, r) in conns.iter_mut().zip(&ready[2..]) {
            if r.read && c.up.is_empty() && !c.eof {
                match c.sock.read(&mut buf) {
                    Ok(0) => c.eof = true,
                    Ok(n) => c.up.extend_from_slice(&buf[..n]),
                    Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                    Err(_) => c.eof = true,
                }
            }
        }
    }
}

/// Moves what can be moved between a connection and its channel.
fn pump(core: &mut Core, c: &mut Conn, now: Instant, buf: &mut [u8]) -> Keep {
    let Some((id, epoch)) = c.chan else {
        if core.established() {
            match core.open() {
                Ok(id) => c.chan = Some((id, core.epoch())),
                Err(_) => return Keep::No,
            }
        } else if now >= c.since + OPEN_WAIT {
            return Keep::No;
        }
        return Keep::Yes;
    };
    if core.epoch() != epoch {
        return Keep::No;
    }
    match core.phase(id) {
        Some(Phase::Open) => {}
        Some(Phase::Opening) if now < c.since + OPEN_WAIT => return Keep::Yes,
        _ => {
            core.close(id);
            return Keep::No;
        }
    }
    // Up: the connection to the line.
    while !c.up.is_empty() {
        match core.write(id, &c.up) {
            Ok(n) => drop(c.up.drain(..n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(_) => {
                core.close(id);
                return Keep::No;
            }
        }
    }
    if c.eof && c.up.is_empty() {
        core.close(id);
        return Keep::No;
    }
    // Down: the line to the connection.
    loop {
        if !c.down.is_empty() {
            match c.sock.write(&c.down) {
                Ok(n) => drop(c.down.drain(..n)),
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {
                    return Keep::Yes;
                }
                Err(_) => {
                    core.close(id);
                    return Keep::No;
                }
            }
            continue;
        }
        match core.read(id, buf) {
            Ok(0) => {
                // neochumbyd closed it: so does the connection, once it
                // has everything (down is empty here).
                core.close(id);
                return Keep::No;
            }
            Ok(n) => c.down.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Keep::Yes,
            Err(_) => {
                core.close(id);
                return Keep::No;
            }
        }
    }
}
