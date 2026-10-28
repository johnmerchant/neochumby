//! The request pipe: how an app or a service asks the session (neochumby)
//! to open another app, or to hand the Chumby back to its own software, and
//! how the session tells services which app is in front.
//!
//! The session runs every app and every service with
//! NEOCHUMBY_SESSION=<fd>: one end of a Unix socket pair, whose other end
//! the session holds. It carries lines of
//! text, each ending in '\n', at most [`MAX_LINE`] bytes. Each end ignores
//! lines it doesn't know, so lines can be added later without breaking
//! anything.
//!
//! What an app may ask:
//!
//!   open <app>     run that app next (an app name: apps/<app>/ in the tree)
//!   chumby-mode    end neochumby and hand the Chumby back to its software
//!
//! An app writes its request and then exits. The last request it made
//! counts. If it hasn't exited a few seconds later, the session ends it.
//!
//! A service's request ends the app in front first, so the session takes
//! it only from the start app's service, never over an app that says
//! `interrupt = false`, and not after the escape hatch until an app is
//! opened by hand. What the session tells a service:
//!
//!   front <app>    that app is now in front (also as the service starts)
//!
//! NEOCHUMBY_REASON, when set, says why the session opened the app. An app
//! ignores values it doesn't know.

use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd, RawFd};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;

use neochumby_proto::valid_name;
use neochumby_sys::fd;

/// The variable holding the pipe's descriptor.
pub const ENV: &str = "NEOCHUMBY_SESSION";
/// The variable saying why the app was opened.
pub const REASON_ENV: &str = "NEOCHUMBY_REASON";
/// The longest line either end reads. Longer ones are dropped.
pub const MAX_LINE: usize = 256;

/// What an app may ask of the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Run this app next.
    Open(String),
    /// End neochumby; the Chumby's own software carries on.
    ChumbyMode,
}

impl Request {
    /// The request a line holds, or None for a line this end doesn't know.
    pub fn parse(line: &str) -> Option<Request> {
        let mut words = line.split(' ');
        match (words.next()?, words.next(), words.next()) {
            ("open", Some(app), None) if valid_name(app) => Some(Request::Open(app.to_string())),
            ("chumby-mode", None, None) => Some(Request::ChumbyMode),
            _ => None,
        }
    }

    /// The request as a line, with its '\n'.
    pub fn line(&self) -> String {
        match self {
            Request::Open(app) => format!("open {app}\n"),
            Request::ChumbyMode => "chumby-mode\n".into(),
        }
    }
}

/// What the session tells a service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// This app is now in front.
    Front(String),
}

impl Notice {
    /// The notice a line holds, or None for a line this end doesn't know.
    pub fn parse(line: &str) -> Option<Notice> {
        match line.split_once(' ') {
            Some(("front", app)) if valid_name(app) => Some(Notice::Front(app.to_string())),
            _ => None,
        }
    }

    /// The notice as a line, with its '\n'.
    pub fn line(&self) -> String {
        match self {
            Notice::Front(app) => format!("front {app}\n"),
        }
    }
}

/// Why the session opened this app: NEOCHUMBY_REASON, if set.
pub fn reason() -> Option<String> {
    std::env::var(REASON_ENV).ok().filter(|r| !r.is_empty())
}

/// Splits what arrives into lines. A line longer than [`MAX_LINE`], or not
/// UTF-8, is dropped whole.
#[derive(Default)]
pub struct Lines {
    buf: Vec<u8>,
    /// The line arriving is already too long; skip to its end.
    overlong: bool,
}

impl Lines {
    /// The lines completed by `data`, without their '\n'.
    pub fn push(&mut self, mut data: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(i) = data.iter().position(|&b| b == b'\n') {
            self.take(&data[..i]);
            if !self.overlong
                && let Ok(s) = std::str::from_utf8(&self.buf)
            {
                out.push(s.to_string());
            }
            self.buf.clear();
            self.overlong = false;
            data = &data[i + 1..];
        }
        self.take(data);
        out
    }

    fn take(&mut self, part: &[u8]) {
        if self.overlong || self.buf.len() + part.len() > MAX_LINE {
            self.overlong = true;
            self.buf.clear();
        } else {
            self.buf.extend_from_slice(part);
        }
    }
}

/// One end of a request pipe: the app's, from [`Pipe::from_env`], or the
/// session's, from [`Pipe::pair`].
pub struct Pipe {
    sock: UnixStream,
    lines: Lines,
}

/// What [`Pipe::receive`] found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Received {
    /// The complete lines that had arrived.
    pub lines: Vec<String>,
    /// The other end has closed: nothing more will arrive.
    pub closed: bool,
}

impl Pipe {
    /// The pipe the session gave this process, or None if the session
    /// didn't start it (run by hand, or on a host).
    pub fn from_env() -> Option<Pipe> {
        let n: RawFd = std::env::var(ENV).ok()?.parse().ok()?;
        // A copy, so that a second call doesn't close the first's. Neither
        // is passed on to programs this one runs.
        let copy = File::from(fd::handed_over(n)?);
        if !copy.metadata().ok()?.file_type().is_socket() {
            return None;
        }
        Some(Pipe { sock: UnixStream::from(OwnedFd::from(copy)), lines: Lines::default() })
    }

    /// A new pipe: the session's end, which never waits, and the end to
    /// give to what it runs. Neither is inherited by what the session runs
    /// until it says so.
    pub fn pair() -> io::Result<(Pipe, UnixStream)> {
        let (mine, theirs) = UnixStream::pair()?;
        // UnixStream::pair asks for close-on-exec, which the Chumby's 2.6.16
        // kernel predates; musl falls back to fcntl, and so does this.
        fd::set_cloexec(mine.as_fd(), true)?;
        fd::set_cloexec(theirs.as_fd(), true)?;
        mine.set_nonblocking(true)?;
        Ok((Pipe { sock: mine, lines: Lines::default() }, theirs))
    }

    /// Asks the session to run `app` next. Exit after this.
    pub fn open(&mut self, app: &str) -> io::Result<()> {
        self.send(&Request::Open(app.to_string()).line())
    }

    /// Asks the session to hand the Chumby back to its software. Exit after
    /// this.
    pub fn chumby_mode(&mut self) -> io::Result<()> {
        self.send(&Request::ChumbyMode.line())
    }

    /// Writes a whole line, waiting for room if need be.
    pub fn send(&mut self, line: &str) -> io::Result<()> {
        self.sock.write_all(line.as_bytes())
    }

    /// Writes a whole line if there's room for it now, on the session's
    /// end; false if there isn't, or the other end has gone. A service that
    /// doesn't read can't hold the session up.
    pub fn try_send(&mut self, line: &str) -> bool {
        matches!(self.sock.write(line.as_bytes()), Ok(n) if n == line.len())
    }

    /// Whatever has arrived, without waiting.
    pub fn receive(&mut self) -> Received {
        let mut r = Received::default();
        let mut buf = [0u8; 512];
        loop {
            match fd::recv_nowait(self.sock.as_fd(), &mut buf) {
                Ok(0) => {
                    r.closed = true;
                    return r;
                }
                Ok(n) => r.lines.extend(self.lines.push(&buf[..n])),
                Err(e) => match e.kind() {
                    io::ErrorKind::Interrupted => {}
                    io::ErrorKind::WouldBlock => return r,
                    _ => {
                        r.closed = true;
                        return r;
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests() {
        assert_eq!(Request::parse("open doom"), Some(Request::Open("doom".into())));
        assert_eq!(Request::parse("chumby-mode"), Some(Request::ChumbyMode));
        for unknown in ["", "open", "open Doom", "open ../doom", "open doom now", "chumby-mode please", "close doom"] {
            assert_eq!(Request::parse(unknown), None, "{unknown:?}");
        }
        for r in [Request::Open("wx".into()), Request::ChumbyMode] {
            let line = r.line();
            assert_eq!(Request::parse(line.strip_suffix('\n').unwrap()), Some(r));
        }
    }

    #[test]
    fn notices() {
        assert_eq!(Notice::parse("front doom"), Some(Notice::Front("doom".into())));
        for unknown in ["front", "front ", "front Doom", "front doom now", "back doom"] {
            assert_eq!(Notice::parse(unknown), None, "{unknown:?}");
        }
        assert_eq!(Notice::Front("wx".into()).line(), "front wx\n");
    }

    #[test]
    fn lines() {
        let mut l = Lines::default();
        assert_eq!(l.push(b"open d"), Vec::<String>::new());
        assert_eq!(l.push(b"oom\nchumby-mode\nne"), ["open doom", "chumby-mode"]);
        assert_eq!(l.push(b"w x\n"), ["new x"]);
        // Too long, in one piece or several: dropped, and the next line kept.
        let long = vec![b'x'; MAX_LINE + 1];
        assert_eq!(l.push(&[&long[..], b"\nok\n"].concat()), ["ok"]);
        assert_eq!(l.push(&long[..10]), Vec::<String>::new());
        assert_eq!(l.push(&long), Vec::<String>::new());
        assert_eq!(l.push(b"\nok\n"), ["ok"]);
        // Exactly MAX_LINE is kept; bytes that aren't UTF-8 are dropped.
        assert_eq!(
            l.push(&[&long[..MAX_LINE], b"\n"].concat()),
            [String::from_utf8(long[..MAX_LINE].to_vec()).unwrap()]
        );
        assert_eq!(l.push(b"\xff\nok\n"), ["ok"]);
    }

    #[test]
    fn a_pipe_carries_lines_both_ways() {
        let (mut session, theirs) = Pipe::pair().unwrap();
        let mut app = Pipe { sock: theirs, lines: Lines::default() };
        assert_eq!(session.receive(), Received::default());
        app.open("doom").unwrap();
        app.send("something new\n").unwrap();
        app.chumby_mode().unwrap();
        let r = session.receive();
        assert_eq!(r.lines, ["open doom", "something new", "chumby-mode"]);
        assert!(!r.closed);
        session.send("hello\n").unwrap();
        assert!(session.try_send("front wx\n"));
        assert_eq!(app.receive().lines, ["hello", "front wx"]);
        // A reader that doesn't read: the writer isn't held up.
        let line = "x".repeat(MAX_LINE - 1) + "\n";
        let mut sent = 0;
        while session.try_send(&line) {
            sent += 1;
            assert!(sent < 100_000, "never full");
        }
        drop(app);
        assert!(session.receive().closed);
    }
}
