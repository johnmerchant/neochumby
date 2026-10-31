//! Touches, as the terminal reports them: the session turns each into an
//! xterm mouse report on the app's stdin (SGR, `ESC [ < b ; col ; row M`
//! and `m` on release, once the app has asked for mode 1006; else X10,
//! `ESC [ M` and three bytes, its release button 3). Anything else typed
//! is read past.

/// A touch on a cell, counted from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    Down { col: usize, row: usize },
    Up { col: usize, row: usize },
}

/// What turns the terminal on for this: mouse presses and releases
/// (1000) as SGR reports (1006), no cursor, and a clear screen.
pub const START: &str = "\x1b[?1000h\x1b[?1006h\x1b[?25l\x1b[0m\x1b[2J";
/// And back.
pub const END: &str = "\x1b[?1006l\x1b[?1000l\x1b[0m\x1b[2J\x1b[H\x1b[?25h";

/// Reads reports from bytes as they come, in pieces of any size.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
}

/// The most a report can be; anything longer that hasn't ended is not one.
const MAX_REPORT: usize = 32;

impl Reader {
    /// The touches in `bytes` and what came before them.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Touch> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let Some(start) = self.buf.iter().position(|&b| b == 0x1b) else {
                self.buf.clear();
                break;
            };
            self.buf.drain(..start);
            match parse(&self.buf) {
                Parsed::Touch(t, n) => {
                    out.extend(t);
                    self.buf.drain(..n);
                }
                Parsed::NotOne(n) => {
                    self.buf.drain(..n.max(1));
                }
                Parsed::More if self.buf.len() > MAX_REPORT => {
                    self.buf.drain(..1);
                }
                Parsed::More => break,
            }
        }
        out
    }
}

enum Parsed {
    /// A report (or one that isn't a touch: another button, a drag), and
    /// its length.
    Touch(Option<Touch>, usize),
    /// Not a report: skip this many bytes.
    NotOne(usize),
    /// It may be one, once more comes.
    More,
}

/// The report at the start of `b`, which starts with ESC.
fn parse(b: &[u8]) -> Parsed {
    let prefix = |p: &[u8]| -> Option<bool> {
        if b.len() < p.len() {
            return (b == &p[..b.len()]).then_some(false);
        }
        Some(b.starts_with(p)).filter(|&y| y)
    };
    match prefix(b"\x1b[<") {
        Some(true) => return sgr(&b[3..]),
        Some(false) => return Parsed::More,
        None => {}
    }
    match prefix(b"\x1b[M") {
        Some(true) if b.len() >= 6 => {
            let (cb, cx, cy) = (b[3].wrapping_sub(32), b[4].wrapping_sub(32), b[5].wrapping_sub(32));
            let (col, row) = ((cx as usize).saturating_sub(1), (cy as usize).saturating_sub(1));
            let t = match cb & 0b1110_0011 {
                0 => Some(Touch::Down { col, row }),
                3 => Some(Touch::Up { col, row }),
                _ => None,
            };
            Parsed::Touch(t, 6)
        }
        Some(_) => Parsed::More,
        None => Parsed::NotOne(1),
    }
}

/// An SGR report's body, after `ESC [ <`: `b;col;row` and `M` or `m`.
fn sgr(body: &[u8]) -> Parsed {
    let Some(end) = body.iter().position(|&c| c == b'M' || c == b'm') else {
        return if body.iter().all(|c| c.is_ascii_digit() || *c == b';') { Parsed::More } else { Parsed::NotOne(1) };
    };
    let n = 3 + end + 1;
    let fields: Vec<Option<usize>> =
        body[..end].split(|&c| c == b';').map(|f| std::str::from_utf8(f).ok()?.parse().ok()).collect();
    let [Some(button), Some(col), Some(row)] = fields[..] else { return Parsed::NotOne(n) };
    let (col, row) = (col.saturating_sub(1), row.saturating_sub(1));
    // The left button (0) only: not a drag (32) or a wheel (64).
    let t = match (button, body[end]) {
        (0, b'M') => Some(Touch::Down { col, row }),
        (0, b'm') => Some(Touch::Up { col, row }),
        _ => None,
    };
    Parsed::Touch(t, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_reports() {
        let mut r = Reader::default();
        assert_eq!(
            r.push(b"\x1b[<0;21;7M\x1b[<0;21;7m"),
            [Touch::Down { col: 20, row: 6 }, Touch::Up { col: 20, row: 6 }]
        );
        // Drags and other buttons are read past.
        assert_eq!(r.push(b"\x1b[<32;3;3M\x1b[<64;1;1M"), []);
    }

    #[test]
    fn x10_reports() {
        let mut r = Reader::default();
        // Button 0 at column 1, row 1; then the release (button 3).
        assert_eq!(r.push(b"\x1b[M !!\x1b[M#!!"), [Touch::Down { col: 0, row: 0 }, Touch::Up { col: 0, row: 0 }]);
    }

    #[test]
    fn reports_split_anywhere_and_keys_between() {
        let all = b"q\x1b[<0;5;9Mxx\x1b[A\x1b[<0;5;9m";
        for cut in 0..all.len() {
            let mut r = Reader::default();
            let mut got = r.push(&all[..cut]);
            got.extend(r.push(&all[cut..]));
            assert_eq!(got, [Touch::Down { col: 4, row: 8 }, Touch::Up { col: 4, row: 8 }], "cut at {cut}");
        }
    }

    #[test]
    fn junk_does_not_stick() {
        let mut r = Reader::default();
        assert_eq!(r.push(b"\x1b[<99999999999999999999999999999999999"), []);
        assert_eq!(r.push(b"\x1b[<0;2;2M"), [Touch::Down { col: 1, row: 1 }]);
        assert_eq!(r.push(b"\x1b[<;;M\x1b[<0;1;1m"), [Touch::Up { col: 0, row: 0 }]);
    }
}
