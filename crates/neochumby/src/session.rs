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
//!   speaker <v>    unmute the speaker at master volume v (0-100), for a
//!                  jailed app with `access = ["speaker"]`, which can't
//!   speaker off    mute it again and put the boot's volume back
//!   play <url>     play an http:// stream through the Chumby's own player
//!                  (btplay, run by the session as root), for an app with
//!                  `access = ["speaker"]`
//!   play off       stop it
//!   brightness <n> set the backlight, 0 (off) to 65535 (full): for the
//!                  start app, and its service, only
//!   music pause    pause, resume or toggle the default music app, from the
//!   music resume   start app or its service only: the session passes
//!   music toggle   `pause` or `resume` on to that app's service
//!
//! An app asks for the speaker while it plays (neochumby::audio), and
//! carries on: only `open` and `chumby-mode` are followed by its exit.
//! The same goes for `play`, `brightness` and `music`. For `toggle`, the
//! session keeps whether the music is playing, starting as playing.
//!
//! An app writes its request and then exits. The last request it made
//! counts. If it hasn't exited a few seconds later, the session ends it.
//!
//! A service doesn't exit after asking, and its request ends the app in
//! front first, so the session takes it only: over the start app from any
//! service, over another app from the start app's service alone (and
//! `chumby-mode` from that one alone); never over an app that says
//! `interrupt = false`, or one that has asked for something itself; and
//! not after the escape hatch until an app is opened by hand. What the
//! session tells a service:
//!
//!   front <app>    that app is now in front (also as the service starts)
//!
//! And what it tells the default music app's service, when the start app
//! asks (`music`):
//!
//!   pause          stop playing, and be ready to resume
//!   resume         play again
//!
//! What the session tells the start app, while it's in front:
//!
//!   button         the top button was pressed (show your main screen)
//!
//! A start app that doesn't read its pipe can ignore it. If the line can't
//! be written (the pipe is closed or full), the session opens the start
//! app again with NEOCHUMBY_REASON=button instead.
//!
//! NEOCHUMBY_REASON, when set, says why the session opened the app:
//! `button` when the top button did. An app ignores values it doesn't know.
//!
//! The session also tells what it runs, as it starts it:
//!
//!   NEOCHUMBY_NAME            the Chumby's name, as its neochumbyd knows it
//!   NEOCHUMBY_STICK_FREE_KIB  the free space on the stick, in KiB (unset
//!                             without a stick)
//!   NEOCHUMBY_STORAGE         the app's own directory that lasts across
//!                             boots, for an app with `access = ["storage"]`
//! NEOCHUMBY_MUSIC, when set, names this Chumby's default music app (an
//! app with `music = true`): a start app's MUSIC button opens it. It's
//! unset when there's none. The session sets it for everything it runs.

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
/// The variable holding the Chumby's name.
pub const NAME_ENV: &str = "NEOCHUMBY_NAME";
/// The variable holding the stick's free space, in KiB.
pub const STICK_FREE_ENV: &str = "NEOCHUMBY_STICK_FREE_KIB";
/// The variable holding the app's lasting directory.
pub const STORAGE_ENV: &str = "NEOCHUMBY_STORAGE";
/// The variable naming the default music app.
pub const MUSIC_ENV: &str = "NEOCHUMBY_MUSIC";
/// The longest line either end reads. Longer ones are dropped.
pub const MAX_LINE: usize = 256;
/// The longest URL `play` takes.
pub const MAX_URL: usize = 200;
/// The brightest the backlight goes.
pub const MAX_BRIGHTNESS: u32 = 65535;

/// What an app may ask of the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Run this app next.
    Open(String),
    /// End neochumby; the Chumby's own software carries on.
    ChumbyMode,
    /// Unmute the speaker at this master volume (0-100), or, with None,
    /// mute it and put back the volume it booted with: for a jailed app,
    /// which can't (neochumby::audio asks).
    Speaker(Option<u32>),
    /// Play this stream (an http:// URL, [`valid_url`]) through the
    /// Chumby's own player, or, with None, stop it.
    Play(Option<String>),
    /// Set the backlight, 0 to [`MAX_BRIGHTNESS`].
    Brightness(u32),
    /// Pause, resume or toggle the default music app (the start app, and
    /// its service, only).
    Music(Music),
}

/// What a `music` request asks of the default music app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Music {
    Pause,
    Resume,
    /// Pause if it's playing, else resume, as the session last left it.
    Toggle,
}

/// Whether `url` is one `play` takes: http:// (the stock player has no
/// TLS), a host, at most [`MAX_URL`] bytes, and only the characters a URL
/// may hold unescaped. The session passes it to the player as one argument,
/// never through a shell; this keeps out anything else besides.
pub fn valid_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else { return false };
    url.len() <= MAX_URL
        && !rest.is_empty()
        && !rest.starts_with(['/', '?', '#'])
        && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&b))
}

impl Request {
    /// The request a line holds, or None for a line this end doesn't know.
    pub fn parse(line: &str) -> Option<Request> {
        let mut words = line.split(' ');
        match (words.next()?, words.next(), words.next()) {
            ("open", Some(app), None) if valid_name(app) => Some(Request::Open(app.to_string())),
            ("chumby-mode", None, None) => Some(Request::ChumbyMode),
            ("speaker", Some("off"), None) => Some(Request::Speaker(None)),
            ("speaker", Some(v), None) => v.parse().ok().filter(|v| *v <= 100).map(|v| Request::Speaker(Some(v))),
            ("play", Some("off"), None) => Some(Request::Play(None)),
            ("play", Some(url), None) if valid_url(url) => Some(Request::Play(Some(url.to_string()))),
            ("brightness", Some(n), None) => n.parse().ok().filter(|n| *n <= MAX_BRIGHTNESS).map(Request::Brightness),
            ("music", Some("pause"), None) => Some(Request::Music(Music::Pause)),
            ("music", Some("resume"), None) => Some(Request::Music(Music::Resume)),
            ("music", Some("toggle"), None) => Some(Request::Music(Music::Toggle)),
            _ => None,
        }
    }

    /// The request as a line, with its '\n'.
    pub fn line(&self) -> String {
        match self {
            Request::Open(app) => format!("open {app}\n"),
            Request::ChumbyMode => "chumby-mode\n".into(),
            Request::Speaker(Some(v)) => format!("speaker {v}\n"),
            Request::Speaker(None) => "speaker off\n".into(),
            Request::Play(Some(url)) => format!("play {url}\n"),
            Request::Play(None) => "play off\n".into(),
            Request::Brightness(n) => format!("brightness {n}\n"),
            Request::Music(Music::Pause) => "music pause\n".into(),
            Request::Music(Music::Resume) => "music resume\n".into(),
            Request::Music(Music::Toggle) => "music toggle\n".into(),
        }
    }
}

/// What the session tells a service, or the start app in front.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// To services: this app is now in front.
    Front(String),
    /// To the start app in front: the top button was pressed.
    Button,
    /// To the default music app's service: stop playing, ready to resume.
    Pause,
    /// To the default music app's service: play again.
    Resume,
}

impl Notice {
    /// The notice a line holds, or None for a line this end doesn't know.
    pub fn parse(line: &str) -> Option<Notice> {
        match line.split_once(' ') {
            Some(("front", app)) if valid_name(app) => Some(Notice::Front(app.to_string())),
            None if line == "button" => Some(Notice::Button),
            None if line == "pause" => Some(Notice::Pause),
            None if line == "resume" => Some(Notice::Resume),
            _ => None,
        }
    }

    /// The notice as a line, with its '\n'.
    pub fn line(&self) -> String {
        match self {
            Notice::Front(app) => format!("front {app}\n"),
            Notice::Button => "button\n".into(),
            Notice::Pause => "pause\n".into(),
            Notice::Resume => "resume\n".into(),
        }
    }
}

/// Why the session opened this app: NEOCHUMBY_REASON, if set.
pub fn reason() -> Option<String> {
    std::env::var(REASON_ENV).ok().filter(|r| !r.is_empty())
}

/// The Chumby's name (NEOCHUMBY_NAME), if the session gave it.
pub fn name() -> Option<String> {
    std::env::var(NAME_ENV).ok().filter(|n| !n.is_empty())
}

/// The free space on the stick, in KiB, as the app started
/// (NEOCHUMBY_STICK_FREE_KIB); None without a stick.
pub fn stick_free_kib() -> Option<u64> {
    std::env::var(STICK_FREE_ENV).ok()?.parse().ok()
}

/// The app's own directory that lasts across boots (NEOCHUMBY_STORAGE),
/// for an app with `access = ["storage"]`.
pub fn storage() -> Option<std::path::PathBuf> {
    std::env::var_os(STORAGE_ENV).filter(|s| !s.is_empty()).map(Into::into)
}

/// This Chumby's default music app, to `open`: NEOCHUMBY_MUSIC, if set to
/// an app name. None when there's no music app (grey out a MUSIC button).
pub fn music() -> Option<String> {
    std::env::var(MUSIC_ENV).ok().filter(|m| valid_name(m))
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
    /// The rest of a line [`Pipe::try_send`] could write only part of.
    waiting: Vec<u8>,
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
        Some(Pipe { sock: UnixStream::from(OwnedFd::from(copy)), lines: Lines::default(), waiting: Vec::new() })
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
        Ok((Pipe { sock: mine, lines: Lines::default(), waiting: Vec::new() }, theirs))
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

    /// Asks the session to play `url` (an http:// stream), or with None to
    /// stop. Take the speaker first (neochumby::audio): the session stops
    /// the stream when another process takes it. Carry on after this.
    pub fn play(&mut self, url: Option<&str>) -> io::Result<()> {
        if let Some(u) = url
            && !valid_url(u)
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{u:?}: not a URL play takes")));
        }
        self.send(&Request::Play(url.map(str::to_string)).line())
    }

    /// Asks the session to set the backlight (0 to [`MAX_BRIGHTNESS`]): for
    /// the start app and its service. Carry on after this.
    pub fn brightness(&mut self, level: u32) -> io::Result<()> {
        self.send(&Request::Brightness(level.min(MAX_BRIGHTNESS)).line())
    }

    /// Asks the session to pause, resume or toggle the default music app:
    /// for a start app, or its service. Unlike `open`, carry on after it.
    pub fn music(&mut self, what: Music) -> io::Result<()> {
        self.send(&Request::Music(what).line())
    }

    /// Writes a whole line, waiting for room if need be.
    pub fn send(&mut self, line: &str) -> io::Result<()> {
        self.sock.write_all(line.as_bytes())
    }

    /// Writes a whole line if there's room for it now, on the session's
    /// end; false if there isn't, or the other end has gone. A service that
    /// doesn't read can't hold the session up.
    ///
    /// A line is never cut: if only part of it fits, the rest waits, and
    /// goes before anything else, as [`Pipe::flush`] finds room. While it
    /// waits, this writes nothing and returns false.
    pub fn try_send(&mut self, line: &str) -> bool {
        if !self.flush() {
            return false;
        }
        match self.sock.write(line.as_bytes()) {
            Ok(n) => {
                self.waiting.extend_from_slice(&line.as_bytes()[n..]);
                true
            }
            Err(_) => false,
        }
    }

    /// Writes what's left of a line [`Pipe::try_send`] began, as much as
    /// fits now; true once nothing is left.
    pub fn flush(&mut self) -> bool {
        while !self.waiting.is_empty() {
            match self.sock.write(&self.waiting) {
                Ok(n) if n > 0 => {
                    self.waiting.drain(..n);
                }
                _ => return false,
            }
        }
        true
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
        assert_eq!(Request::parse("music toggle"), Some(Request::Music(Music::Toggle)));
        for unknown in [
            "",
            "open",
            "open Doom",
            "open ../doom",
            "open doom now",
            "chumby-mode please",
            "close doom",
            "music",
            "music stop",
            "music pause now",
        ] {
            assert_eq!(Request::parse(unknown), None, "{unknown:?}");
        }
        let music = [Music::Pause, Music::Resume, Music::Toggle].map(Request::Music);
        for r in [
            Request::Open("wx".into()),
            Request::ChumbyMode,
            Request::Play(Some("http://ice1.somafm.com/groovesalad-128-mp3".into())),
            Request::Play(None),
            Request::Brightness(9000),
        ]
        .into_iter()
        .chain(music)
        {
            let line = r.line();
            assert_eq!(Request::parse(line.strip_suffix('\n').unwrap()), Some(r));
        }
        assert_eq!(Request::parse("brightness 0"), Some(Request::Brightness(0)));
        for unknown in ["brightness", "brightness 65536", "brightness -1", "brightness full", "play", "play  off"] {
            assert_eq!(Request::parse(unknown), None, "{unknown:?}");
        }
    }

    #[test]
    fn urls_play_takes() {
        for ok in ["http://a", "http://ice1.somafm.com:80/dronezone-128-mp3", "http://h/p?a=1&b=%20#x"] {
            assert!(valid_url(ok), "{ok}");
        }
        let long = format!("http://h/{}", "x".repeat(MAX_URL));
        for bad in [
            "https://h/s",
            "ftp://h",
            "http://",
            "http:///etc/passwd",
            "file:///etc/passwd",
            "-http://h",
            "http://h/a b",
            "http://h/`reboot`",
            "http://h/\"x\"",
            "http://h/a\\b",
            "http://h/<x>",
            "http://h/é",
            &long,
        ] {
            assert!(!valid_url(bad), "{bad}");
            assert_eq!(Request::parse(&format!("play {bad}")), None, "{bad}");
        }
    }

    #[test]
    fn notices() {
        assert_eq!(Notice::parse("front doom"), Some(Notice::Front("doom".into())));
        for unknown in ["front", "front ", "front Doom", "front doom now", "back doom"] {
            assert_eq!(Notice::parse(unknown), None, "{unknown:?}");
        }
        assert_eq!(Notice::Front("wx".into()).line(), "front wx\n");
        assert_eq!(Notice::parse("button"), Some(Notice::Button));
        assert_eq!(Notice::parse("button now"), None);
        assert_eq!(Notice::Button.line(), "button\n");
        for n in [Notice::Pause, Notice::Resume] {
            assert_eq!(Notice::parse(n.line().trim_end()), Some(n));
        }
        assert_eq!(Notice::parse("pause now"), None);
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
        let mut app = Pipe { sock: theirs, lines: Lines::default(), waiting: Vec::new() };
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
        // A reader that doesn't read: the writer isn't held up, and the
        // reader never gets a line cut short. Lines of a size that doesn't
        // divide the buffer, so the last one written is likely written in
        // part.
        let line = "x".repeat(MAX_LINE - 9) + "\n";
        let mut sent = 0;
        while session.try_send(&line) {
            sent += 1;
            assert!(sent < 100_000, "never full");
        }
        assert!(!session.try_send("front wx\n"), "written while a line waits");
        let mut got = Vec::new();
        while got.len() < sent {
            session.flush();
            got.extend(app.receive().lines);
        }
        assert!(session.flush());
        assert_eq!(got.len(), sent);
        assert!(got.iter().all(|l| l.len() == MAX_LINE - 9), "a line was cut");
        assert!(session.try_send("front wx\n"));
        assert_eq!(app.receive().lines, ["front wx"]);
        drop(app);
        assert!(session.receive().closed);
    }
}
