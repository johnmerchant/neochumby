//! neochumby's home screen, as an app (`home=` in its manifest): the
//! installed apps as a list, tap one to open it; PAIR when booted with a
//! neochumbyd this Chumby is not paired with; the time; and CHUMBY MODE,
//! which exits cleanly so the session hands the Chumby back to its own
//! software. The session falls back to it from any other home screen
//! (holding the top-left corner).
//!
//! An app that needs a neochumbyd this Chumby has not got is listed, greyed,
//! and does not open. An app whose `shells=` leaves out this home screen
//! ("home") is not listed.

mod pair;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use neochumby::apps::{self, App};
use neochumby::conf;
use neochumby::fb::{self, Fb, rgb};
use neochumby::input::{Touch, Touchscreen};

/// This home screen's app name, as other apps' `shells=` name it.
const SHELL: &str = "home";

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
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
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
                fb.rect(4, y, 8, ROW_H, if ok { a.colour } else { GREY });
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

fn launch(a: &App, exec: &Path, root: &Path) {
    let status = Command::new(exec).current_dir(&a.dir).env("NEOCHUMBY_ROOT", root).env("HOME", "/tmp").status();
    eprintln!("neochumby-home: {} exited: {status:?}", a.name);
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
    let mut touch = Touchscreen::open();
    let root = apps::root();
    let all = apps::all(&root);
    let mut screen = Screen {
        listed: all
            .iter()
            .enumerate()
            .filter(|(_, a)| a.title.is_some() && a.exec.is_some() && a.offered_in(SHELL))
            .map(|(i, _)| i)
            .collect(),
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
                        Some(Hit::ChumbyMode) => {
                            fb.clear(rgb(0, 0, 0));
                            return;
                        }
                        Some(Hit::App(i)) => {
                            if let (true, Some(exec)) = (all[i].runnable(), &all[i].exec) {
                                launch(&all[i], exec, &root);
                            }
                        }
                        Some(Hit::More) => screen.page = (screen.page + 1) % screen.pages(),
                        Some(Hit::Pair) => {
                            if let Some((server, port)) = screen.pairing.clone()
                                && pair::run(&mut fb, &mut touch, &server, port, &root)
                            {
                                screen.pairing = None;
                            }
                        }
                        None => {}
                    }
                    // Taps made meanwhile were someone else's.
                    touch.poll();
                    draw(&mut fb, &all, &screen, None);
                }
            }
        }
        if last_clock.elapsed() >= Duration::from_secs(1) {
            draw_clock(&mut fb);
            last_clock = Instant::now();
        }
        neochumby::sleep_ms(30);
    }
}
