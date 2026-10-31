//! The terminal's characters and what the program's escape sequences do to
//! them (vte parses them), and drawing them: no device of its own, so it
//! draws on any embedded-graphics DrawTarget (the screen, Fb), in a console
//! font (font.rs) and a colour scheme (colors.rs). It understands what the
//! Linux console does, and what xterm adds that programs use.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

use super::boxes;
use super::colors::{Rgb, Scheme};
use super::font::Font;
use crate::fb::rgb;

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

/// What of a touch the program asked to see, by xterm's mouse reporting.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mouse {
    /// 9 (presses), 1000 (and releases), 1002 (and drags) or 1003 (and any
    /// motion, which a touch only has while pressed); 0, none.
    pub mode: u16,
    /// 1006: SGR's encoding, else X10's.
    pub sgr: bool,
}

impl Mouse {
    /// DECSET (`on`) or DECRST of private mode `mode`, if it is a mouse
    /// mode.
    pub fn set(&mut self, mode: u16, on: bool) {
        match mode {
            9 | 1000 | 1002 | 1003 => self.mode = if on { mode } else { 0 },
            1006 => self.sgr = on,
            _ => {}
        }
    }

    /// What the program is sent for a touch on cell (`col`, `row`), counted
    /// from 0, if it asked for touches like it.
    pub fn report(&self, button: Button, col: usize, row: usize) -> Option<Vec<u8>> {
        let wanted = match button {
            Button::Press => self.mode != 0,
            Button::Release => self.mode >= 1000,
            Button::Drag => self.mode >= 1002,
        };
        if !wanted {
            return None;
        }
        let code = if button == Button::Drag { 32 } else { 0 };
        let (col, row) = (col + 1, row + 1);
        Some(if self.sgr {
            let end = if button == Button::Release { 'm' } else { 'M' };
            format!("\x1b[<{code};{col};{row}{end}").into_bytes()
        } else {
            // X10's bytes stop at 223 columns and rows; the screen has far
            // fewer.
            let code = if button == Button::Release { 3 } else { code };
            let at = |n: usize| (32 + n.min(223)) as u8;
            vec![0x1b, b'[', b'M', 32 + code, at(col), at(row)]
        })
    }
}

/// A touch, as the mouse it is to the program: its left button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Press,
    Drag,
    Release,
}

/// DEC's special graphics (in G0 after "ESC ( 0", as curses' line drawing
/// is for xterm, or in G1, shifted in, as it is for the Linux console), '`'
/// to '~': what has no character of its own here is the nearest that has.
const GRAPHICS: [char; 31] = [
    '*', '▒', ' ', ' ', ' ', ' ', '°', '±', ' ', ' ', '┘', '┐', '┌', '└', '┼', '-', '-', '─', '_', '_', '├', '┤', '┴',
    '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
];

/// The 256 colours: the scheme's 16, then xterm's 6x6x6 cube and 24 greys.
fn palette(scheme: &Scheme) -> [u16; 256] {
    const LEVEL: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let mut p = [0u16; 256];
    for (n, c) in p.iter_mut().enumerate() {
        *c = match n {
            0..16 => to565(scheme.palette[n]),
            16..232 => {
                let i = n - 16;
                rgb(LEVEL[i / 36], LEVEL[i / 6 % 6], LEVEL[i % 6])
            }
            _ => {
                let v = 8 + 10 * (n - 232) as u8;
                rgb(v, v, v)
            }
        };
    }
    p
}

fn to565([r, g, b]: Rgb) -> u16 {
    rgb(r, g, b)
}

pub struct Grid {
    cols: usize,
    rows: usize,
    font: Font,
    /// The scheme's foreground and background, and the 256 colours.
    fg: u16,
    bg: u16,
    palette: [u16; 256],
    /// The scheme's cursor colour, unless it's the foreground: the cursor
    /// is then its cell in reverse video.
    cursor: Option<u16>,
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
    /// The last character printed, which REP repeats.
    last: char,
    /// Whether G0 and G1 are DEC's special graphics, and whether G1 is
    /// shifted in (SO).
    g0_graphics: bool,
    g1_graphics: bool,
    shifted: bool,
    /// On the alternate screen: the main screen's cells and cursor, which
    /// leaving it restores.
    main: Option<(Vec<Cell>, (usize, usize, Pen))>,
    pub mouse: Mouse,
    /// Answers to the program's queries (cursor position, device
    /// attributes), for the caller to write back to it.
    pub replies: Vec<u8>,
}

impl Grid {
    /// A blank terminal of as many characters of `font` as fit in `width`
    /// by `height` pixels, in `scheme`'s colours.
    pub fn new(width: usize, height: usize, font: Font, scheme: &Scheme) -> Grid {
        let (cols, rows) = ((width / font.width).max(1), (height / font.height).max(1));
        let (fg, bg) = (to565(scheme.foreground), to565(scheme.background));
        let blank = Cell { ch: ' ', fg, bg, underline: false };
        Grid {
            cols,
            rows,
            font,
            fg,
            bg,
            palette: palette(scheme),
            cursor: (scheme.cursor != scheme.foreground).then(|| to565(scheme.cursor)),
            cells: vec![blank; cols * rows],
            dirty: vec![true; cols * rows],
            x: 0,
            y: 0,
            wrap_pending: false,
            pen: PEN,
            saved: (0, 0, PEN),
            top: 0,
            bottom: rows - 1,
            autowrap: true,
            cursor_shown: true,
            drawn_cursor: None,
            last: ' ',
            g0_graphics: false,
            // As the Linux console's G1 starts.
            g1_graphics: true,
            shifted: false,
            main: None,
            mouse: Mouse::default(),
            replies: Vec::new(),
        }
    }

    /// Its size in characters: columns, rows.
    #[cfg(test)]
    fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    /// The scheme's background, which the screen beyond the characters is.
    pub fn background(&self) -> u16 {
        self.bg
    }

    /// Draws what changed since the last call on `target`, from its top
    /// left: the characters in the font; box and block characters it
    /// hasn't as [`boxes`] draws them, scaled to the cell; anything else it
    /// hasn't as its '?'.
    pub fn draw<D: DrawTarget<Color = Rgb565>>(&mut self, target: &mut D) -> Result<(), D::Error> {
        let cols = self.cols;
        let cursor = self.cursor_shown.then_some(self.y * cols + self.x);
        if self.drawn_cursor != cursor {
            for i in [self.drawn_cursor, cursor].into_iter().flatten() {
                self.dirty[i] = true;
            }
            self.drawn_cursor = cursor;
        }
        let (w, h) = (self.font.width, self.font.height);
        let row_bytes = self.font.row_bytes();
        for i in 0..self.cells.len() {
            if !std::mem::take(&mut self.dirty[i]) {
                continue;
            }
            let mut c = self.cells[i];
            if Some(i) == cursor {
                match self.cursor {
                    // On a background of its own colour, it's in reverse.
                    Some(colour) if colour != c.bg => (c.fg, c.bg) = (c.bg, colour),
                    _ => std::mem::swap(&mut c.fg, &mut c.bg),
                }
            }
            let drawn = boxes::glyph(c.ch);
            let glyph = match drawn {
                Some(_) => self.font.glyph(c.ch),
                None => self.font.glyph(c.ch).or_else(|| self.font.glyph('?')),
            };
            let bit = |x: usize, y: usize| match (glyph, drawn) {
                (Some(g), _) => g[y * row_bytes + x / 8] & (0x80 >> (x % 8)) != 0,
                (None, Some(b)) => b[y * boxes::CELL_H / h] & (0x80 >> (x * boxes::CELL_W / w)) != 0,
                (None, None) => false,
            };
            // The underline is the cell's last row.
            let lit = |p: usize| (c.underline && p / w == h - 1) || bit(p % w, p / w);
            let at = Point::new((i % cols * w) as i32, (i / cols * h) as i32);
            let cell = Rectangle::new(at, Size::new(w as u32, h as u32));
            let (fg, bg) = (Rgb565::from(RawU16::new(c.fg)), Rgb565::from(RawU16::new(c.bg)));
            target.fill_contiguous(&cell, (0..w * h).map(|p| if lit(p) { fg } else { bg }))?;
        }
        Ok(())
    }

    fn colour(&self, c: Colour, default: u16, bright: bool) -> u16 {
        match c {
            Colour::Default => default,
            Colour::Index(n) if bright && n < 8 => self.palette[n as usize + 8],
            Colour::Index(n) => self.palette[n as usize],
            Colour::Rgb(r, g, b) => rgb(r, g, b),
        }
    }

    /// A blank in the current background, as erasing leaves (xterm's
    /// background colour erase).
    fn blank(&self) -> Cell {
        Cell { underline: false, ..self.cell_of(' ') }
    }

    fn cell_of(&self, ch: char) -> Cell {
        let p = self.pen;
        let mut fg = self.colour(p.fg, self.fg, p.bold);
        let mut bg = self.colour(p.bg, self.bg, false);
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
        self.x = x.min(self.cols - 1);
        self.y = y.min(self.rows - 1);
        self.wrap_pending = false;
    }

    /// Moves rows `top+n..=bottom` up to `top`, blanking the `n` below.
    fn scroll_up(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        let cols = self.cols;
        for row in top..=bottom {
            if row + n <= bottom {
                for col in 0..cols {
                    let c = self.cells[(row + n) * cols + col];
                    self.set(row * cols + col, c);
                }
            } else {
                self.erase(row * cols, (row + 1) * cols);
            }
        }
    }

    /// Moves rows `top..=bottom-n` down by `n`, blanking the `n` freed.
    fn scroll_down(&mut self, top: usize, bottom: usize, n: usize) {
        let n = n.min(bottom + 1 - top);
        let cols = self.cols;
        for row in (top..=bottom).rev() {
            if row >= top + n {
                for col in 0..cols {
                    let c = self.cells[(row - n) * cols + col];
                    self.set(row * cols + col, c);
                }
            } else {
                self.erase(row * cols, (row + 1) * cols);
            }
        }
    }

    fn linefeed(&mut self) {
        if self.y == self.bottom {
            self.scroll_up(self.top, self.bottom, 1);
        } else if self.y + 1 < self.rows {
            self.y += 1;
        }
    }

    /// To the alternate screen (`on`), blank, or back to the main one as it
    /// was; with the cursor saved and restored too, for 1049.
    fn alternate(&mut self, on: bool, cursor: bool) {
        match (on, self.main.take()) {
            (true, None) => {
                let saved = if cursor { (self.x, self.y, self.pen) } else { self.saved };
                self.main = Some((self.cells.clone(), saved));
                self.erase(0, self.cols * self.rows);
            }
            (false, Some((cells, saved))) => {
                for (i, c) in cells.into_iter().enumerate() {
                    self.set(i, c);
                }
                if cursor {
                    let (x, y, pen) = saved;
                    self.pen = pen;
                    self.goto(x, y);
                }
            }
            (_, main) => self.main = main,
        }
    }

    /// What the program is sent for a touch on cell (`col`, `row`), if it
    /// asked for touches like it.
    #[cfg(test)]
    fn report(&self, button: Button, col: usize, row: usize) -> Option<Vec<u8>> {
        self.mouse.report(button, col.min(self.cols - 1), row.min(self.rows - 1))
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
                // The Linux console's 21 is normal intensity, as 22 is.
                21 | 22 => self.pen.bold = false,
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
        let graphics = if self.shifted { self.g1_graphics } else { self.g0_graphics };
        let ch = match ch {
            '`'..='~' if graphics => GRAPHICS[ch as usize - '`' as usize],
            _ => ch,
        };
        self.last = ch;
        let c = self.cell_of(ch);
        self.set(self.y * self.cols + self.x, c);
        if self.x + 1 < self.cols {
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
            // SO and SI: G1, then G0 again.
            0x0e => self.shifted = true,
            0x0f => self.shifted = false,
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, action: char) {
        let args: Vec<usize> = params.iter().map(|p| p[0] as usize).collect();
        // The nth argument, where 0 or absent means `default`.
        let arg = |n: usize, default: usize| args.get(n).copied().filter(|&v| v != 0).unwrap_or(default);
        let private = intermediates == b"?";
        let (x, y) = (self.x, self.y);
        let (cols, rows) = (self.cols, self.rows);
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
                let at = y * cols + x;
                match args.first().copied().unwrap_or(0) {
                    0 => self.erase(at, cols * rows),
                    1 => self.erase(0, at + 1),
                    _ => self.erase(0, cols * rows),
                }
            }
            (false, 'K') => {
                let row = y * cols;
                match args.first().copied().unwrap_or(0) {
                    0 => self.erase(row + x, row + cols),
                    1 => self.erase(row, row + x + 1),
                    _ => self.erase(row, row + cols),
                }
            }
            (false, 'X') => self.erase(y * cols + x, y * cols + (x + arg(0, 1)).min(cols)),
            (false, 'P' | '@') => {
                let n = arg(0, 1).min(cols - x);
                let row = y * cols;
                let mut line: Vec<Cell> = self.cells[row + x..row + cols].to_vec();
                let b = self.blank();
                if action == 'P' {
                    line.drain(..n);
                    line.extend(std::iter::repeat_n(b, n));
                } else {
                    line.splice(0..0, std::iter::repeat_n(b, n));
                    line.truncate(cols - x);
                }
                for (i, c) in line.into_iter().enumerate() {
                    self.set(row + x + i, c);
                }
            }
            (false, 'L') if (self.top..=self.bottom).contains(&y) => self.scroll_down(y, self.bottom, arg(0, 1)),
            (false, 'M') if (self.top..=self.bottom).contains(&y) => self.scroll_up(y, self.bottom, arg(0, 1)),
            (false, 'S') => self.scroll_up(self.top, self.bottom, arg(0, 1)),
            (false, 'T') => self.scroll_down(self.top, self.bottom, arg(0, 1)),
            (false, 'I') => self.goto((x / 8 + arg(0, 1)) * 8, y),
            (false, 'Z') => self.goto(x.div_ceil(8).saturating_sub(arg(0, 1)) * 8, y),
            (false, 'b') => {
                for _ in 0..arg(0, 1).min(cols * rows) {
                    self.print(self.last);
                }
            }
            (false, 'm') => self.sgr(params),
            (false, 'r') => {
                let (top, bottom) = (arg(0, 1) - 1, arg(1, rows).min(rows) - 1);
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
                        47 | 1047 => self.alternate(on, false),
                        1049 => self.alternate(on, true),
                        m => self.mouse.set(m as u16, on),
                    }
                }
            }
            (false, 'n') => match arg(0, 0) {
                5 => self.replies.extend_from_slice(b"\x1b[0n"),
                6 => self.replies.extend_from_slice(format!("\x1b[{};{}R", y + 1, x + 1).as_bytes()),
                _ => {}
            },
            // A VT102, as the Linux console says it is.
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
        match intermediates {
            b"(" => self.g0_graphics = byte == b'0',
            b")" => self.g1_graphics = byte == b'0',
            b"" => match byte {
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
                    let fresh = Grid::new(0, 0, self.font.clone(), &Scheme::default());
                    let (cols, rows, fg, bg, palette, cursor) =
                        (self.cols, self.rows, self.fg, self.bg, self.palette, self.cursor);
                    let blank = Cell { ch: ' ', fg, bg, underline: false };
                    *self = Grid {
                        cols,
                        rows,
                        fg,
                        bg,
                        palette,
                        cursor,
                        cells: vec![blank; cols * rows],
                        dirty: vec![true; cols * rows],
                        bottom: rows - 1,
                        replies,
                        ..fresh
                    };
                }
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen to draw on: RGB565 pixels.
    struct Screen {
        w: usize,
        px: Vec<u16>,
    }

    impl DrawTarget for Screen {
        type Color = Rgb565;
        type Error = std::convert::Infallible;

        fn draw_iter<I: IntoIterator<Item = Pixel<Rgb565>>>(&mut self, pixels: I) -> Result<(), Self::Error> {
            for Pixel(p, c) in pixels {
                let i = p.y as usize * self.w + p.x as usize;
                if let Some(px) = self.px.get_mut(i) {
                    *px = RawU16::from(c).into_inner();
                }
            }
            Ok(())
        }
    }

    impl OriginDimensions for Screen {
        fn size(&self) -> Size {
            Size::new(self.w as u32, (self.px.len() / self.w) as u32)
        }
    }

    fn grid(font: &str) -> Grid {
        Grid::new(320, 240, Font::builtin(font).unwrap(), &Scheme::default())
    }

    fn feed(g: &mut Grid, bytes: &[u8]) {
        let mut p = vte::Parser::new();
        p.advance(g, bytes);
    }

    #[test]
    fn as_many_characters_as_fit() {
        for (font, cols, rows) in [("spleen-5x8", 64, 30), ("spleen-8x16", 40, 15), ("spleen-32x64", 10, 3)] {
            assert_eq!(grid(font).size(), (cols, rows), "{font}");
        }
    }

    #[test]
    fn drawn_in_the_font_and_the_schemes_colours() {
        let s = Scheme::default();
        let mut g = grid("spleen-8x16");
        feed(&mut g, b"\x1b[?25l\x1b[31mA\x1b[0m\x1b[44m \x1b[1;30mB");
        let mut screen = Screen { w: 320, px: vec![0; 320 * 240] };
        g.draw(&mut screen).unwrap();
        let cell = |col: usize| -> Vec<u16> {
            (0..16).flat_map(|y| (0..8).map(move |x| (x, y))).map(|(x, y)| screen.px[y * 320 + col * 8 + x]).collect()
        };
        let a = cell(0);
        assert!(a.contains(&to565(s.palette[1])) && a.contains(&to565(s.background)));
        assert!(cell(1).iter().all(|&p| p == to565(s.palette[4])));
        // Bold black is bright black.
        assert!(cell(2).contains(&to565(s.palette[8])));
        // The rest is the scheme's background.
        assert!(cell(3).iter().all(|&p| p == to565(s.background)));
    }

    #[test]
    fn the_cursor_in_its_colour() {
        let mut screen = Screen { w: 320, px: vec![0; 320 * 240] };
        let first = |s: &Screen| s.px[0];
        // GitHub Dark's is its foreground: the cell in reverse, as before.
        let mut g = grid("spleen-8x16");
        g.draw(&mut screen).unwrap();
        assert_eq!(first(&screen), to565(Scheme::default().foreground));
        // One of its own.
        let s = Scheme { cursor: [0x12, 0x34, 0x56], ..Scheme::default() };
        let mut g = Grid::new(320, 240, Font::builtin("spleen-8x16").unwrap(), &s);
        g.draw(&mut screen).unwrap();
        assert_eq!(first(&screen), to565(s.cursor));
        // Reset (RIS) keeps it.
        feed(&mut g, b"\x1bc");
        g.draw(&mut screen).unwrap();
        assert_eq!(first(&screen), to565(s.cursor));
    }

    #[test]
    fn the_linux_consoles_line_drawing() {
        let mut g = grid("spleen-5x8");
        // G1 is DEC's graphics, shifted in by SO, as curses' "linux" does.
        feed(&mut g, b"\x0eqx\x0fq");
        let chars: String = g.cells[..3].iter().map(|c| c.ch).collect();
        assert_eq!(chars, "─│q");
    }

    #[test]
    fn touches_as_the_program_asked() {
        let mut g = grid("spleen-5x8");
        assert_eq!(g.report(Button::Press, 3, 4), None);
        feed(&mut g, b"\x1b[?1000h");
        assert_eq!(g.report(Button::Press, 3, 4).unwrap(), b"\x1b[M $%");
        assert_eq!(g.report(Button::Release, 3, 4).unwrap(), b"\x1b[M#$%");
        assert_eq!(g.report(Button::Drag, 3, 4), None);
        feed(&mut g, b"\x1b[?1002h\x1b[?1006h");
        assert_eq!(g.report(Button::Press, 3, 4).unwrap(), b"\x1b[<0;4;5M");
        assert_eq!(g.report(Button::Drag, 99, 99).unwrap(), b"\x1b[<32;64;30M");
        assert_eq!(g.report(Button::Release, 3, 4).unwrap(), b"\x1b[<0;4;5m");
        // A reset forgets it, and keeps the size.
        feed(&mut g, b"\x1bc");
        assert_eq!((g.mouse, g.size()), (Mouse::default(), (64, 30)));
    }
}
