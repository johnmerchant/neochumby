//! The terminal's characters and what the program's escape sequences do to
//! them (vte parses them), and drawing them: no device of its own, so it
//! draws on any embedded-graphics DrawTarget of 320x240 (the screen, Fb).

use embedded_graphics::mono_font::MonoTextStyleBuilder;
use embedded_graphics::mono_font::iso_8859_1::FONT_5X8;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use embedded_graphics::text::{Baseline, Text};

use super::boxes;
use crate::fb::{self, rgb};

pub const CELL_W: usize = 5;
pub const CELL_H: usize = 8;
pub const COLS: usize = fb::W / CELL_W;
pub const ROWS: usize = fb::H / CELL_H;

const FG: u16 = rgb(204, 204, 204);
pub const BG: u16 = rgb(0, 0, 0);

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: char,
    fg: u16,
    bg: u16,
    underline: bool,
}

#[derive(Clone, Copy)]
enum Colour {
    Default,
    Index(u8),
    Rgb(u8, u8, u8),
}

/// What printing a character with the current SGR state gives it.
#[derive(Clone, Copy)]
struct Pen {
    fg: Colour,
    bg: Colour,
    bold: bool,
    reverse: bool,
    underline: bool,
}

const PEN: Pen = Pen { fg: Colour::Default, bg: Colour::Default, bold: false, reverse: false, underline: false };

/// xterm's 256 colours: its 16, a 6x6x6 cube, then 24 greys.
fn palette(n: u8) -> u16 {
    const BASIC: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    const LEVEL: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match n {
        0..16 => {
            let (r, g, b) = BASIC[n as usize];
            rgb(r, g, b)
        }
        16..232 => {
            let i = (n - 16) as usize;
            rgb(LEVEL[i / 36], LEVEL[i / 6 % 6], LEVEL[i % 6])
        }
        _ => {
            let v = 8 + 10 * (n - 232);
            rgb(v, v, v)
        }
    }
}

pub struct Grid {
    cells: Vec<Cell>,
    dirty: Vec<bool>,
    x: usize,
    y: usize,
    /// A character went in the last column: the next one wraps first.
    wrap_pending: bool,
    pen: Pen,
    saved: (usize, usize, Pen),
    /// The scrolling region, rows top..=bottom.
    top: usize,
    bottom: usize,
    autowrap: bool,
    pub(super) cursor_shown: bool,
    drawn_cursor: Option<usize>,
    /// Answers to the program's queries (cursor position, device
    /// attributes), for the caller to write back to it.
    pub replies: Vec<u8>,
}

impl Grid {
    pub fn new() -> Grid {
        let blank = Cell { ch: ' ', fg: FG, bg: BG, underline: false };
        Grid {
            cells: vec![blank; COLS * ROWS],
            dirty: vec![true; COLS * ROWS],
            x: 0,
            y: 0,
            wrap_pending: false,
            pen: PEN,
            saved: (0, 0, PEN),
            top: 0,
            bottom: ROWS - 1,
            autowrap: true,
            cursor_shown: true,
            drawn_cursor: None,
            replies: Vec::new(),
        }
    }

    /// Draws what changed since the last call on `target`, 320x240: the
    /// characters in embedded-graphics' 5x8 (misc-fixed, Latin-1), the box
    /// and block characters as [`boxes`] draws them.
    pub fn draw<D: DrawTarget<Color = Rgb565>>(&mut self, target: &mut D) -> Result<(), D::Error> {
        let cursor = self.cursor_shown.then_some(self.y * COLS + self.x);
        if self.drawn_cursor != cursor {
            for i in [self.drawn_cursor, cursor].into_iter().flatten() {
                self.dirty[i] = true;
            }
            self.drawn_cursor = cursor;
        }
        for i in 0..self.cells.len() {
            if !std::mem::take(&mut self.dirty[i]) {
                continue;
            }
            let mut c = self.cells[i];
            if Some(i) == cursor {
                std::mem::swap(&mut c.fg, &mut c.bg);
            }
            let at = Point::new((i % COLS * CELL_W) as i32, (i / COLS * CELL_H) as i32);
            let (fg, bg) = (Rgb565::from(RawU16::new(c.fg)), Rgb565::from(RawU16::new(c.bg)));
            if let Some(mut g) = boxes::glyph(c.ch) {
                if c.underline {
                    g[CELL_H - 1] = 0xf8;
                }
                let cell = Rectangle::new(at, Size::new(CELL_W as u32, CELL_H as u32));
                let bit = |p: usize| g[p / CELL_W] & (0x80 >> (p % CELL_W)) != 0;
                target.fill_contiguous(&cell, (0..CELL_W * CELL_H).map(|p| if bit(p) { fg } else { bg }))?;
            } else {
                let style = MonoTextStyleBuilder::new().font(&FONT_5X8).text_color(fg).background_color(bg).build();
                let mut utf8 = [0u8; 4];
                Text::with_baseline(c.ch.encode_utf8(&mut utf8), at, style, Baseline::Top).draw(target)?;
                // The font's own underline falls below the cell: the cell's
                // last row, as the box characters have it.
                if c.underline {
                    let row = Rectangle::new(at + Point::new(0, CELL_H as i32 - 1), Size::new(CELL_W as u32, 1));
                    target.fill_solid(&row, fg)?;
                }
            }
        }
        Ok(())
    }

    fn colour(c: Colour, default: u16, bright: bool) -> u16 {
        match c {
            Colour::Default => default,
            Colour::Index(n) if bright && n < 8 => palette(n + 8),
            Colour::Index(n) => palette(n),
            Colour::Rgb(r, g, b) => rgb(r, g, b),
        }
    }

    /// A blank in the current background, as erasing leaves (xterm's
    /// background colour erase).
    fn blank(&self) -> Cell {
        Cell { underline: false, ..self.cell(' ') }
    }

    fn cell(&self, ch: char) -> Cell {
        let p = self.pen;
        let mut fg = Self::colour(p.fg, FG, p.bold);
        let mut bg = Self::colour(p.bg, BG, false);
        if p.reverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        Cell { ch, fg, bg, underline: p.underline }
    }

    fn set(&mut self, i: usize, c: Cell) {
        if self.cells[i] != c {
            self.cells[i] = c;
            self.dirty[i] = true;
        }
    }

    /// Blanks cells `from..to`, indices into the whole screen.
    fn erase(&mut self, from: usize, to: usize) {
        let b = self.blank();
        for i in from..to.min(self.cells.len()) {
            self.set(i, b);
        }
    }

    fn goto(&mut self, x: usize, y: usize) {
        self.x = x.min(COLS - 1);
        self.y = y.min(ROWS - 1);
        self.wrap_pending = false;
    }

    /// Moves rows `top+n..=bottom` up to `top`, blanking the `n` below.
    fn scroll_up(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        for row in top..=bottom {
            if row + n <= bottom {
                for col in 0..COLS {
                    let c = self.cells[(row + n) * COLS + col];
                    self.set(row * COLS + col, c);
                }
            } else {
                self.erase(row * COLS, (row + 1) * COLS);
            }
        }
    }

    /// Moves rows `top..=bottom-n` down by `n`, blanking the `n` freed.
    fn scroll_down(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        for row in (top..=bottom).rev() {
            if row >= top + n {
                for col in 0..COLS {
                    let c = self.cells[(row - n) * COLS + col];
                    self.set(row * COLS + col, c);
                }
            } else {
                self.erase(row * COLS, (row + 1) * COLS);
            }
        }
    }

    fn linefeed(&mut self) {
        if self.y == self.bottom {
            self.scroll_up(self.top, self.bottom, 1);
        } else if self.y + 1 < ROWS {
            self.y += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.y == self.top {
            self.scroll_down(self.top, self.bottom, 1);
        } else {
            self.y = self.y.saturating_sub(1);
        }
    }

    fn sgr(&mut self, params: &vte::Params) {
        if params.is_empty() {
            self.pen = PEN;
            return;
        }
        let mut it = params.iter();
        while let Some(p) = it.next() {
            match p[0] {
                0 => self.pen = PEN,
                1 => self.pen.bold = true,
                22 => self.pen.bold = false,
                4 => self.pen.underline = true,
                24 => self.pen.underline = false,
                7 => self.pen.reverse = true,
                27 => self.pen.reverse = false,
                n @ 30..=37 => self.pen.fg = Colour::Index(n as u8 - 30),
                39 => self.pen.fg = Colour::Default,
                n @ 40..=47 => self.pen.bg = Colour::Index(n as u8 - 40),
                49 => self.pen.bg = Colour::Default,
                n @ 90..=97 => self.pen.fg = Colour::Index(n as u8 - 90 + 8),
                n @ 100..=107 => self.pen.bg = Colour::Index(n as u8 - 100 + 8),
                which @ (38 | 48) => {
                    // 38;5;N and 38;2;R;G;B, or the same with colons (one
                    // parameter, an optional colour space before R).
                    let c = if p.len() > 1 {
                        match p[1] {
                            5 if p.len() > 2 => Some(Colour::Index(p[2] as u8)),
                            2 if p.len() >= 5 => {
                                let v = &p[p.len() - 3..];
                                Some(Colour::Rgb(v[0] as u8, v[1] as u8, v[2] as u8))
                            }
                            _ => None,
                        }
                    } else {
                        let mut next = || it.next().map(|q| q[0] as u8);
                        match next() {
                            Some(5) => next().map(Colour::Index),
                            Some(2) => match (next(), next(), next()) {
                                (Some(r), Some(g), Some(b)) => Some(Colour::Rgb(r, g, b)),
                                _ => None,
                            },
                            _ => None,
                        }
                    };
                    match (which, c) {
                        (38, Some(c)) => self.pen.fg = c,
                        (_, Some(c)) => self.pen.bg = c,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

impl vte::Perform for Grid {
    fn print(&mut self, ch: char) {
        if self.wrap_pending {
            self.wrap_pending = false;
            self.x = 0;
            self.linefeed();
        }
        let c = self.cell(ch);
        self.set(self.y * COLS + self.x, c);
        if self.x + 1 < COLS {
            self.x += 1;
        } else if self.autowrap {
            self.wrap_pending = true;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => {
                self.x = self.x.saturating_sub(1);
                self.wrap_pending = false;
            }
            b'\t' => self.goto((self.x / 8 + 1) * 8, self.y),
            // The terminal's line discipline already turns "\n" into "\r\n".
            0x0a..=0x0c => {
                self.wrap_pending = false;
                self.linefeed();
            }
            b'\r' => self.goto(0, self.y),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, action: char) {
        let args: Vec<usize> = params.iter().map(|p| p[0] as usize).collect();
        // The nth argument, where 0 or absent means `default`.
        let arg = |n: usize, default: usize| args.get(n).copied().filter(|&v| v != 0).unwrap_or(default);
        let private = intermediates == b"?";
        let (x, y) = (self.x, self.y);
        match (private, action) {
            (false, 'A') => self.goto(x, y.saturating_sub(arg(0, 1))),
            (false, 'B' | 'e') => self.goto(x, y + arg(0, 1)),
            (false, 'C' | 'a') => self.goto(x + arg(0, 1), y),
            (false, 'D') => self.goto(x.saturating_sub(arg(0, 1)), y),
            (false, 'E') => self.goto(0, y + arg(0, 1)),
            (false, 'F') => self.goto(0, y.saturating_sub(arg(0, 1))),
            (false, 'G' | '`') => self.goto(arg(0, 1) - 1, y),
            (false, 'd') => self.goto(x, arg(0, 1) - 1),
            (false, 'H' | 'f') => self.goto(arg(1, 1) - 1, arg(0, 1) - 1),
            (false, 'J') => {
                let at = y * COLS + x;
                match args.first().copied().unwrap_or(0) {
                    0 => self.erase(at, COLS * ROWS),
                    1 => self.erase(0, at + 1),
                    _ => self.erase(0, COLS * ROWS),
                }
            }
            (false, 'K') => {
                let row = y * COLS;
                match args.first().copied().unwrap_or(0) {
                    0 => self.erase(row + x, row + COLS),
                    1 => self.erase(row, row + x + 1),
                    _ => self.erase(row, row + COLS),
                }
            }
            (false, 'X') => self.erase(y * COLS + x, y * COLS + (x + arg(0, 1)).min(COLS)),
            (false, 'P' | '@') => {
                let n = arg(0, 1).min(COLS - x);
                let row = y * COLS;
                let mut line: Vec<Cell> = self.cells[row + x..row + COLS].to_vec();
                let b = self.blank();
                if action == 'P' {
                    line.drain(..n);
                    line.extend(std::iter::repeat_n(b, n));
                } else {
                    line.splice(0..0, std::iter::repeat_n(b, n));
                    line.truncate(COLS - x);
                }
                for (i, c) in line.into_iter().enumerate() {
                    self.set(row + x + i, c);
                }
            }
            (false, 'L') if (self.top..=self.bottom).contains(&y) => self.scroll_down(y, self.bottom, arg(0, 1)),
            (false, 'M') if (self.top..=self.bottom).contains(&y) => self.scroll_up(y, self.bottom, arg(0, 1)),
            (false, 'S') => self.scroll_up(self.top, self.bottom, arg(0, 1)),
            (false, 'T') => self.scroll_down(self.top, self.bottom, arg(0, 1)),
            (false, 'm') => self.sgr(params),
            (false, 'r') => {
                let (top, bottom) = (arg(0, 1) - 1, arg(1, ROWS).min(ROWS) - 1);
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto(0, 0);
                }
            }
            (true, 'h' | 'l') => {
                let on = action == 'h';
                for &mode in &args {
                    match mode {
                        7 => self.autowrap = on,
                        25 => self.cursor_shown = on,
                        _ => {}
                    }
                }
            }
            (false, 'n') => match arg(0, 0) {
                5 => self.replies.extend_from_slice(b"\x1b[0n"),
                6 => self.replies.extend_from_slice(format!("\x1b[{};{}R", y + 1, x + 1).as_bytes()),
                _ => {}
            },
            // A VT102.
            (false, 'c') if intermediates.is_empty() => self.replies.extend_from_slice(b"\x1b[?6c"),
            (false, 's') => self.saved = (x, y, self.pen),
            (false, 'u') => {
                let (sx, sy, pen) = self.saved;
                self.pen = pen;
                self.goto(sx, sy);
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.saved = (self.x, self.y, self.pen),
            b'8' => {
                let (sx, sy, pen) = self.saved;
                self.pen = pen;
                self.goto(sx, sy);
            }
            b'D' => self.linefeed(),
            b'E' => {
                self.goto(0, self.y);
                self.linefeed();
            }
            b'M' => self.reverse_index(),
            b'c' => {
                let replies = std::mem::take(&mut self.replies);
                *self = Grid { replies, ..Grid::new() };
            }
            _ => {}
        }
    }
}
