//! AirPlay's own frontend, in text mode: the now-playing screen as a grid
//! of characters ([`draw`]), what a touch or a key on it does
//! ([`Layout::hit`], [`Inputs`]), and the escape sequences that put the
//! grid on the terminal, only the rows that changed ([`Grid::diff`]).
//!
//! It draws with what every text-mode terminal here has: the Linux
//! console's escape sequences, its 16 colours (the owner's colour scheme)
//! for the text, xterm's 256 for the cover art, and the box-drawing lines
//! Spleen has in every size. The cover and the progress bar are spaces
//! with a background colour, not block characters, which Spleen's small
//! sizes lack and the dist kernels' console would show as `?`. Text
//! outside ISO 8859-1, which the fonts cover, shows as `?`, one for each
//! character, so lines still wrap and cut where they should.
//!
//! It fits any size from 30 by 12 characters; the manifest asks for
//! Spleen 6x12 (53 by 20), where a cell is twice as tall as it is wide, so
//! a cover twice as many cells across as down is square.

use std::fmt::Write;
use std::time::Instant;

use neochumby_airplay_proto::frontend::NowPlaying;
use neochumby_airplay_proto::{Control, Cover, Play};

/// A character's colour: the terminal's own, one of the 16 the owner's
/// scheme sets, or one of xterm's 256.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Ansi(u8),
    Index(u8),
}

/// One character on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
}

const BLANK: Cell = Cell { ch: ' ', fg: Color::Default, bg: Color::Default, bold: false };

/// How text is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Style {
    fg: Color,
    bg: Color,
    bold: bool,
}

const PLAIN: Style = Style { fg: Color::Default, bg: Color::Default, bold: false };
const DIM: Style = Style { fg: Color::Ansi(8), ..PLAIN };
const ACCENT: Color = Color::Ansi(5);

fn fg(c: u8) -> Style {
    Style { fg: Color::Ansi(c), ..PLAIN }
}

/// The screen, as characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    cells: Vec<Cell>,
}

impl Grid {
    pub fn new(cols: u16, rows: u16) -> Grid {
        Grid { cols, rows, cells: vec![BLANK; cols as usize * rows as usize] }
    }

    /// The cell at (`col`, `row`), counted from 0.
    pub fn get(&self, col: u16, row: u16) -> Cell {
        if col < self.cols && row < self.rows {
            self.cells[row as usize * self.cols as usize + col as usize]
        } else {
            BLANK
        }
    }

    fn set(&mut self, col: u16, row: u16, cell: Cell) {
        if col < self.cols && row < self.rows {
            self.cells[row as usize * self.cols as usize + col as usize] = cell;
        }
    }

    /// `s` from (`col`, `row`), at most `width` characters of it.
    fn text(&mut self, col: u16, row: u16, s: &str, width: u16, st: Style) {
        for (i, ch) in s.chars().take(width as usize).enumerate() {
            self.set(col + i as u16, row, Cell { ch, fg: st.fg, bg: st.bg, bold: st.bold });
        }
    }

    /// `n` spaces in background `bg`.
    fn fill(&mut self, col: u16, row: u16, n: u16, bg: Color) {
        for i in 0..n {
            self.set(col + i, row, Cell { bg, ..BLANK });
        }
    }

    /// A row's characters, for tests and logs.
    pub fn row_text(&self, row: u16) -> String {
        (0..self.cols).map(|c| self.get(c, row).ch).collect()
    }

    /// What puts this grid on a terminal that shows `prev`: each row that
    /// differs, whole. Every row, without `prev` or at another size.
    pub fn diff(&self, prev: Option<&Grid>) -> String {
        let prev = prev.filter(|p| p.cols == self.cols && p.rows == self.rows);
        let mut out = String::new();
        let w = self.cols as usize;
        for r in 0..self.rows {
            let row = &self.cells[r as usize * w..(r as usize + 1) * w];
            if prev.is_some_and(|p| &p.cells[r as usize * w..(r as usize + 1) * w] == row) {
                continue;
            }
            let _ = write!(out, "\x1b[{};1H", r + 1);
            let mut style = None;
            for c in row {
                let st = Style { fg: c.fg, bg: c.bg, bold: c.bold };
                if style != Some(st) {
                    out.push_str(&sgr(st));
                    style = Some(st);
                }
                out.push(c.ch);
            }
            out.push_str("\x1b[0m");
        }
        out
    }
}

/// The escape sequence for a style, from the terminal's defaults.
fn sgr(st: Style) -> String {
    let mut s = String::from("\x1b[0");
    if st.bold {
        s.push_str(";1");
    }
    for (c, base, bright, ext) in [(st.fg, 30, 90, 38), (st.bg, 40, 100, 48)] {
        match c {
            Color::Default => {}
            Color::Ansi(n) if n < 8 => {
                let _ = write!(s, ";{}", base + n as u16);
            }
            Color::Ansi(n) => {
                let _ = write!(s, ";{}", bright + (n as u16 & 7));
            }
            Color::Index(n) => {
                let _ = write!(s, ";{ext};5;{n}");
            }
        }
    }
    s.push('m');
    s
}

/// What the terminal is told as the screen starts: the alternate screen,
/// no cursor, no wrapping at the edge, cleared, and touches as SGR mouse
/// reports.
pub const START: &str = "\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[0m\x1b[2J\x1b[?1000h\x1b[?1006h";
/// And as it ends: all that undone.
pub const END: &str = "\x1b[?1006l\x1b[?1000l\x1b[0m\x1b[2J\x1b[?7h\x1b[?25h\x1b[?1049l";

/// Characters on the screen, counted from 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub col: u16,
    pub row: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.col && col < self.col + self.w && row >= self.row && row < self.row + self.h
    }
}

/// What a touch, or a key, does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Control(Control),
    /// Leave: the session opens the start app.
    Close,
}

/// The buttons along the bottom, from the left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Previous,
    PlayPause,
    Next,
    VolumeDown,
    VolumeUp,
}

const BUTTONS: [Button; 5] = [Button::Previous, Button::PlayPause, Button::Next, Button::VolumeDown, Button::VolumeUp];

impl Button {
    fn control(self) -> Control {
        match self {
            Button::Previous => Control::Previous,
            Button::PlayPause => Control::PlayPause,
            Button::Next => Control::Next,
            Button::VolumeDown => Control::VolumeDown,
            Button::VolumeUp => Control::VolumeUp,
        }
    }

    /// Its label, in `room` characters, while the sender is `play`ing.
    fn label(self, play: Play, room: u16) -> &'static str {
        let wide = room >= 5;
        match self {
            Button::Previous => "|<<",
            Button::PlayPause if play == Play::Playing => "||",
            Button::PlayPause => ">",
            Button::Next => ">>|",
            Button::VolumeDown if wide => "VOL -",
            Button::VolumeDown => "-",
            Button::VolumeUp if wide => "VOL +",
            Button::VolumeUp => "+",
        }
    }
}

/// Where everything goes on a screen of a size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub cols: u16,
    pub rows: u16,
    /// The way out, top right.
    pub close: Rect,
    /// The cover art, if there's room.
    pub cover: Option<Rect>,
    /// The track's title, artist, album and sender.
    pub info: Rect,
    pub status: u16,
    pub bar: u16,
    pub times: u16,
    buttons: Vec<(Rect, Button)>,
}

/// The smallest screen it draws on; smaller is drawn at this size, cut.
pub const MIN: (u16, u16) = (30, 12);

/// The layout of a `cols` by `rows` screen.
pub fn layout(cols: u16, rows: u16) -> Layout {
    let (cols, rows) = (cols.max(MIN.0), rows.max(MIN.1));
    let buttons_row = rows - 3;
    let times = rows - 5;
    let bar = rows - 6;
    let status = rows - 7;
    let info_h = status - 3;
    // A cell is about twice as tall as it is wide: a square cover is twice
    // as many cells across as down. It needs 20 columns left beside it.
    let cover_h = info_h.min(8);
    let cover = (cover_h >= 3 && cols >= cover_h * 2 + 24).then(|| Rect { col: 1, row: 2, w: cover_h * 2, h: cover_h });
    let info_col = cover.map_or(1, |c| c.col + c.w + 2);
    let info = Rect { col: info_col, row: 2, w: cols - info_col - 1, h: info_h };
    let gap = 1;
    let w = (cols - 2 - gap * (BUTTONS.len() as u16 - 1)) / BUTTONS.len() as u16;
    let used = w * BUTTONS.len() as u16 + gap * (BUTTONS.len() as u16 - 1);
    let left = (cols - used) / 2;
    let buttons = BUTTONS
        .iter()
        .enumerate()
        .map(|(i, b)| (Rect { col: left + i as u16 * (w + gap), row: buttons_row, w, h: 3 }, *b))
        .collect();
    Layout { cols, rows, close: Rect { col: cols - 5, row: 0, w: 5, h: 2 }, cover, info, status, bar, times, buttons }
}

impl Layout {
    /// What a touch at (`col`, `row`) does, with `np` showing: nothing on
    /// a button the sender can't take now.
    pub fn hit(&self, np: &NowPlaying, col: u16, row: u16) -> Option<Action> {
        if self.close.contains(col, row) {
            return Some(Action::Close);
        }
        let (rect, b) = self.buttons.iter().find(|(r, _)| r.contains(col, row))?;
        let _ = rect;
        enabled(np).then_some(Action::Control(b.control()))
    }
}

/// Whether the sender takes the buttons now.
fn enabled(np: &NowPlaying) -> bool {
    np.link && np.state.controls && np.state.play != Play::Stopped
}

/// What a key does: for a keyboard on a dist Chumby, and for testing.
pub fn key(c: char) -> Option<Action> {
    Some(match c {
        ' ' => Action::Control(Control::PlayPause),
        'n' => Action::Control(Control::Next),
        'p' => Action::Control(Control::Previous),
        '+' | '=' => Action::Control(Control::VolumeUp),
        '-' => Action::Control(Control::VolumeDown),
        'q' => Action::Close,
        _ => return None,
    })
}

/// `s` as the fonts can show it: each character outside ISO 8859-1, or
/// that a terminal would act on, as one `?`.
pub fn printable(s: &str) -> String {
    s.chars().map(|c| if c.is_control() || c > '\u{ff}' { '?' } else { c }).collect()
}

/// `s` in at most `width` characters: cut, with "..." where it was.
fn cut(s: &str, width: u16) -> String {
    let n = s.chars().count();
    if n <= width as usize {
        return s.to_string();
    }
    if width < 4 {
        return s.chars().take(width as usize).collect();
    }
    let mut out: String = s.chars().take(width as usize - 3).collect();
    out.truncate(out.trim_end().len());
    out + "..."
}

/// `s` in at most `lines` lines of `width`, broken at spaces where it
/// can be, the last cut.
fn wrap(s: &str, width: u16, lines: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() && out.len() + 1 < lines {
        let chars: Vec<char> = rest.chars().collect();
        if chars.len() <= width as usize {
            break;
        }
        // The last space that leaves the line within the width, else a
        // hard break.
        let at = chars[..=width as usize].iter().rposition(|&c| c == ' ').filter(|&i| i > 0).unwrap_or(width as usize);
        out.push(chars[..at].iter().collect::<String>().trim_end().to_string());
        let skip: usize = chars[..at].iter().map(|c| c.len_utf8()).sum();
        rest = rest[skip..].trim_start();
    }
    if !rest.is_empty() {
        out.push(cut(rest, width));
    }
    out
}

/// A time as "m:ss", or "h:mm:ss" from an hour.
fn clock(ms: u32) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// The nearest of xterm's 256 colours to 8-bit red, green and blue: its
/// 6x6x6 cube, or its grey ramp for a grey.
pub fn xterm256([r, g, b]: [u8; 3]) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |v: u8| {
        LEVELS.iter().enumerate().min_by_key(|(_, l)| (**l as i32 - v as i32).abs()).map_or(0, |(i, _)| i as u8)
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube = 16 + 36 * ri + 6 * gi + bi;
    let dist = |a: [u8; 3]| -> i32 { a.iter().zip([r, g, b]).map(|(x, y)| (*x as i32 - y as i32).pow(2)).sum() };
    let cube_rgb = [LEVELS[ri as usize], LEVELS[gi as usize], LEVELS[bi as usize]];
    // The grey ramp: 232 to 255, 8 to 238 in steps of 10.
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gi = ((avg.saturating_sub(3)) / 10).min(23) as u8;
    let grey = 8 + 10 * gi;
    if dist([grey, grey, grey]) < dist(cube_rgb) { 232 + gi } else { cube }
}

/// The cover, in `r`: a cell's background the average of the picture
/// under it.
fn draw_cover(g: &mut Grid, cover: &Cover, r: Rect) {
    if cover.is_empty() {
        // A frame where it goes.
        for row in 0..r.h {
            for col in 0..r.w {
                let edge = row == 0 || row == r.h - 1 || col == 0 || col == r.w - 1;
                let ch = match (row, col) {
                    (0, 0) => '┌',
                    (0, c) if c == r.w - 1 => '┐',
                    (rw, 0) if rw == r.h - 1 => '└',
                    (rw, c) if rw == r.h - 1 && c == r.w - 1 => '┘',
                    (0, _) => '─',
                    (rw, _) if rw == r.h - 1 => '─',
                    _ if edge => '│',
                    _ => ' ',
                };
                g.set(r.col + col, r.row + row, Cell { ch, ..Cell { fg: Color::Ansi(8), ..BLANK } });
            }
        }
        return;
    }
    let (cw, ch) = (cover.width as u32, cover.height as u32);
    // Fit the picture's shape in the cells, centred.
    let (w, h) = if cw * r.h as u32 * 2 >= ch * r.w as u32 {
        (r.w as u32, ((ch * r.w as u32) / (cw * 2)).max(1).min(r.h as u32))
    } else {
        (((cw * r.h as u32 * 2) / ch).max(1).min(r.w as u32), r.h as u32)
    };
    let (x0, y0) = (r.col as u32 + (r.w as u32 - w) / 2, r.row as u32 + (r.h as u32 - h) / 2);
    for cy in 0..h {
        for cx in 0..w {
            let (px0, px1) = (cx * cw / w, ((cx + 1) * cw / w).max(cx * cw / w + 1));
            let (py0, py1) = (cy * ch / h, ((cy + 1) * ch / h).max(cy * ch / h + 1));
            let mut sum = [0u32; 3];
            let mut n = 0;
            for py in py0..py1 {
                for px in px0..px1 {
                    let p = cover.rgb(px as u16, py as u16);
                    sum.iter_mut().zip(p).for_each(|(s, v)| *s += v as u32);
                    n += 1;
                }
            }
            let avg = sum.map(|s| (s / n.max(1)) as u8);
            g.set((x0 + cx) as u16, (y0 + cy) as u16, Cell { bg: Color::Index(xterm256(avg)), ..BLANK });
        }
    }
}

/// The screen for `np` at `now`, laid out as `l`; `connected`, whether
/// the screen has AirPlay's service.
pub fn draw(np: &NowPlaying, now: Instant, l: &Layout, connected: bool) -> Grid {
    let mut g = Grid::new(l.cols, l.rows);
    // The header: AIRPLAY and the receiver's name, and the way out.
    g.fill(0, 0, l.cols, ACCENT);
    let head = Style { fg: Color::Ansi(15), bg: ACCENT, bold: true };
    g.text(1, 0, "AIRPLAY", 7, head);
    if !np.name.is_empty() {
        let name = cut(&printable(&np.name), l.cols.saturating_sub(16));
        g.text(10, 0, &name, l.cols.saturating_sub(16), Style { bold: false, ..head });
    }
    g.text(l.close.col + 1, 0, "[x]", 3, head);

    let i = l.info;
    let ready = connected && np.link;
    if !ready || (np.state.play == Play::Stopped && np.track.is_empty()) {
        let (big, small) = if !connected {
            ("Starting...", "Waiting for AirPlay's service.".to_string())
        } else if !np.link {
            ("No receiver", "AirPlay needs a paired neochumbyd.".to_string())
        } else {
            let name = if np.name.is_empty() { "this Chumby".to_string() } else { printable(&np.name) };
            ("Ready for AirPlay", format!("Play to {name} from a phone or Mac."))
        };
        let top = l.rows / 2 - 2;
        let c = |s: &str| (l.cols.saturating_sub(s.chars().count() as u16)) / 2;
        g.text(c(big), top, big, l.cols, Style { bold: true, ..PLAIN });
        for (n, line) in wrap(&small, l.cols - 2, 2).iter().enumerate() {
            g.text(c(line), top + 2 + n as u16, line, l.cols - 2, DIM);
        }
    } else {
        if let Some(r) = l.cover {
            draw_cover(&mut g, &np.cover, r);
        }
        let mut row = i.row;
        let title = if np.track.title.is_empty() { "Unknown".to_string() } else { printable(&np.track.title) };
        let lines = if i.h >= 6 { 2 } else { 1 };
        for line in wrap(&title, i.w, lines) {
            g.text(i.col, row, &line, i.w, Style { bold: true, ..PLAIN });
            row += 1;
        }
        for (text, st) in [(&np.track.artist, fg(6)), (&np.track.album, PLAIN)] {
            if !text.is_empty() && row < i.row + i.h {
                g.text(i.col, row, &cut(&printable(text), i.w), i.w, st);
                row += 1;
            }
        }
        if !np.track.source.is_empty() && row + 1 < i.row + i.h {
            g.text(i.col, row + 1, &cut(&format!("from {}", printable(&np.track.source)), i.w), i.w, DIM);
        }
    }

    // The state, and the volume.
    let (word, st) = match (ready, np.state.play) {
        (false, _) | (true, Play::Stopped) => ("", PLAIN),
        (true, Play::Playing) => ("> PLAYING", Style { bold: true, ..fg(2) }),
        (true, Play::Paused) => ("|| PAUSED", Style { bold: true, ..fg(3) }),
    };
    g.text(1, l.status, word, 12, st);
    if ready && np.state.play != Play::Stopped {
        let right = match (np.state.controls, np.state.volume) {
            (false, _) => "no remote control".to_string(),
            (true, Some(v)) => format!("VOL {v}%"),
            (true, None) => String::new(),
        };
        let w = right.chars().count() as u16;
        g.text(l.cols.saturating_sub(w + 1), l.status, &right, w, DIM);
    }

    // The progress bar and the times.
    let bar_w = l.cols - 2;
    let (elapsed, duration) = (np.elapsed_ms(now), np.state.duration_ms);
    let filled = if ready && duration > 0 { (elapsed as u64 * bar_w as u64 / duration as u64) as u16 } else { 0 };
    // The rest of the bar as a line: the Linux console has no bright
    // backgrounds to show it as a block.
    g.text(1, l.bar, &"─".repeat(bar_w as usize), bar_w, DIM);
    g.fill(1, l.bar, filled.min(bar_w), ACCENT);
    if ready && np.state.play != Play::Stopped {
        g.text(1, l.times, &clock(elapsed), 10, PLAIN);
        let total = if duration > 0 { clock(duration) } else { "-:--".into() };
        let w = total.chars().count() as u16;
        g.text(l.cols - 1 - w, l.times, &total, w, PLAIN);
    }

    // The buttons.
    let on = enabled(np);
    for (r, b) in &l.buttons {
        let frame = if on { PLAIN } else { DIM };
        draw_box(&mut g, *r, frame);
        let label = b.label(np.state.play, r.w.saturating_sub(2));
        let lw = label.chars().count() as u16;
        let st = if on { Style { bold: true, ..PLAIN } } else { DIM };
        g.text(r.col + (r.w.saturating_sub(lw)) / 2, r.row + 1, label, r.w.saturating_sub(2), st);
    }
    g
}

/// A box of light lines around `r`.
fn draw_box(g: &mut Grid, r: Rect, st: Style) {
    if r.w < 2 || r.h < 2 {
        return;
    }
    let cell = |ch| Cell { ch, fg: st.fg, bg: st.bg, bold: false };
    for c in 0..r.w {
        let (top, bottom) = match c {
            0 => ('┌', '└'),
            c if c == r.w - 1 => ('┐', '┘'),
            _ => ('─', '─'),
        };
        g.set(r.col + c, r.row, cell(top));
        g.set(r.col + c, r.row + r.h - 1, cell(bottom));
    }
    for row in 1..r.h - 1 {
        g.set(r.col, r.row + row, cell('│'));
        g.set(r.col + r.w - 1, r.row + row, cell('│'));
    }
}

/// What arrives on the terminal, as it was typed: a touch (the left
/// button going down, as a mouse report) or a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Tap { col: u16, row: u16 },
    Key(char),
}

/// Reads the terminal's input as it comes, in whatever pieces: SGR mouse
/// reports (`ESC [ < b ; col ; row M`), X10 ones (`ESC [ M b col row`),
/// for a terminal that ignored the ask for SGR, and keys. Other escape
/// sequences (a keyboard's arrows) are skipped, as are moves, releases,
/// other buttons and the wheel.
#[derive(Default)]
pub struct Inputs {
    buf: Vec<u8>,
}

impl Inputs {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Input> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.buf.len() {
            let b = &self.buf[i..];
            if b[0] != 0x1b {
                if b[0].is_ascii() && !b[0].is_ascii_control() {
                    out.push(Input::Key(b[0] as char));
                }
                i += 1;
                continue;
            }
            match parse_escape(b) {
                Parsed::Partial => break,
                Parsed::Skip(n) => i += n,
                Parsed::Input(n, inp) => {
                    out.extend(inp);
                    i += n;
                }
            }
        }
        self.buf.drain(..i);
        // A sequence that never ends isn't one.
        if self.buf.len() > 64 {
            self.buf.clear();
        }
        out
    }
}

enum Parsed {
    Partial,
    Skip(usize),
    Input(usize, Option<Input>),
}

/// The escape sequence at the start of `b` (which starts with ESC).
fn parse_escape(b: &[u8]) -> Parsed {
    match b.get(1) {
        None => return Parsed::Partial,
        Some(b'[') => {}
        Some(_) => return Parsed::Skip(2),
    }
    match b.get(2) {
        None => Parsed::Partial,
        Some(b'M') => {
            let Some(&[button, col, row]) = b.get(3..6).map(|s| <&[u8; 3]>::try_from(s).expect("three")) else {
                return Parsed::Partial;
            };
            let button = button.wrapping_sub(32);
            let tap = (button & 0b110_0011 == 0)
                .then(|| Input::Tap { col: col.saturating_sub(33) as u16, row: row.saturating_sub(33) as u16 });
            Parsed::Input(6, tap)
        }
        Some(b'<') => {
            let Some(end) = b.iter().position(|&c| c == b'M' || c == b'm') else {
                return if b.len() > 32 { Parsed::Skip(b.len()) } else { Parsed::Partial };
            };
            let nums: Vec<u16> =
                std::str::from_utf8(&b[3..end]).unwrap_or("").split(';').filter_map(|n| n.parse().ok()).collect();
            let tap = match nums[..] {
                [button, col, row] if b[end] == b'M' && button & 0b110_0011 == 0 => {
                    Some(Input::Tap { col: col.saturating_sub(1), row: row.saturating_sub(1) })
                }
                _ => None,
            };
            Parsed::Input(end + 1, tap)
        }
        Some(_) => {
            // Any other CSI: parameters, then a final byte.
            match b[2..].iter().position(|c| (0x40..=0x7e).contains(c)) {
                Some(n) => Parsed::Skip(3 + n),
                None => Parsed::Partial,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use neochumby_airplay_proto::frontend::ToFrontend;
    use neochumby_airplay_proto::{State, Track};

    use super::*;

    fn playing() -> NowPlaying {
        let mut np = NowPlaying::default();
        let now = Instant::now();
        np.apply(ToFrontend::Link(true), now);
        np.apply(ToFrontend::Name("Kitchen Chumby".into()), now);
        np.apply(
            ToFrontend::Track(Track {
                title: "A Song With A Rather Long Title That Wraps".into(),
                artist: "The Band".into(),
                album: "The Album".into(),
                source: "Test iPhone".into(),
            }),
            now,
        );
        np.apply(
            ToFrontend::State(State {
                play: Play::Playing,
                controls: true,
                elapsed_ms: 83_000,
                duration_ms: 296_000,
                volume: Some(50),
            }),
            now,
        );
        np
    }

    fn find(g: &Grid, s: &str) -> Option<(u16, u16)> {
        (0..g.rows).find_map(|r| g.row_text(r).find(s).map(|c| (g.row_text(r)[..c].chars().count() as u16, r)))
    }

    #[test]
    fn the_layout_at_spleen_6x12() {
        let l = layout(53, 20);
        assert_eq!(l.cover, Some(Rect { col: 1, row: 2, w: 16, h: 8 }));
        assert_eq!(l.info, Rect { col: 19, row: 2, w: 33, h: 10 });
        assert_eq!((l.status, l.bar, l.times), (13, 14, 15));
        assert_eq!(l.buttons.len(), 5);
        // The buttons fill the bottom three rows, inside the edges.
        for (r, _) in &l.buttons {
            assert_eq!((r.row, r.h), (17, 3));
            assert!(r.col >= 1 && r.col + r.w <= 52, "{r:?}");
        }
        // Spleen 8x16: a smaller cover; the smallest screen: none.
        assert_eq!(layout(40, 15).cover, Some(Rect { col: 1, row: 2, w: 10, h: 5 }));
        assert_eq!(layout(30, 12).cover, None);
        // Below the smallest, the smallest.
        assert_eq!((layout(10, 5).cols, layout(10, 5).rows), MIN);
    }

    #[test]
    fn it_draws_what_is_playing() {
        let np = playing();
        let l = layout(53, 20);
        let g = draw(&np, np.at.unwrap(), &l, true);
        assert!(g.row_text(0).starts_with(" AIRPLAY  Kitchen Chumby"), "{:?}", g.row_text(0));
        assert!(g.row_text(0).ends_with("[x] "), "{:?}", g.row_text(0));
        // The title, wrapped at a space, then the artist and the album.
        assert_eq!(find(&g, "A Song With A Rather Long Title"), Some((19, 2)));
        assert_eq!(find(&g, "That Wraps"), Some((19, 3)));
        assert_eq!(find(&g, "The Band"), Some((19, 4)));
        assert_eq!(find(&g, "The Album"), Some((19, 5)));
        assert_eq!(find(&g, "from Test iPhone"), Some((19, 7)));
        assert_eq!(find(&g, "> PLAYING"), Some((1, 13)));
        assert_eq!(find(&g, "VOL 50%"), Some((45, 13)));
        assert_eq!(find(&g, "1:23"), Some((1, 15)));
        assert_eq!(find(&g, "4:56"), Some((48, 15)));
        // 83 of 296 seconds: 14 of the bar's 51 cells.
        let filled = (1..52).filter(|&c| g.get(c, 14).bg == ACCENT).count();
        assert_eq!(filled, 14);
        assert!(find(&g, "||").is_some_and(|(_, r)| r == 18), "pause, while playing");
        // No cover: a frame and a note.
        assert_eq!(g.get(1, 2).ch, '┌');
    }

    #[test]
    fn the_time_runs_on() {
        let np = playing();
        let l = layout(53, 20);
        let later = np.at.unwrap() + Duration::from_secs(10);
        assert!(find(&draw(&np, later, &l, true), "1:33").is_some());
    }

    #[test]
    fn ready_waiting_and_no_receiver() {
        let l = layout(53, 20);
        let now = Instant::now();
        let mut np = NowPlaying::default();
        assert!(find(&draw(&np, now, &l, false), "Starting...").is_some());
        assert!(find(&draw(&np, now, &l, true), "No receiver").is_some());
        np.apply(ToFrontend::Link(true), now);
        np.apply(ToFrontend::Name("Den".into()), now);
        let g = draw(&np, now, &l, true);
        assert!(find(&g, "Ready for AirPlay").is_some());
        assert!(find(&g, "Play to Den from a phone or Mac.").is_some());
        // Nothing to control: the buttons are dimmed and do nothing.
        let (r, _) = l.buttons[1];
        assert_eq!(l.hit(&np, r.col + 1, r.row + 1), None);
        assert_eq!(g.get(r.col, r.row).fg, Color::Ansi(8));
    }

    #[test]
    fn touches_map_to_controls() {
        let np = playing();
        let l = layout(53, 20);
        let at = |i: usize| (l.buttons[i].0.col + 2, l.buttons[i].0.row + 1);
        let want = [Control::Previous, Control::PlayPause, Control::Next, Control::VolumeDown, Control::VolumeUp];
        for (i, c) in want.into_iter().enumerate() {
            let (col, row) = at(i);
            assert_eq!(l.hit(&np, col, row), Some(Action::Control(c)), "button {i}");
        }
        assert_eq!(l.hit(&np, 51, 0), Some(Action::Close));
        assert_eq!(l.hit(&np, 50, 1), Some(Action::Close));
        assert_eq!(l.hit(&np, 25, 8), None, "the title");
        // Without remote control from the sender, only the way out.
        let mut np = np;
        np.state.controls = false;
        assert_eq!(l.hit(&np, at(1).0, at(1).1), None);
        assert_eq!(l.hit(&np, 51, 0), Some(Action::Close));
        assert_eq!(key(' '), Some(Action::Control(Control::PlayPause)));
        assert_eq!(key('q'), Some(Action::Close));
    }

    #[test]
    fn mouse_reports_in_pieces() {
        let mut i = Inputs::default();
        // SGR: down at column 10, row 5 (from 1), then its release.
        assert_eq!(i.push(b"\x1b[<0;10;"), []);
        assert_eq!(i.push(b"5M\x1b[<0;10;5m"), [Input::Tap { col: 9, row: 4 }]);
        // X10: down at column 3, row 2; a release (3) is nothing.
        assert_eq!(i.push(&[0x1b, b'[', b'M', 32, 35, 34]), [Input::Tap { col: 2, row: 1 }]);
        assert_eq!(i.push(&[0x1b, b'[', b'M', 35, 35, 34]), []);
        // A move (32), the wheel (64), the right button: nothing.
        assert_eq!(i.push(b"\x1b[<32;1;1M\x1b[<64;1;1M\x1b[<2;1;1M"), []);
        // Keys, and an arrow skipped.
        assert_eq!(i.push(b"q\x1b[A n"), [Input::Key('q'), Input::Key(' '), Input::Key('n')]);
    }

    #[test]
    fn text_outside_latin1_is_a_question_mark_each() {
        assert_eq!(printable("Café 東京 🎵\n"), "Café ?? ??");
        assert_eq!(cut("Hello world", 8), "Hello...");
        assert_eq!(cut("Hello", 8), "Hello");
        assert_eq!(wrap("one two three four", 9, 2), ["one two", "three..."]);
        assert_eq!(wrap("abcdefghijkl", 5, 2), ["abcde", "fg..."]);
        assert_eq!(clock(83_000), "1:23");
        assert_eq!(clock(3_723_000), "1:02:03");
    }

    #[test]
    fn only_changed_rows_are_written() {
        let np = playing();
        let l = layout(53, 20);
        let t0 = np.at.unwrap();
        let a = draw(&np, t0, &l, true);
        let all = a.diff(None);
        assert_eq!(all.matches(";1H").count(), 20, "every row");
        assert!(all.starts_with("\x1b[1;1H"));
        // A second on: the bar and the time only.
        let b = draw(&np, t0 + Duration::from_secs(1), &l, true);
        let d = b.diff(Some(&a));
        assert!(d.contains("\x1b[16;1H"), "the times' row");
        assert!(!d.contains("\x1b[3;1H"), "not the title's");
        assert_eq!(b.diff(Some(&b)), "");
        assert_eq!(sgr(Style { fg: Color::Ansi(13), bg: Color::Index(196), bold: true }), "\x1b[0;1;95;48;5;196m");
    }

    #[test]
    fn the_cover_as_colours() {
        assert_eq!(xterm256([255, 0, 0]), 196);
        assert_eq!(xterm256([0, 0, 0]), 16);
        assert_eq!(xterm256([128, 128, 128]), 244);
        let mut np = playing();
        // Red on the left half, blue on the right.
        let mut px = Vec::new();
        for _y in 0..4 {
            for x in 0..4 {
                px.extend_from_slice(&(if x < 2 { 0xf800u16 } else { 0x001f }).to_le_bytes());
            }
        }
        np.apply(ToFrontend::Cover(Cover { width: 4, height: 4, rgb565: px }), Instant::now());
        let l = layout(53, 20);
        let g = draw(&np, Instant::now(), &l, true);
        assert_eq!(g.get(1, 2).bg, Color::Index(196));
        assert_eq!(g.get(16, 9).bg, Color::Index(21));
    }
}
