//! A terminal on the screen, for programs written for one: a
//! pseudo-terminal (neochumby_sys::pty) whose output is drawn in a 5x8
//! font, [`COLS`] by [`ROWS`] (64 by 30). It understands what programs that
//! print in colour and move the cursor use (xterm's colours, cursor
//! movement, erasing, scrolling regions), not mouse reporting, the
//! alternate screen or anything typed: there is no keyboard.
//!
//! The program stays in the app's process group, without a controlling
//! terminal of its own, so the escape hatch ends it with the app.

mod font;
mod grid;

use std::io::{self, Read, Write};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use neochumby_sys::pty;

pub use grid::{COLS, ROWS};

use crate::fb::Fb;

/// How often the screen is redrawn while output arrives.
const FRAME: Duration = Duration::from_millis(40);

/// Runs `cmd` on the terminal, the whole screen, until the terminal closes:
/// when the program and anything it started that kept it have exited. Its
/// output stays on the screen. `TERM` is `xterm-256color`.
pub fn run(mut cmd: Command) -> io::Result<ExitStatus> {
    let mut fb = Fb::open()?;
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
        if !grid.replies.is_empty() {
            let _ = master.write_all(&std::mem::take(&mut grid.replies));
        }
        if !open {
            // Nothing will be typed: once it is over, no cursor.
            grid.cursor_shown = false;
        }
        if !open || drawn.elapsed() >= FRAME {
            grid.draw(fb.pixels());
            drawn = Instant::now();
        }
        if !open {
            break;
        }
    }
    child.wait()
}
