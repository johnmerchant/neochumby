//! GET APPS on the Chumby: the terminal the session gave it, the touches
//! on it, and the companion, neochumbyd's catalog.
//!
//! The terminal is put in raw mode for as long as it runs (rustix), so that touch reports arrive as
//! they are typed and aren't echoed. Taps act on release, on the target
//! they were pressed on, which is lit while held.

use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use neochumby::companion::Typed;
use neochumby_proto::catalog::Catalog;
use rustix::termios::{self, OptionalActions};

use crate::input::{END, Reader, START, Touch};
use crate::ui::{Action, App, Effect};

/// The terminal's size if it doesn't say: Spleen 6x12's on 320x240.
const SIZE: (usize, usize) = (53, 20);
/// How long the loop waits for the server before looking at the screen.
const POLL: Duration = Duration::from_millis(40);
/// How often the spinner turns.
const SPIN: Duration = Duration::from_millis(250);

/// Runs GET APPS until it exits: its exit status.
pub fn run() -> i32 {
    let stdin = io::stdin();
    let saved = termios::tcgetattr(&stdin).ok();
    if let Some(t) = &saved {
        let mut raw = t.clone();
        raw.make_raw();
        if let Err(e) = termios::tcsetattr(&stdin, OptionalActions::Now, &raw) {
            eprintln!("neochumby-catalog: raw mode: {e}");
        }
    }
    let size = termios::tcgetwinsize(&stdin)
        .ok()
        .filter(|w| w.ws_col >= 40 && w.ws_row >= 16)
        .map_or(SIZE, |w| (w.ws_col as usize, w.ws_row as usize));
    eprintln!("neochumby-catalog: a terminal of {}x{}", size.0, size.1);

    let mut out = io::stdout().lock();
    let _ = out.write_all(START.as_bytes());
    let status = Screen { size, out: &mut out }.run();
    let _ = out.write_all(END.as_bytes());
    let _ = out.flush();
    if let Some(t) = &saved {
        let _ = termios::tcsetattr(&stdin, OptionalActions::Now, t);
    }
    status
}

struct Screen<'a, W: Write> {
    size: (usize, usize),
    out: &'a mut W,
}

impl<W: Write> Screen<'_, W> {
    fn run(&mut self) -> i32 {
        let (cols, rows) = self.size;
        let (tx, keys) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            let mut stdin = io::stdin().lock();
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });

        let mut app = App::new();
        let mut link = None;
        let mut pending = connect(&mut app, &mut link);
        let mut reader = Reader::default();
        let mut shown = String::new();
        let mut spun = Instant::now();
        loop {
            for e in std::mem::take(&mut pending) {
                match e {
                    Effect::Exit => return 0,
                    Effect::Connect => pending.extend(connect(&mut app, &mut link)),
                    Effect::Send(ask) => {
                        if let Some(l) = link.as_mut()
                            && let Err(e) = Typed::<Catalog>::send(l, &ask)
                        {
                            app.gone(&e.to_string());
                            link = None;
                        }
                    }
                }
            }
            if !pending.is_empty() {
                continue;
            }

            let frame = app.draw(cols, rows);
            let ansi = frame.ansi();
            if ansi != shown {
                let _ = self.out.write_all(ansi.as_bytes());
                let _ = self.out.flush();
                shown = ansi;
            }

            match link.as_mut().map(|l| l.recv_timeout(POLL)) {
                Some(Ok(Some(a))) => pending.extend(app.answer(a, rows)),
                Some(Ok(None)) => {}
                Some(Err(e)) => {
                    eprintln!("neochumby-catalog: {e}");
                    app.gone(&e.to_string());
                    link = None;
                }
                None => std::thread::sleep(POLL),
            }
            loop {
                match keys.try_recv() {
                    Ok(bytes) => {
                        for t in reader.push(&bytes) {
                            pending.extend(touch(&mut app, &frame, t, rows));
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    // No terminal any more: nothing to show it on.
                    Err(mpsc::TryRecvError::Disconnected) => return 0,
                }
            }
            if app.busy() && spun.elapsed() >= SPIN {
                app.tick += 1;
                spun = Instant::now();
            }
        }
    }
}

/// Opens the companion, neochumbyd's catalog: what to ask first.
fn connect(app: &mut App, link: &mut Option<Typed<Catalog>>) -> Vec<Effect> {
    match neochumby::companion::open(neochumby::companion!()) {
        Ok(l) => {
            *link = Some(l.typed::<Catalog>());
            app.connected()
        }
        Err(e) => {
            eprintln!("neochumby-catalog: neochumbyd: {e}");
            app.gone(&e.to_string());
            Vec::new()
        }
    }
}

/// A touch on `frame`: lights the target pressed, and acts on its
/// release over the same one.
fn touch(app: &mut App, frame: &crate::screen::Frame<Action>, t: Touch, rows: usize) -> Vec<Effect> {
    match t {
        Touch::Down { col, row } => {
            app.pressed = frame.hit(col, row);
            Vec::new()
        }
        Touch::Up { col, row } => match app.pressed.take() {
            Some(a) if frame.hit(col, row) == Some(a) => app.act(a, rows),
            _ => Vec::new(),
        },
    }
}
