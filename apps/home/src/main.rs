//! neochumby-home, the app list: the installed apps, tap one to open it;
//! PAIR when booted with a neochumbyd this Chumby is not paired with; the
//! time; and how it's connected (network.rs), which only looks. It's an
//! ordinary app that may be the start app (`start = true`), and the start app when
//! no other is chosen. The session also opens it from anything when the
//! top-left corner is held.
//!
//! It runs in text mode, like any app without `fb`: it draws on its
//! terminal (term.rs), and touches come as mouse reports on its stdin. In
//! its own font, Spleen 8x16, the terminal is 40 by 15, and the rows fall
//! where they did when home drew the framebuffer itself: apps from y=16,
//! 48 high, and the bar along the bottom 32. Another font (the owner's
//! `font=`) scales the same layout to its cells.
//!
//! It opens apps by asking the session over its request
//! pipe (neochumby::session), then exits: the session runs what it asked
//! for, and opens the start app again when that exits. Opened for any
//! reason (NEOCHUMBY_REASON), it shows its first page. The top button,
//! pressed with it in front, arrives as a `button` line on its pipe: it
//! goes back to the first page, leaving pairing if that's showing.
//!
//! An app that needs a neochumbyd this Chumby has not got is listed, greyed,
//! and does not open.

mod network;
mod pair;
mod term;

use std::time::{Duration, Instant};

use neochumby::apps::{self, App};
use neochumby::conf;
use neochumby::session::{Notice, Pipe};
use term::{Paint, Rgb, Term, Touch, fit, rgb565};

const GREY: Rgb = (90, 90, 100);
const TEXT: Rgb = (230, 230, 230);
const TITLE: Rgb = (255, 200, 120);
const ROW: Rgb = (28, 28, 48);
const PRESSED: Rgb = (230, 230, 230);
const DARK: Rgb = (0, 0, 0);
const GREEN: Rgb = (120, 220, 120);

/// How often the network's line is read again.
const NETWORK_EVERY: Duration = Duration::from_secs(5);

/// Apps to a page: four, or three and MORE.
const ROWS: usize = 4;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    App(usize),
    More,
    Pair,
}

/// Where things are on a terminal of `cols` by `rows` cells: the title on
/// the top row, then [`ROWS`] rows of apps, each `entry` high, and the bar
/// (PAIR), when it has a button, `bar` high along the bottom.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Layout {
    cols: usize,
    rows: usize,
    entry: usize,
    bar: usize,
}

impl Layout {
    fn new(cols: usize, rows: usize, bar: bool) -> Layout {
        let bar = match bar {
            false => 0,
            true if rows >= 12 => 2,
            true => 1,
        };
        let entry = (rows.saturating_sub(1 + bar) / ROWS).max(1);
        Layout { cols, rows, entry, bar }
    }

    /// How many of an entry's rows it fills: all but the first, which
    /// parts it from what is above, when it has three or more.
    fn filled(&self) -> usize {
        if self.entry >= 3 { self.entry - 1 } else { self.entry }
    }

    /// The first row entry `i` fills.
    fn entry_row(&self, i: usize) -> usize {
        1 + i * self.entry + (self.entry - self.filled())
    }

    /// The bar's first row.
    fn bar_row(&self) -> usize {
        self.rows - self.bar
    }
}

/// What is on screen: the listed apps (by index), a page at a time.
struct Screen {
    listed: Vec<usize>,
    page: usize,
    pairing: Option<(String, u16)>,
    /// How the Chumby is connected, a line to show.
    network: String,
}

impl Screen {
    /// The top button: back to the first page.
    fn button(&mut self) {
        self.page = 0;
    }

    fn pages(&self) -> usize {
        if self.listed.len() <= ROWS { 1 } else { self.listed.len().div_ceil(ROWS - 1) }
    }

    /// The rows on this page: apps, and MORE last when there are more pages.
    fn rows(&self) -> Vec<Hit> {
        if self.pages() == 1 {
            return self.listed.iter().map(|&i| Hit::App(i)).collect();
        }
        let per = ROWS - 1;
        let mut r: Vec<Hit> = self.listed.iter().skip(self.page * per).take(per).map(|&i| Hit::App(i)).collect();
        r.push(Hit::More);
        r
    }

    /// The bar's buttons, left to right.
    fn bar(&self) -> Vec<Hit> {
        let mut b = Vec::new();
        if self.pairing.is_some() {
            b.push(Hit::Pair);
        }
        b
    }

    fn layout(&self, cols: usize, rows: usize) -> Layout {
        Layout::new(cols, rows, !self.bar().is_empty())
    }

    /// What is on cell (`col`, `row`).
    fn hit(&self, l: &Layout, col: usize, row: usize) -> Option<Hit> {
        if l.bar > 0 && row >= l.bar_row() {
            let bar = self.bar();
            return bar.get(col * bar.len() / l.cols.max(1)).copied();
        }
        let row = row.checked_sub(1)?;
        // Not on the row that parts an entry from the one above.
        if row % l.entry < l.entry - l.filled() {
            return None;
        }
        self.rows().get(row / l.entry).copied()
    }
}

fn clock() -> String {
    let t = neochumby_sys::time::local_time();
    format!("{:02}:{:02}", t.hour, t.minute)
}

/// How the Chumby is connected, under the title: on the row that parts the
/// first app from it, where a tap does nothing. A layout too small for that
/// row goes without. Padded, to overwrite a longer line.
fn paint_network(p: &mut Paint, l: &Layout, line: &str) {
    if l.entry - l.filled() == 0 {
        return;
    }
    let w = l.cols.saturating_sub(2);
    p.at(1, 1).text(&format!("{:<w$}", fit(line, w)), GREY, None, false);
}

fn paint_clock(p: &mut Paint, l: &Layout) {
    let t = clock();
    p.at(l.cols.saturating_sub(t.len() + 1), 0).text(&t, TEXT, None, true);
}

/// A button of the bar, `w` wide from `col`, its label in the middle.
fn button(p: &mut Paint, l: &Layout, col: usize, w: usize, label: &str, colour: Rgb, pressed: bool) {
    let bg = if pressed { (255, 255, 255) } else { colour };
    p.fill(col, l.bar_row(), w, l.bar, bg);
    let label = fit(label, w);
    p.at(col + (w - label.len()) / 2, l.bar_row() + (l.bar - 1) / 2).text(&label, DARK, Some(bg), true);
}

fn draw(t: &mut Term, all: &[App], s: &Screen, pressed: Option<Hit>) {
    let l = s.layout(t.cols, t.rows);
    let mut p = Paint::cleared();
    p.at(1, 0).text("neochumby", TITLE, None, true);
    paint_clock(&mut p, &l);
    paint_network(&mut p, &l, &s.network);

    if s.listed.is_empty() {
        p.at(1, 2).text("no apps installed", GREY, None, false);
    }
    let filled = l.filled();
    let stripe = (l.cols / 40).max(1);
    for (i, hit) in s.rows().into_iter().enumerate() {
        let row = l.entry_row(i);
        let lit = pressed == Some(hit);
        let bg = if lit { PRESSED } else { ROW };
        p.fill(0, row, l.cols, filled, bg);
        let label_row = row + (filled - 1) / 2;
        let text_col = stripe + 1;
        let width = l.cols.saturating_sub(text_col + 1);
        match hit {
            Hit::App(i) => {
                let a = &all[i];
                let ok = a.runnable();
                p.fill(0, row, stripe, filled, if ok { rgb565(a.color) } else { GREY });
                let fg = if lit {
                    DARK
                } else if ok {
                    TEXT
                } else {
                    GREY
                };
                p.at(text_col, label_row).text(&fit(a.title.as_deref().unwrap_or(&a.name), width), fg, Some(bg), true);
                let note = "needs pairing";
                if !ok && width > 16 + note.len() {
                    p.at(l.cols - note.len() - 1, label_row).text(note, GREY, Some(bg), false);
                }
            }
            Hit::More => {
                let label = format!("MORE  {}/{}", s.page + 1, s.pages());
                p.at(text_col, label_row).text(&fit(&label, width), if lit { DARK } else { TEXT }, Some(bg), true);
            }
            _ => {}
        }
    }

    let bar = s.bar();
    for (i, hit) in bar.iter().enumerate() {
        let (from, to) = (i * l.cols / bar.len(), (i + 1) * l.cols / bar.len());
        button(&mut p, &l, from, to - from, "PAIR", GREEN, pressed == Some(*hit));
    }
    t.show(&p.0);
}

/// Asks the session for `what` and exits, leaving the terminal cleared,
/// or returns if there's no session to ask (run by hand).
fn ask(t: &mut Term, pipe: Option<&mut Pipe>, what: &str, send: impl FnOnce(&mut Pipe) -> std::io::Result<()>) {
    let Some(pipe) = pipe else {
        eprintln!("neochumby-home: no session to ask for {what} (not run by neochumby)");
        return;
    };
    if let Err(e) = send(pipe) {
        eprintln!("neochumby-home: asking for {what}: {e}");
        return;
    }
    t.leave();
    std::process::exit(0);
}

/// Whether the top button was pressed: a `button` line on the pipe since
/// the last look. Never waits.
fn button_pressed(pipe: Option<&mut Pipe>) -> bool {
    pipe.is_some_and(|p| has_button(&p.receive().lines))
}

fn has_button(lines: &[String]) -> bool {
    lines.iter().any(|l| Notice::parse(l) == Some(Notice::Button))
}

/// A neochumbyd the boot hook found but this Chumby is not paired with.
fn pairing_offer() -> Option<(String, u16)> {
    let server = std::env::var("NEOCHUMBY_SERVER").ok().filter(|s| conf::valid_value(s))?;
    let port =
        std::env::var("NEOCHUMBY_API_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(neochumby_proto::API_PORT);
    match conf::paired() {
        Some(p) if p.server == server => None,
        _ => Some((server, port)),
    }
}

fn main() {
    neochumby::debug::install("neochumby-home");
    let mut t = Term::open();
    let mut pipe = Pipe::from_env();
    let root = apps::root();
    let all = apps::all(&root);
    let mut screen = Screen {
        listed: all.iter().enumerate().filter(|(_, a)| a.title.is_some() && a.exec.is_some()).map(|(i, _)| i).collect(),
        page: 0,
        pairing: pairing_offer(),
        network: network::Status::read().line(),
    };
    let mut pressed: Option<Hit> = None;
    let mut last_clock = Instant::now();
    let mut last_network = Instant::now();
    draw(&mut t, &all, &screen, None);
    // For the scenarios, which wait for it.
    eprintln!("neochumby-home: showing {} apps on {}x{}", screen.listed.len(), t.cols, t.rows);

    loop {
        for touch in t.poll(30) {
            let l = screen.layout(t.cols, t.rows);
            match touch {
                Touch::Down { col, row } => {
                    let hit = screen.hit(&l, col, row);
                    if hit != pressed {
                        pressed = hit;
                        draw(&mut t, &all, &screen, pressed);
                    }
                }
                Touch::Up => {
                    match pressed.take() {
                        Some(Hit::App(i)) => {
                            if all[i].runnable() {
                                ask(&mut t, pipe.as_mut(), &all[i].name, |p| p.open(&all[i].name));
                            }
                        }
                        Some(Hit::More) => screen.page = (screen.page + 1) % screen.pages(),
                        Some(Hit::Pair) => {
                            if let Some((server, port)) = screen.pairing.clone()
                                && pair::run(&mut t, &server, port, &root, || button_pressed(pipe.as_mut()))
                            {
                                screen.pairing = None;
                            }
                            // Back from pairing, by the button or not: the first page.
                            screen.page = 0;
                        }
                        None => {}
                    }
                    t.drain();
                    draw(&mut t, &all, &screen, None);
                }
            }
        }
        if button_pressed(pipe.as_mut()) {
            screen.button();
            pressed = None;
            draw(&mut t, &all, &screen, None);
        }
        if last_clock.elapsed() >= Duration::from_secs(1) {
            let mut p = Paint::default();
            paint_clock(&mut p, &screen.layout(t.cols, t.rows));
            t.show(&p.0);
            last_clock = Instant::now();
        }
        if last_network.elapsed() >= NETWORK_EVERY {
            let line = network::Status::read().line();
            if line != screen.network {
                let mut p = Paint::default();
                paint_network(&mut p, &screen.layout(t.cols, t.rows), &line);
                t.show(&p.0);
                screen.network = line;
            }
            last_network = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(apps: usize, pairing: bool) -> Screen {
        Screen {
            listed: (0..apps).collect(),
            page: 0,
            pairing: pairing.then(|| ("s".into(), 1)),
            network: String::new(),
        }
    }

    #[test]
    fn the_button_goes_back_to_the_first_page() {
        let mut s = screen(10, false);
        s.page = 2;
        assert!(s.pages() > 2);
        s.button();
        assert_eq!(s.page, 0);
        assert!(has_button(&["front wx".into(), "button".into()]));
        assert!(!has_button(&["front wx".into(), "buttons".into()]));
    }

    /// In Spleen 8x16, a tap lands where it did on the framebuffer home
    /// drew before: dev/emulate-smoke taps apps at y = 62, 102, 142 and 182.
    #[test]
    fn rows_fall_where_they_did() {
        let s = screen(5, true);
        let l = s.layout(40, 15);
        assert_eq!(l, Layout { cols: 40, rows: 15, entry: 3, bar: 2 });
        let at = |x: usize, y: usize| s.hit(&l, x / 8, y / 16);
        assert_eq!(at(160, 62), Some(Hit::App(0)));
        assert_eq!(at(160, 102), Some(Hit::App(1)));
        assert_eq!(at(160, 142), Some(Hit::App(2)));
        assert_eq!(at(160, 182), Some(Hit::More));
        assert_eq!(at(160, 220), Some(Hit::Pair));
        assert_eq!(at(160, 228), Some(Hit::Pair));
        // Above the apps, where dev/emulate-smoke taps for `neochumby
        // sensors`, and the row parting the first from the title: nothing.
        assert_eq!(at(160, 8), None);
        assert_eq!(at(200, 20), None);
    }

    #[test]
    fn the_bar_holds_what_there_is() {
        // PAIR alone, else no bar.
        let s = screen(2, true);
        let l = s.layout(40, 15);
        assert_eq!(s.hit(&l, 35, 14), Some(Hit::Pair));
        let s = screen(2, false);
        let l = s.layout(40, 15);
        assert_eq!(l.bar, 0);
        assert_eq!(s.hit(&l, 20, 14), None);
        assert_eq!(s.hit(&l, 20, 2), Some(Hit::App(0)));
    }

    #[test]
    fn another_font_scales_the_layout() {
        // Spleen 5x8: 64 by 30.
        let s = screen(3, true);
        let l = s.layout(64, 30);
        assert_eq!((l.entry, l.bar), (6, 2));
        assert_eq!(s.hit(&l, 10, 8), Some(Hit::App(1)));
        assert_eq!(s.hit(&l, 10, 29), Some(Hit::Pair));
    }
}
