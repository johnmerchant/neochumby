//! A terminal on the screen, for programs written for one: a
//! pseudo-terminal (neochumby_sys::pty) whose output is drawn on [`Fb`] in
//! embedded-graphics' 5x8 font, [`COLS`] by [`ROWS`] (64 by 30), with box
//! drawing and block characters drawn to fit. It understands what programs
//! that print in colour and move the cursor use, curses' among them
//! (xterm's colours, cursor movement, erasing, scrolling regions, the
//! alternate screen, line drawing).
//!
//! There is no keyboard. The touchscreen is the mouse, for a program that
//! asks for one (xterm's mouse reporting, as curses' mousemask does): a
//! touch presses its left button on the character under it. To one that
//! doesn't, a tap can type something ([`run_tapping`]), such as the key
//! that quits it.
//!
//! The program stays in the app's process group, without a controlling
//! terminal of its own, so the escape hatch ends it with the app.

mod boxes;
mod grid;

use std::io::{self, Read, Write};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use neochumby_sys::pty;

use grid::{Button, CELL_H, CELL_W};
pub use grid::{COLS, ROWS};

use crate::fb::Fb;
use crate::input::{Touch, Touchscreen};

/// How often the screen is redrawn while output arrives, and the touchscreen
/// read.
const FRAME: Duration = Duration::from_millis(40);

/// Runs `cmd` on the terminal, the whole screen, until the terminal closes:
/// when the program and anything it started that kept it have exited. Its
/// output stays on the screen. `TERM` is `xterm-256color`.
pub fn run(cmd: Command) -> io::Result<ExitStatus> {
    run_tapping(cmd, b"")
}

/// [`run`], where a tap types `keys` while the program hasn't asked for
/// the mouse: `b"q"` quits top.
pub fn run_tapping(mut cmd: Command, keys: &[u8]) -> io::Result<ExitStatus> {
    let mut fb = Fb::open()?;
    let mut touch = Touchscreen::open();
    let (mut master, slave) = pty::open(COLS as u16, ROWS as u16)?;
    cmd.stdin(slave.try_clone()?).stdout(slave.try_clone()?).stderr(slave).env("TERM", "xterm-256color");
    let mut child = cmd.spawn()?;
    // With the Command go our copies of the slave: the terminal closes when
    // the program's do.
    drop(cmd);

    let mut grid = grid::Grid::new();
    let mut parser = vte::Parser::new();
    let mut buf = [0u8; 4096];
    let mut drawn = Instant::now();
    // The cell a touch is down on.
    let mut down: Option<(usize, usize)> = None;
    fb.clear(grid::BG);
    loop {
        let mut open = true;
        if pty::readable(&master, FRAME.as_millis() as u16)? {
            match master.read(&mut buf) {
                Ok(n @ 1..) => parser.advance(&mut grid, &buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // Closed: EIO on Linux, end of file elsewhere.
                _ => open = false,
            }
        }
        let mut typed = std::mem::take(&mut grid.replies);
        for t in touch.poll() {
            let (button, at) = match (t, down) {
                (Touch::Down { x, y, .. }, was) => {
                    let at = (x.max(0) as usize / CELL_W, y.max(0) as usize / CELL_H);
                    down = Some(at);
                    match was {
                        None => (Button::Press, at),
                        Some(was) if was != at => (Button::Drag, at),
                        Some(_) => continue,
                    }
                }
                (Touch::Up, Some(at)) => {
                    down = None;
                    (Button::Release, at)
                }
                (Touch::Up, None) => continue,
            };
            match grid.report(button, at.0, at.1) {
                Some(report) => typed.extend(report),
                None if grid.mouse.mode == 0 && button == Button::Release => typed.extend_from_slice(keys),
                None => {}
            }
        }
        if !typed.is_empty() && open {
            let _ = master.write_all(&typed);
        }
        if !open {
            // Nothing will be typed: once it is over, no cursor.
            grid.cursor_shown = false;
        }
        if !open || drawn.elapsed() >= FRAME {
            let Ok(()) = grid.draw(&mut fb);
            drawn = Instant::now();
        }
        if !open {
            break;
        }
    }
    child.wait()
}
