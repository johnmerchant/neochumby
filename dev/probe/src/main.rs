//! neochumby-probe: an app for the end-to-end scenarios (dev/e2e). It fills
//! the screen with a pattern a scenario can recognise (magenta, a white band
//! across the middle) and logs every touch it sees, and otherwise runs until
//! it is ended, as an app that never exits on its own would: the corner hold
//! is the way out.

#![forbid(unsafe_code)]

use neochumby::fb::{self, Fb, rgb};
use neochumby::input::{Touch, Touchscreen};

/// The pattern's colours, which the scenarios look for.
const MAGENTA: u16 = rgb(255, 0, 255);
const WHITE: u16 = rgb(255, 255, 255);

fn main() {
    neochumby::debug::install("neochumby-probe");
    let mut fb = Fb::open().expect("/dev/fb0");
    let mut touch = Touchscreen::open();
    fb.clear(MAGENTA);
    fb.rect(0, 100, fb::W, 40, WHITE);
    fb.text(120, 112, "PROBE", 2, rgb(0, 0, 0));
    eprintln!("neochumby-probe: running");
    loop {
        for t in touch.poll() {
            match t {
                Touch::Down { x, y, .. } => eprintln!("neochumby-probe: down {x} {y}"),
                Touch::Up => eprintln!("neochumby-probe: up"),
            }
        }
        neochumby::sleep_ms(50);
    }
}
