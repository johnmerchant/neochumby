//! A screen of character cells, drawn whole and written to the terminal at
//! once, with the places a tap means something ([`Frame::hits`]).
//!
//! Only printable ASCII is drawn: Spleen has it in every size, and the
//! Linux console maps it the same with any font. Anything else is put as
//! its nearest ASCII ([`ascii`]). Colours are the eight of `TERM=linux`,
//! bold for the bright ones; the owner's scheme decides what they look
//! like.

use std::fmt::Write as _;

/// One of the terminal's eight colours, or its default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Default,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

impl Color {
    fn code(self) -> Option<u8> {
        Some(match self {
            Color::Default => return None,
            Color::Black => 0,
            Color::Red => 1,
            Color::Green => 2,
            Color::Yellow => 3,
            Color::Blue => 4,
            Color::Magenta => 5,
            Color::Cyan => 6,
            Color::White => 7,
        })
    }
}

/// How a cell is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
}

impl Style {
    pub const PLAIN: Style = Style { fg: Color::Default, bg: Color::Default, bold: false };

    pub const fn fg(fg: Color) -> Style {
        Style { fg, bg: Color::Default, bold: false }
    }

    pub const fn bold(self) -> Style {
        Style { bold: true, ..self }
    }

    pub const fn on(self, bg: Color) -> Style {
        Style { bg, ..self }
    }

    /// The SGR sequence that sets it from nothing.
    fn sgr(self) -> String {
        let mut s = String::from("\x1b[0");
        if self.bold {
            s.push_str(";1");
        }
        if let Some(c) = self.fg.code() {
            let _ = write!(s, ";3{c}");
        }
        if let Some(c) = self.bg.code() {
            let _ = write!(s, ";4{c}");
        }
        s.push('m');
        s
    }
}

/// A rectangle of cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// What is on the screen, and what tapping each part of it does.
pub struct Frame<A> {
    pub cols: usize,
    pub rows: usize,
    cells: Vec<(char, Style)>,
    /// The tap targets, last drawn on top.
    pub hits: Vec<(Rect, A)>,
}

impl<A: Clone> Frame<A> {
    pub fn new(cols: usize, rows: usize) -> Frame<A> {
        Frame { cols, rows, cells: vec![(' ', Style::PLAIN); cols * rows], hits: Vec::new() }
    }

    /// Puts `text` at (x, y), cut at the screen's edge: the columns used.
    pub fn put(&mut self, x: usize, y: usize, text: &str, style: Style) -> usize {
        if y >= self.rows {
            return 0;
        }
        let mut n = 0;
        for (i, c) in ascii(text).chars().enumerate() {
            if x + i >= self.cols {
                break;
            }
            self.cells[y * self.cols + x + i] = (c, style);
            n += 1;
        }
        n
    }

    /// Puts `text` so that it ends at the screen's right edge, less `pad`.
    pub fn put_right(&mut self, y: usize, pad: usize, text: &str, style: Style) {
        let w = ascii(text).chars().count();
        let x = self.cols.saturating_sub(w + pad);
        self.put(x, y, text, style);
    }

    /// Fills `r` with blanks in `style`.
    pub fn fill(&mut self, r: Rect, style: Style) {
        for y in r.y..(r.y + r.h).min(self.rows) {
            for x in r.x..(r.x + r.w).min(self.cols) {
                self.cells[y * self.cols + x] = (' ', style);
            }
        }
    }

    /// A button: `r` filled, its label in the middle, tapping it `action`.
    pub fn button(&mut self, r: Rect, label: &str, style: Style, action: A) {
        self.fill(r, style);
        let w = label.chars().count().min(r.w);
        self.put(r.x + (r.w - w) / 2, r.y + r.h / 2, &label.chars().take(w).collect::<String>(), style);
        self.hits.push((r, action));
    }

    /// Makes `r` a tap target, as it is drawn.
    pub fn target(&mut self, r: Rect, action: A) {
        self.hits.push((r, action));
    }

    /// What tapping the cell (x, y) does, if anything.
    pub fn hit(&self, x: usize, y: usize) -> Option<A> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone())
    }

    /// Row `y` as text, its right end trimmed: for tests.
    pub fn row(&self, y: usize) -> String {
        self.cells[y * self.cols..(y + 1) * self.cols].iter().map(|(c, _)| *c).collect::<String>().trim_end().into()
    }

    /// The style of the cell at (x, y).
    pub fn style(&self, x: usize, y: usize) -> Style {
        self.cells[y * self.cols + x].1
    }

    /// The whole screen as text, a line per row: for tests.
    pub fn text(&self) -> String {
        (0..self.rows).map(|y| self.row(y) + "\n").collect()
    }

    /// What the terminal is sent to show it: from the top left, every
    /// cell, with a style change only where the style changes. The last
    /// cell is left alone, so that the terminal never scrolls.
    pub fn ansi(&self) -> String {
        let mut out = String::from("\x1b[H");
        let mut last = None;
        for y in 0..self.rows {
            let _ = write!(out, "\x1b[{};1H", y + 1);
            for x in 0..self.cols {
                if y == self.rows - 1 && x == self.cols - 1 {
                    break;
                }
                let (c, s) = self.cells[y * self.cols + x];
                if last != Some(s) {
                    out.push_str(&s.sgr());
                    last = Some(s);
                }
                out.push(c);
            }
        }
        out.push_str("\x1b[0m");
        out
    }
}

/// `s` in printable ASCII: the usual typographic marks as their ASCII
/// forms, other characters as `?`, and controls left out.
pub fn ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            ' '..='~' => out.push(c),
            '\t' | '\n' | '\r' => out.push(' '),
            '\u{2018}' | '\u{2019}' | '\u{2032}' => out.push('\''),
            '\u{201c}' | '\u{201d}' | '\u{2033}' => out.push('"'),
            '\u{2010}'..='\u{2015}' | '\u{2212}' => out.push('-'),
            '\u{2026}' => out.push_str("..."),
            '\u{a0}' => out.push(' '),
            c if c.is_control() => {}
            _ => out.push('?'),
        }
    }
    out
}

/// `text` in lines of at most `width` characters, broken between words
/// where it can be; at most `max` lines, the last ending in "..." if
/// there was more.
pub fn wrap(text: &str, width: usize, max: usize) -> Vec<String> {
    let text = ascii(text);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        loop {
            let need = if line.is_empty() { word.len() } else { line.len() + 1 + word.len() };
            if need <= width {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(&word);
                break;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                continue;
            }
            // A word longer than a line: broken where the line ends.
            let rest = word.split_off(width.max(1));
            lines.push(word);
            word = rest;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > max {
        lines.truncate(max);
        if let Some(last) = lines.last_mut() {
            let keep = width.saturating_sub(3).min(last.len());
            last.truncate(keep);
            last.push_str("...");
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ascii_is_drawn() {
        assert_eq!(ascii("It\u{2019}s \u{201c}fine\u{201d} \u{2014} really\u{2026}"), "It's \"fine\" - really...");
        assert_eq!(ascii("caf\u{e9}\u{1b}[31m \u{1f600}"), "caf?[31m ?");
    }

    #[test]
    fn words_wrap() {
        assert_eq!(wrap("a b c", 3, 5), ["a b", "c"]);
        assert_eq!(wrap("forecasts for your city, by touch", 12, 5), ["forecasts", "for your", "city, by", "touch"]);
        assert_eq!(wrap("abcdefgh", 3, 5), ["abc", "def", "gh"]);
        assert_eq!(wrap("one two three four", 9, 1), ["one tw..."]);
        assert!(wrap("", 10, 2).is_empty());
    }

    #[test]
    fn put_is_cut_at_the_edge_and_taps_find_the_top_target() {
        let mut f: Frame<u8> = Frame::new(10, 3);
        assert_eq!(f.put(7, 0, "hello", Style::PLAIN), 3);
        assert_eq!(f.row(0), "       hel");
        f.button(Rect { x: 0, y: 1, w: 10, h: 2 }, "OK", Style::fg(Color::Black).on(Color::Cyan), 1);
        f.target(Rect { x: 0, y: 2, w: 2, h: 1 }, 2);
        assert_eq!(f.row(2), "    OK");
        assert_eq!((f.hit(5, 1), f.hit(1, 2), f.hit(5, 0)), (Some(1), Some(2), None));
    }

    #[test]
    fn ansi_changes_style_only_where_it_changes() {
        let mut f: Frame<()> = Frame::new(4, 2);
        f.put(0, 0, "ab", Style::fg(Color::Red).bold());
        let a = f.ansi();
        assert_eq!(a, "\x1b[H\x1b[1;1H\x1b[0;1;31mab\x1b[0m  \x1b[2;1H   \x1b[0m");
    }
}
