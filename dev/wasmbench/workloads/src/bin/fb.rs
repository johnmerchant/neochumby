//! fb: the framebuffer kernel (workloads::fb), each part timed.
//!
//!   fb [FRAMES]    100 frames by default
//!
//! Prints one line per part: its name, the frames, the milliseconds, and a
//! checksum of the buffer.

use std::time::Instant;

use workloads::fb::{self, H, S, W};
use workloads::{arg, ms};

fn main() {
    let frames = arg(1, 100);
    let mut back = vec![0u16; W * H];
    let mut front = vec![0u16; W * H];
    let mut sprite = vec![0u16; S * S];

    let t0 = Instant::now();
    fb::fill(&mut back, frames);
    let t = ms(t0);
    println!("fill frames={frames} ms={t:.1} sum={}", fb::sum(&back));

    let t0 = Instant::now();
    fb::copy(&mut back, &mut front, frames);
    let t = ms(t0);
    println!("copy frames={frames} ms={t:.1} sum={}", fb::sum(&front));

    fb::sprite(&mut sprite);
    let t0 = Instant::now();
    fb::blit(&mut front, &sprite, frames);
    let t = ms(t0);
    println!("blit frames={frames} ms={t:.1} sum={}", fb::sum(&front));

    let t0 = Instant::now();
    fb::rect(&mut front, frames);
    let t = ms(t0);
    println!("rect frames={frames} ms={t:.1} sum={}", fb::sum(&front));
}
