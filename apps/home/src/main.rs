//! neochumby-home, the app list: the installed apps, tap one to open it;
//! PAIR when booted with a neochumbyd this Chumby is not paired with; the
//! time; and CHUMBY MODE, which hands the Chumby back to its own software.
//! It's an ordinary app that may be the start app (`start = true`), and the
//! start app when no other is chosen. The session also opens it from
//! anything when the top-left corner is held.
//!
//! It opens apps and CHUMBY MODE by asking the session over its request
//! pipe (neochumby::session), then exits: the session runs what it asked
//! for, and opens the start app again when that exits. Opened for any
//! reason (NEOCHUMBY_REASON), it shows its first page. The top button,
//! pressed with it in front, arrives as a `button` line on its pipe: it
//! goes back to the first page, leaving pairing if that's showing.
//!
//! An app that needs a neochumbyd this Chumby has not got is listed, greyed,
//! and does not open.

mod pair;

use std::time::{Duration, Instant};

use neochumby::apps::{self, App};
use neochumby::conf;
use neochumby::fb::{self, Fb, rgb};
use neochumby::input::{Touch, Touchscreen};
use neochumby::session::{Notice, Pipe};

const BG: u16 = rgb(16, 16, 32);
const GREY: u16 = rgb(90, 90, 100);
const TEXT: u16 = rgb(230, 230, 230);

const TOP: usize = 44;
const ROW_H: usize = 36;
const GAP: usize = 4;
const ROWS: usize = 4;
const BAR_Y: usize = fb::H - 36;

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    App(usize),
    More,
    Pair,
    ChumbyMode,
}

/// What is on screen: the listed apps (by index), a page at a time.
struct Screen {
    listed: Vec<usize>,
    page: usize,
    pairing: Option<(String, u16)>,
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

    fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let (x, y) = (x.max(0) as usize, y.max(0) as usize);
        if y >= BAR_Y {
            return Some(match &self.pairing {
                Some(_) if x < fb::W / 2 => Hit::Pair,
                _ => Hit::ChumbyMode,
            });
        }
        let row = y.checked_sub(TOP)? / (ROW_H + GAP);
        self.rows().get(row).copied()
    }
}

fn clock() -> String {
    let t = neochumby_sys::time::local_time();
    format!("{:02}:{:02}", t.hour, t.minute)
}

fn draw_clock(fb: &mut Fb) {
    let t = clock();
    let x = fb::W - Fb::text_width(&t, 2) - 10;
    fb.rect(x, 12, Fb::text_width(&t, 2), 16, BG);
    fb.text(x, 12, &t, 2, TEXT);
}

fn button(fb: &mut Fb, x: usize, w: usize, label: &str, colour: u16, pressed: bool) {
    fb.rect(x, BAR_Y, w, 32, if pressed { rgb(255, 255, 255) } else { colour });
    let tx = x + w.saturating_sub(Fb::text_width(label, 2)) / 2;
    fb.text(tx, BAR_Y + 8, label, 2, rgb(0, 0, 0));
}

fn draw(fb: &mut Fb, all: &[App], s: &Screen, pressed: Option<Hit>) {
    fb.clear(BG);
    fb.text(10, 12, "neochumby", 2, rgb(255, 200, 120));
    draw_clock(fb);

    if s.listed.is_empty() {
        fb.text(10, TOP + 10, "no apps installed", 2, GREY);
    }
    for (row, hit) in s.rows().into_iter().enumerate() {
        let y = TOP + row * (ROW_H + GAP);
        let lit = pressed == Some(hit);
        fb.rect(4, y, fb::W - 8, ROW_H, if lit { rgb(60, 60, 80) } else { rgb(28, 28, 48) });
        match hit {
            Hit::App(i) => {
                let a = &all[i];
                let ok = a.runnable();
                fb.rect(4, y, 8, ROW_H, if ok { a.color } else { GREY });
                fb.text(22, y + 10, a.title.as_deref().unwrap_or(&a.name), 2, if ok { TEXT } else { GREY });
                if !ok {
                    fb.text(fb::W - Fb::text_width("needs pairing", 1) - 12, y + 14, "needs pairing", 1, GREY);
                }
            }
            Hit::More => {
                let label = format!("MORE  {}/{}", s.page + 1, s.pages());
                fb.text(22, y + 10, &label, 2, TEXT);
            }
            _ => {}
        }
    }

    let half = (fb::W - 12) / 2;
    if s.pairing.is_some() {
        button(fb, 4, half, "PAIR", rgb(120, 220, 120), pressed == Some(Hit::Pair));
        button(fb, 8 + half, half, "CHUMBY MODE", rgb(120, 200, 255), pressed == Some(Hit::ChumbyMode));
    } else {
        button(fb, 4, fb::W - 8, "CHUMBY MODE", rgb(120, 200, 255), pressed == Some(Hit::ChumbyMode));
    }
}

/// Asks the session for `what` and exits, leaving the screen black, or
/// returns if there's no session to ask (run by hand).
fn ask(fb: &mut Fb, pipe: Option<&mut Pipe>, what: &str, send: impl FnOnce(&mut Pipe) -> std::io::Result<()>) {
    let Some(pipe) = pipe else {
        eprintln!("neochumby-home: no session to ask for {what} (not run by neochumby)");
        return;
    };
    if let Err(e) = send(pipe) {
        eprintln!("neochumby-home: asking for {what}: {e}");
        return;
    }
    fb.clear(rgb(0, 0, 0));
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
    let mut fb = Fb::open().expect("/dev/fb0");
    let mut pipe = Pipe::from_env();
    let mut touch = Touchscreen::open();
    let root = apps::root();
    let all = apps::all(&root);
    let mut screen = Screen {
        listed: all.iter().enumerate().filter(|(_, a)| a.title.is_some() && a.exec.is_some()).map(|(i, _)| i).collect(),
        page: 0,
        pairing: pairing_offer(),
    };
    let mut pressed: Option<Hit> = None;
    let mut last_clock = Instant::now();
    draw(&mut fb, &all, &screen, None);

    loop {
        for t in touch.poll() {
            match t {
                Touch::Down { x, y, .. } => {
                    let hit = screen.hit(x, y);
                    if hit != pressed {
                        pressed = hit;
                        draw(&mut fb, &all, &screen, pressed);
                    }
                }
                Touch::Up => {
                    match pressed.take() {
                        Some(Hit::ChumbyMode) => ask(&mut fb, pipe.as_mut(), "CHUMBY MODE", |p| p.chumby_mode()),
                        Some(Hit::App(i)) => {
                            if all[i].runnable() {
                                ask(&mut fb, pipe.as_mut(), &all[i].name, |p| p.open(&all[i].name));
                            }
                        }
                        Some(Hit::More) => screen.page = (screen.page + 1) % screen.pages(),
                        Some(Hit::Pair) => {
                            if let Some((server, port)) = screen.pairing.clone()
                                && pair::run(&mut fb, &mut touch, &server, port, &root, || {
                                    button_pressed(pipe.as_mut())
                                })
                            {
                                screen.pairing = None;
                            }
                            // Back from pairing, by the button or not: the first page.
                            screen.page = 0;
                        }
                        None => {}
                    }
                    // Taps made meanwhile were someone else's.
                    touch.poll();
                    draw(&mut fb, &all, &screen, None);
                }
            }
        }
        if button_pressed(pipe.as_mut()) {
            screen.button();
            pressed = None;
            draw(&mut fb, &all, &screen, None);
        }
        if last_clock.elapsed() >= Duration::from_secs(1) {
            draw_clock(&mut fb);
            last_clock = Instant::now();
        }
        neochumby::sleep_ms(30);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_goes_back_to_the_first_page() {
        let mut s = Screen { listed: (0..10).collect(), page: 2, pairing: None };
        assert!(s.pages() > 2);
        s.button();
        assert_eq!(s.page, 0);
        assert!(has_button(&["front wx".into(), "button".into()]));
        assert!(!has_button(&["front wx".into(), "buttons".into()]));
    }
}
