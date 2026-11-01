//! neochumby-transitions: the animations the session plays between apps,
//! when this Chumby's settings say `transition=transitions`.
//!
//! The session's transition contract (its transition.rs): two pictures on
//! stdin, the screen as one app left it and as the next has drawn it, each
//! 320x240 RGB565, little-endian; frames of the same form on stdout,
//! shown as they arrive. It picks one of [`effects::ALL`] at random, or
//! the one NEOCHUMBY_EFFECT names, and plays it in about
//! [`effects::Effect::millis`], paced by the clock: a slow frame is
//! followed by a later one, never a slower animation.
//!
//! It runs on a host too, so effects can be looked at without a Chumby:
//!
//!   cat from.raw to.raw | NEOCHUMBY_EFFECT=melt neochumby-transitions > frames.raw
//!   neochumby-transitions --list

mod effects;

use std::io::{self, Read, Write};
use std::time::Instant;

use effects::{Effect, H, Rng, T1, W};

/// About 30 frames a second, if the Chumby keeps up.
const FRAME_MS: u32 = 33;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--list") {
        for e in effects::ALL {
            println!("{e}");
        }
        return;
    }
    let mut rng = Rng::seeded();
    let name = std::env::var("NEOCHUMBY_EFFECT").ok();
    let effect = match name.as_deref().map(|n| (n, effects::ALL.iter().find(|e| **e == n))) {
        Some((_, Some(e))) => e,
        Some((n, None)) => {
            eprintln!("neochumby-transitions: no effect {n}; one at random");
            effects::ALL[rng.below(effects::ALL.len() as u32) as usize]
        }
        None => effects::ALL[rng.below(effects::ALL.len() as u32) as usize],
    };
    let mut input = vec![0u8; W * H * 4];
    if let Err(e) = io::stdin().lock().read_exact(&mut input) {
        eprintln!("neochumby-transitions: reading the two screens: {e}");
        std::process::exit(1);
    }
    let (from, to) = input.split_at(W * H * 2);
    let (from, to) = (pixels(from), pixels(to));

    let mut fx = Effect::new(effect, &std::env::var("NEOCHUMBY_TO").unwrap_or_default(), &mut rng);
    let mut out = vec![0u16; W * H];
    let mut bytes = vec![0u8; W * H * 2];
    let mut stdout = io::stdout().lock();
    let millis = fx.millis();
    let start = Instant::now();
    loop {
        let began = start.elapsed().as_millis() as u32;
        let t = (began * T1 / millis).min(T1);
        fx.render(t, &from, &to, &mut out);
        for (b, p) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(&out) {
            *b = p.to_le_bytes();
        }
        // The session has stopped listening: it's done with us.
        if stdout.write_all(&bytes).and_then(|()| stdout.flush()).is_err() || t == T1 {
            return;
        }
        let spent = start.elapsed().as_millis() as u32 - began;
        neochumby_sys::time::sleep_ms(FRAME_MS.saturating_sub(spent).max(1));
    }
}

fn pixels(b: &[u8]) -> Vec<u16> {
    b.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes(*p)).collect()
}
