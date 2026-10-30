//! AirPlay's own frontend, in front when AirPlay is opened: the
//! now-playing screen in text mode (neochumby_airplay::screen draws it),
//! on the terminal the session gives it. It connects to AirPlay's service
//! (neochumby::service), which tells it what's playing, and sends the
//! service what its buttons do. Touches come as mouse reports on stdin. It
//! exits on its [x] (or `q`), and the session opens the start app.
//!
//! It's an example of a frontend as much as AirPlay's own: another app's
//! does the same, with the owner's approval (CONTRIBUTING.md, "Writing an
//! AirPlay frontend").

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use neochumby::service;
use neochumby_airplay::screen::{self, Action, Grid, Input, Inputs, Layout};
use neochumby_airplay_proto::frontend::{NowPlaying, ToFrontend, ToService};
use neochumby_airplay_proto::{APP, Play, Reader, send};
use neochumby_sys::{fd, pty};

/// A terminal that doesn't say its size: Spleen 5x8's.
const SIZE: (u16, u16) = (64, 30);
/// How often the screen is drawn while playing, for the time and the bar.
const TICK: Duration = Duration::from_millis(500);
/// How often a missing service is looked for.
const RETRY: Duration = Duration::from_secs(1);

/// The connection to AirPlay's service.
struct Service {
    sock: UnixStream,
    reader: Reader,
}

impl Service {
    fn connect() -> io::Result<Service> {
        let sock = service::connect(APP)?;
        sock.set_nonblocking(true)?;
        Ok(Service { sock, reader: Reader::default() })
    }

    /// What has come, without waiting; fails once the service has gone.
    fn messages(&mut self) -> io::Result<Vec<ToFrontend>> {
        let mut out = Vec::new();
        loop {
            match self.reader.next::<ToFrontend>(&mut self.sock) {
                Ok(Some(m)) => out.push(m),
                Ok(None) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(out),
                Err(e) => return Err(e),
            }
        }
    }
}

fn run() -> io::Result<()> {
    let stdin = File::from(io::stdin().as_fd().try_clone_to_owned()?);
    let (cols, rows) = pty::size(stdin.as_fd()).unwrap_or(SIZE);
    // A touch's report as it comes, not once a line ends.
    if let Err(e) = pty::raw(stdin.as_fd()) {
        eprintln!("neochumby-airplay-screen: raw mode: {e}");
    }
    let layout: Layout = screen::layout(cols, rows);
    let mut out = io::stdout().lock();
    out.write_all(screen::START.as_bytes())?;
    out.flush()?;

    let mut np = NowPlaying::default();
    let mut svc: Option<Service> = None;
    let mut next_try = Instant::now();
    let mut inputs = Inputs::default();
    let mut shown: Option<Grid> = None;
    let mut buf = [0u8; 256];
    let mut stdin = stdin;
    loop {
        let now = Instant::now();
        if svc.is_none() && now >= next_try {
            match Service::connect() {
                Ok(s) => svc = Some(s),
                Err(e) => {
                    if shown.is_none() {
                        eprintln!("neochumby-airplay-screen: AirPlay's service: {e}");
                    }
                    next_try = now + RETRY;
                }
            }
        }
        let gone = match svc.as_mut().map(Service::messages) {
            Some(Ok(ms)) => {
                ms.into_iter().for_each(|m| np.apply(m, now));
                false
            }
            Some(Err(_)) => true,
            None => false,
        };
        if gone {
            svc = None;
            np = NowPlaying::default();
            next_try = now;
        }

        let grid = screen::draw(&np, now, &layout, svc.is_some());
        let diff = grid.diff(shown.as_ref());
        if !diff.is_empty() {
            out.write_all(diff.as_bytes())?;
            out.flush()?;
        }
        shown = Some(grid);

        // Wait for a touch, the service, or the next tick.
        let wait = match (&svc, np.state.play) {
            (None, _) => RETRY,
            (Some(_), Play::Playing) => TICK,
            (Some(_), _) => Duration::from_secs(5),
        };
        let ready = match &svc {
            Some(s) => fd::readable(&[stdin.as_fd(), s.sock.as_fd()], wait.as_millis() as i32)?,
            None => fd::readable(&[stdin.as_fd()], wait.as_millis() as i32)?,
        };
        if !ready[0] {
            continue;
        }
        let n = stdin.read(&mut buf)?;
        if n == 0 {
            // The terminal has closed: the session is ending this app.
            return Ok(());
        }
        for input in inputs.push(&buf[..n]) {
            let action = match input {
                Input::Tap { col, row } => layout.hit(&np, col, row),
                Input::Key(c) => screen::key(c),
            };
            match action {
                Some(Action::Close) => {
                    out.write_all(screen::END.as_bytes())?;
                    out.flush()?;
                    return Ok(());
                }
                Some(Action::Control(c)) => {
                    eprintln!("neochumby-airplay-screen: {c:?}");
                    if let Some(s) = svc.as_mut() {
                        // Waiting for the room a control takes is fine.
                        let _ = s.sock.set_nonblocking(false);
                        let _ = send(&mut s.sock, &ToService::Control(c));
                        let _ = s.sock.set_nonblocking(true);
                    }
                }
                None => {}
            }
        }
    }
}

fn main() {
    neochumby::debug::install("neochumby-airplay-screen");
    if let Err(e) = run() {
        eprintln!("neochumby-airplay-screen: {e}");
        std::process::exit(1);
    }
}
