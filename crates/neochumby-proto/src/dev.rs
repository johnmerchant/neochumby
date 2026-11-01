//! The developer link: a paired Chumby's session keeps one connection to
//! its neochumbyd for developing on it without a shell.
//!
//! - The session says Hello, is welcomed and sent its `Config`, then sends
//!   [`Msg::Dev`](crate::Msg::Dev). It keeps the connection, reconnecting
//!   with [`Backoff`] when it drops.
//! - **Log:** the session's stdout and stderr, which its apps and services
//!   inherit, go to its log file as before and, a line at a time, to
//!   neochumbyd as [`Msg::Log`](crate::Msg::Log). Lines are cut at
//!   [`MAX_LINE`] bytes. While neochumbyd can't be reached they wait in a
//!   [`LogQueue`] of at most [`QUEUE_LINES`]; past that the oldest are
//!   dropped, and a line saying how many takes their place. Nothing the
//!   session or an app writes ever waits on the network.
//! - **Restart:** neochumbyd sends [`Msg::Restart`](crate::Msg::Restart);
//!   the session ends the app in front and the services, then runs the tree's session binary again in its place.
//! - **Screenshot:** neochumbyd sends [`Msg::Shot`](crate::Msg::Shot); the
//!   session answers [`Msg::Screen`](crate::Msg::Screen), then the
//!   framebuffer as `Data` of at most [`SHOT_CHUNK`] bytes each.
//! - **Input**, only on a Chumby whose owner set `dev=on` on its neochumbyd
//!   ([`Settings::dev`](crate::Settings::dev)): neochumbyd sends [`Msg::Tap`](crate::Msg::Tap)
//!   or [`Msg::Button`](crate::Msg::Button), and the Chumby answers each
//!   with [`Msg::Done`](crate::Msg::Done), a tap once it has let go. A tap
//!   is written to the touchscreen's input device, so every reader sees it
//!   as a real touch; a button press goes where a real one would, except
//!   to an app that has the button itself (`button = true`), which reads
//!   the hardware and can't be reached. neochumbyd sends the Chumby's
//!   settings (`Msg::Config`) again before each, so the Chumby goes by
//!   what they are now. Without `dev=on`, both ends refuse: a pairing
//!   lets its server read the log and the screen, but drives nothing on
//!   the Chumby unless its owner asks for that.
//!
//! neochumbyd accepts this only from a paired Chumby, over the session the
//! pairing authenticates, and bounds everything that comes: see neochumbyd's
//! dev.rs.

use std::collections::VecDeque;
use std::time::Duration;

/// The longest log line kept, in bytes; longer ones are cut (at a
/// character boundary) and end in [`CUT`].
pub const MAX_LINE: usize = 512;
/// What a cut line ends with.
pub const CUT: &str = " [...]";
/// How many lines the Chumby keeps while neochumbyd can't be reached.
pub const QUEUE_LINES: usize = 500;
/// The most lines in one `Msg::Log`. With [`MAX_LINE`], one message stays
/// well under [`crate::MAX_MSG`].
pub const BATCH_LINES: usize = 64;
/// The most a screenshot's `Data` carries per message.
pub const SHOT_CHUNK: usize = 32768;
/// The largest screen neochumbyd takes: 640x480 (the Chumby's is 320x240).
pub const MAX_SHOT_PIXELS: usize = 640 * 480;
/// The longest a [`Msg::Tap`](crate::Msg::Tap) holds the screen.
pub const MAX_HOLD_MS: u16 = 10_000;
/// How long `neochumbyd button NAME tap` holds the button: a few of the
/// session's 50 ms looks at it.
pub const BUTTON_TAP_MS: u64 = 300;
/// `line` as it is sent and kept: at most [`MAX_LINE`] bytes, without
/// control characters (a tab stays), so a log printed on a terminal can't
/// drive it.
pub fn clean_line(line: &str) -> String {
    let mut out: String = line.chars().map(|c| if c.is_control() && c != '\t' { '?' } else { c }).collect();
    if out.len() > MAX_LINE {
        let mut end = MAX_LINE - CUT.len();
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str(CUT);
    }
    out
}

/// Splits bytes as they come into lines (a final '\r' dropped, invalid
/// UTF-8 replaced), cut by [`clean_line`]. A line growing past
/// [`MAX_LINE`] is cut and its rest skipped, so it never holds more than
/// that.
#[derive(Default)]
pub struct Lines {
    buf: Vec<u8>,
    /// The line arriving was already cut; skip to its end.
    skipping: bool,
}

impl Lines {
    /// Takes in `bytes`, handing each whole line to `each`.
    pub fn push(&mut self, bytes: &[u8], mut each: impl FnMut(String)) {
        for &b in bytes {
            if b == b'\n' {
                if !self.skipping {
                    each(self.take());
                }
                self.buf.clear();
                self.skipping = false;
            } else if !self.skipping {
                self.buf.push(b);
                if self.buf.len() > MAX_LINE * 4 {
                    // Four bytes a character at most: enough to cut from.
                    each(self.take());
                    self.buf.clear();
                    self.skipping = true;
                }
            }
        }
    }

    fn take(&self) -> String {
        let raw = self.buf.strip_suffix(b"\r").unwrap_or(&self.buf);
        clean_line(&String::from_utf8_lossy(raw))
    }
}

/// The Chumby's lines waiting for neochumbyd: at most [`QUEUE_LINES`],
/// dropping the oldest. Where lines were dropped, [`LogQueue::pop`] gives a
/// line saying how many first.
pub struct LogQueue {
    lines: VecDeque<String>,
    cap: usize,
    dropped: u64,
}

impl Default for LogQueue {
    fn default() -> LogQueue {
        LogQueue::new(QUEUE_LINES)
    }
}

impl LogQueue {
    pub fn new(cap: usize) -> LogQueue {
        LogQueue { lines: VecDeque::new(), cap: cap.max(1), dropped: 0 }
    }

    pub fn push(&mut self, line: String) {
        if self.lines.len() == self.cap {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(line);
    }

    /// The next line to send, oldest first.
    pub fn pop(&mut self) -> Option<String> {
        if self.dropped > 0 {
            let n = std::mem::take(&mut self.dropped);
            return Some(format!("neochumby: {n} log line{} dropped", if n == 1 { "" } else { "s" }));
        }
        self.lines.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.dropped == 0 && self.lines.is_empty()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }
}

/// How long to wait before connecting again: from `first`, doubling after
/// each failure up to `most`, and back to `first` once connected.
pub struct Backoff {
    first: Duration,
    most: Duration,
    next: Duration,
}

impl Backoff {
    pub fn new(first: Duration, most: Duration) -> Backoff {
        Backoff { first, most, next: first }
    }

    /// The wait before the next attempt.
    pub fn next_wait(&mut self) -> Duration {
        let wait = self.next;
        self.next = (self.next * 2).min(self.most);
        wait
    }

    pub fn reset(&mut self) {
        self.next = self.first;
    }
}

/// A screenshot as `Data` payloads, after its `Msg::Screen`.
pub fn shot_chunks(pixels: &[u8]) -> impl Iterator<Item = &[u8]> {
    pixels.chunks(SHOT_CHUNK)
}

/// neochumbyd's side of a screenshot: the `Data` after a `Msg::Screen`,
/// put together, refusing more than the screen said.
pub struct ShotIn {
    pub width: u16,
    pub height: u16,
    pixels: Vec<u8>,
}

impl ShotIn {
    /// For a `Msg::Screen`; None if no screen came, or too big a one.
    pub fn new(width: u16, height: u16) -> Option<ShotIn> {
        let n = width as usize * height as usize;
        (n > 0 && n <= MAX_SHOT_PIXELS).then(|| ShotIn { width, height, pixels: Vec::with_capacity(n * 2) })
    }

    fn size(&self) -> usize {
        self.width as usize * self.height as usize * 2
    }

    /// Takes one `Data`; Err if it goes past the screen's size, or is
    /// bigger than [`SHOT_CHUNK`].
    pub fn add(&mut self, d: &[u8]) -> Result<(), &'static str> {
        if d.len() > SHOT_CHUNK || self.pixels.len() + d.len() > self.size() {
            return Err("more screen than it said");
        }
        self.pixels.extend_from_slice(d);
        Ok(())
    }

    /// The pixels, once they have all come.
    pub fn done(&self) -> Option<&[u8]> {
        (self.pixels.len() == self.size()).then_some(&self.pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_split_cut_and_cleaned() {
        let mut l = Lines::default();
        let mut got = Vec::new();
        l.push(b"one\r\ntw", |s| got.push(s));
        l.push(b"o\n\x1b[31mred\n\xffx\n", |s| got.push(s));
        assert_eq!(got, ["one", "two", "?[31mred", "\u{fffd}x"]);

        got.clear();
        let long = "é".repeat(MAX_LINE * 3);
        l.push(long.as_bytes(), |s| got.push(s));
        l.push(b"\nafter\n", |s| got.push(s));
        assert_eq!(got.len(), 2);
        assert!(got[0].len() <= MAX_LINE && got[0].ends_with(CUT), "{}", got[0].len());
        assert_eq!(got[1], "after");

        // Exactly MAX_LINE fits; one more is cut.
        assert_eq!(clean_line(&"x".repeat(MAX_LINE)).len(), MAX_LINE);
        let cut = clean_line(&"x".repeat(MAX_LINE + 1));
        assert!(cut.len() <= MAX_LINE && cut.ends_with(CUT));
    }

    #[test]
    fn the_queue_drops_the_oldest_and_says_so() {
        let mut q = LogQueue::new(3);
        for i in 0..5 {
            q.push(format!("{i}"));
        }
        assert_eq!(q.len(), 3);
        let all: Vec<String> = std::iter::from_fn(|| q.pop()).collect();
        assert_eq!(all, ["neochumby: 2 log lines dropped", "2", "3", "4"]);
        assert!(q.is_empty());
        q.push("a".into());
        q.push("b".into());
        assert_eq!(q.pop().as_deref(), Some("a"));
    }

    #[test]
    fn backoff_doubles_to_its_limit() {
        let s = Duration::from_secs;
        let mut b = Backoff::new(s(1), s(5));
        let waits: Vec<_> = (0..5).map(|_| b.next_wait()).collect();
        assert_eq!(waits, [s(1), s(2), s(4), s(5), s(5)]);
        b.reset();
        assert_eq!(b.next_wait(), s(1));
    }

    #[test]
    fn a_shot_comes_together_and_no_bigger() {
        let pixels: Vec<u8> = (0..320 * 240 * 2).map(|i| i as u8).collect();
        let mut s = ShotIn::new(320, 240).unwrap();
        let mut n = 0;
        for c in shot_chunks(&pixels) {
            assert!(s.done().is_none());
            s.add(c).unwrap();
            n += 1;
        }
        assert_eq!(n, 5);
        assert_eq!(s.done(), Some(&pixels[..]));
        assert!(s.add(&[0]).is_err());
        assert!(ShotIn::new(0, 240).is_none());
        assert!(ShotIn::new(4096, 4096).is_none());
        assert!(ShotIn::new(2, 2).unwrap().add(&[0; 9]).is_err());
    }
}
