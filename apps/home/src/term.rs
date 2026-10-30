//! Home's terminal: its stdin and stdout, which the session shows on the
//! screen (neochumby::term). Home draws on it with escape sequences, cell
//! by cell, and turns on xterm's mouse reports, SGR's (1000 and 1006), so
//! that a touch arrives on stdin as a press and a release on a cell.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;

use neochumby_sys::pty;

/// A colour, as red, green and blue.
pub type Rgb = (u8, u8, u8);

/// The size assumed when the terminal doesn't say (run by hand on a pipe):
/// Spleen 8x16's on the Chumby's 320x240.
const FALLBACK: (u16, u16) = (40, 15);

/// A touch, by the cell it is on, counted from 0.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Touch {
    /// Pressed, or moved while pressed, on (col, row).
    Down {
        col: usize,
        row: usize,
    },
    Up,
}

/// The terminal, set up for home: raw, the cursor hidden, the mouse on.
pub struct Term {
    input: Option<File>,
    out: io::Stdout,
    pub cols: usize,
    pub rows: usize,
    /// What came in but isn't a whole report yet.
    pending: Vec<u8>,
}

impl Term {
    /// Sets up the terminal on stdin and stdout. Run by hand, on something
    /// else than a terminal, it still draws, and sees no touches.
    pub fn open() -> Term {
        let input = io::stdin().as_fd().try_clone_to_owned().map(File::from).ok().filter(|f| pty::raw(f).is_ok());
        if input.is_none() {
            eprintln!("neochumby-home: stdin is no terminal: no touches");
        }
        let (cols, rows) = input.as_ref().and_then(pty::size).unwrap_or(FALLBACK);
        let mut t = Term { input, out: io::stdout(), cols: cols.into(), rows: rows.into(), pending: Vec::new() };
        t.show(b"\x1b[?25l\x1b[?1000h\x1b[?1006h");
        t
    }

    /// Writes `frame`, escapes and all, at once.
    pub fn show(&mut self, frame: &[u8]) {
        let _ = self.out.write_all(frame);
        let _ = self.out.flush();
    }

    /// The touches that have come, waiting up to `ms` milliseconds for one.
    pub fn poll(&mut self, ms: u16) -> Vec<Touch> {
        let Some(input) = &mut self.input else {
            neochumby::sleep_ms(ms.into());
            return Vec::new();
        };
        let mut buf = [0u8; 256];
        if pty::readable(input, ms).unwrap_or(false) {
            match input.read(&mut buf) {
                Ok(n @ 1..) => self.pending.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // The terminal has gone: the session is ending it.
                _ => std::process::exit(0),
            }
        }
        parse(&mut self.pending)
    }

    /// Forgets whatever came meanwhile: taps made while home was busy were
    /// someone else's.
    pub fn drain(&mut self) {
        while !self.poll(0).is_empty() {}
    }

    /// Puts the terminal back as it was, for whatever runs on it next: the
    /// mouse off, the cursor shown, the screen cleared.
    pub fn leave(&mut self) {
        self.show(b"\x1b[?1006l\x1b[?1000l\x1b[0m\x1b[2J\x1b[H\x1b[?25h");
    }
}

/// The touches in `pending`, the SGR mouse reports it holds whole taken
/// out (`ESC [ < B ; COL ; ROW M`, `m` for a release; COL and ROW from
/// 1). Anything else, a key, say, is dropped; a report cut short stays
/// for the rest.
pub fn parse(pending: &mut Vec<u8>) -> Vec<Touch> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < pending.len() {
        if pending[i] != 0x1b {
            i += 1;
            continue;
        }
        let rest = &pending[i..];
        if !b"\x1b[<".starts_with(&rest[..rest.len().min(3)]) {
            i += 1;
            continue;
        }
        let Some(end) = rest.iter().position(|&b| b == b'M' || b == b'm') else {
            // Cut short: the rest comes with the next read.
            break;
        };
        let body = std::str::from_utf8(&rest[3.min(rest.len())..end]).unwrap_or("");
        let n: Vec<usize> = body.split(';').filter_map(|v| v.parse().ok()).collect();
        if let [button, col, row] = n[..]
            && col >= 1
            && row >= 1
        {
            // The left button (0), pressed, released or dragged (32);
            // wheels (64) and other buttons are no touch.
            if button & !32 == 0 {
                out.push(if rest[end] == b'm' { Touch::Up } else { Touch::Down { col: col - 1, row: row - 1 } });
            }
        }
        i += end + 1;
    }
    pending.drain(..i.min(pending.len()));
    out
}

/// A frame being drawn: escape sequences and text, written at once with
/// [`Term::show`].
#[derive(Default)]
pub struct Paint(pub Vec<u8>);

impl Paint {
    /// A frame that starts by clearing the screen to the terminal's own
    /// background.
    pub fn cleared() -> Paint {
        Paint(b"\x1b[0m\x1b[2J".to_vec())
    }

    /// Moves to (col, row), counted from 0.
    pub fn at(&mut self, col: usize, row: usize) -> &mut Self {
        self.0.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
        self
    }

    /// Text in `fg` on `bg` (None: the terminal's own background), bold or
    /// not.
    pub fn text(&mut self, s: &str, fg: Rgb, bg: Option<Rgb>, bold: bool) -> &mut Self {
        let mut sgr = format!("\x1b[0;38;2;{};{};{}", fg.0, fg.1, fg.2);
        if let Some(b) = bg {
            sgr += &format!(";48;2;{};{};{}", b.0, b.1, b.2);
        }
        if bold {
            sgr += ";1";
        }
        self.0.extend_from_slice(sgr.as_bytes());
        self.0.extend_from_slice(b"m");
        self.0.extend_from_slice(s.as_bytes());
        self.0.extend_from_slice(b"\x1b[0m");
        self
    }

    /// A rectangle of `w` by `h` cells at (col, row), filled with `bg`.
    pub fn fill(&mut self, col: usize, row: usize, w: usize, h: usize, bg: Rgb) -> &mut Self {
        let blank = " ".repeat(w);
        for r in row..row + h {
            self.at(col, r).text(&blank, bg, Some(bg), false);
        }
        self
    }
}

/// `s`, cut to `w` characters.
pub fn fit(s: &str, w: usize) -> String {
    s.chars().take(w).collect()
}

/// A colour of the session's, RGB565 (an app's `color`), as red, green
/// and blue.
pub fn rgb565(c: u16) -> Rgb {
    let (r, g, b) = ((c >> 11) & 31, (c >> 5) & 63, c & 31);
    ((r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_become_touches() {
        let mut p = b"\x1b[<0;21;7M\x1b[<0;21;7m".to_vec();
        assert_eq!(parse(&mut p), [Touch::Down { col: 20, row: 6 }, Touch::Up]);
        assert!(p.is_empty());
        // A drag is a touch moving; a wheel and keys are nothing.
        let mut p = b"x\x1b[<32;3;4M\x1b[<64;1;1M\x1b[A".to_vec();
        assert_eq!(parse(&mut p), [Touch::Down { col: 2, row: 3 }]);
        // Cut short: kept for the rest.
        let mut p = b"\x1b[<0;5;".to_vec();
        assert_eq!(parse(&mut p), []);
        p.extend_from_slice(b"6M");
        assert_eq!(parse(&mut p), [Touch::Down { col: 4, row: 5 }]);
        assert!(p.is_empty());
    }

    #[test]
    fn colours_from_the_sessions() {
        assert_eq!(rgb565(0xffff), (255, 255, 255));
        assert_eq!(rgb565(neochumby::fb::rgb(255, 0, 0)), (255, 0, 0));
        assert_eq!(rgb565(0), (0, 0, 0));
    }
}
