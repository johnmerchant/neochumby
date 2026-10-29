//! Flurry, the Mac OS X screensaver, drawn live on the Chumby: glowing
//! streams of smoke that follow sparks around an attractor.
//!
//! A port of Calum Robinson's Flurry 1.4 (2001-2007; the code Apple ships
//! in Mac OS X, and that xscreensaver's `hacks/glx/flurry*.c` derive from),
//! under its BSD licence, below and in `NOTICE` beside this crate's
//! Cargo.toml. Only its source was used: no binary, picture or other file
//! of Apple's.
//!
//! The model is the original's: the star (the attractor) and the sparks
//! move on the same curves, the smoke puffs are released at the star each
//! frame, pulled by every spark under the same gravity, drag and death
//! rules, and coloured the same way. It is in fixed point, as the Chumby
//! has no FPU: world positions in 1/16 units, velocities in 1/256 units a
//! second, time in microseconds, angles as fractions of a turn in a u32
//! (so they wrap by themselves) and a sine table built at compile time.
//!
//! The drawing is what OpenGL did for it, by hand: each puff is a quad
//! stretched along its motion on the screen, textured with one of 64 frames
//! of a speckled smoke texture (with mipmaps) and added to an 8:8:8 buffer;
//! before each frame the buffer is darkened. Then the buffer is converted
//! to RGB565. Rows with nothing left to fade are skipped.
//!
//! Where the port differs, on purpose:
//! - Puffs are released at 60 a second per stream, as the original does at
//!   60 fps (it releases one a frame), spread along the star's path through
//!   each frame. At 25 fps that gives the same streams; releasing once a
//!   frame leaves gaps between the puffs.
//! - A frame's light is scaled by (dt * 60)^(1/8), so that 25 fps is as
//!   bright as the original at 60 fps (it adds once a frame, and fades once
//!   a frame). xscreensaver scales by its frame rate too.
//! - The texture's animation steps at 60 frames a second, whatever the
//!   frame rate.
//! - The ring of puffs can be made smaller than the original's 3,600, to
//!   bound the work.
//!
//! ```text
//! Copyright (c) 2002, Calum Robinson
//! All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the following conditions are met:
//!
//! * Redistributions of source code must retain the above copyright notice, this
//!   list of conditions and the following disclaimer.
//!
//! * Redistributions in binary form must reproduce the above copyright notice,
//!   this list of conditions and the following disclaimer in the documentation
//!   and/or other materials provided with the distribution.
//!
//! * Neither the name of the author nor the names of its contributors may be used
//!   to endorse or promote products derived from this software without specific
//!   prior written permission.
//!
//! THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
//! ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
//! WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
//! DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE LIABLE FOR
//! ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
//! (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
//! LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON
//! ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
//! (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
//! SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
//! ```
//!
//! Our changes are MIT, as the rest of this crate.

use crate::manifest::{H, W};

const WU: usize = W as usize;
const HU: usize = H as usize;

// ------------------------------------------------------------ numbers

/// One, in the Q16 fixed point most of this uses.
const ONE: i32 = 1 << 16;

/// sin over a turn in 1024 steps (and one more, for interpolating past the
/// last), in Q16. Built with integers at compile time.
static SINE: [i32; 1025] = sine_table();

const fn sine_table() -> [i32; 1025] {
    let mut t = [0i32; 1025];
    let mut k = 0;
    while k <= 1024 {
        let q = k % 512;
        let m = if q > 256 { 512 - q } else { q };
        let v = sin_quarter(m);
        t[k] = if k >= 512 && k < 1024 { -v } else { v };
        k += 1;
    }
    t
}

/// sin(2πm/1024) for m in 0..=256, in Q16: a Taylor series in Q30.
const fn sin_quarter(m: usize) -> i32 {
    const PI_Q30: i64 = 3_373_259_426;
    let x = PI_Q30 * 2 * m as i64 / 1024;
    let x2 = (x * x) >> 30;
    let mut term = x;
    let mut sum = x;
    let mut n = 1i64;
    while n < 10 {
        term = -((term * x2) >> 30) / ((2 * n) * (2 * n + 1));
        sum += term;
        n += 1;
    }
    ((sum + (1 << 13)) >> 14) as i32
}

/// sin of an angle in turns (2^32 a turn), in Q16, interpolated.
fn sin(a: u32) -> i32 {
    let i = (a >> 22) as usize;
    let f = ((a >> 6) & 0xFFFF) as i32;
    let s0 = SINE[i];
    s0 + (((SINE[i + 1] - s0) * f) >> 16)
}

fn cos(a: u32) -> i32 {
    sin(a.wrapping_add(1 << 30))
}

/// A third of a turn.
const THIRD: u32 = 0x5555_5555;

/// The square root of n, rounded down (digit by digit: no division).
pub fn isqrt(n: u64) -> u32 {
    let mut rem = n;
    let mut root: u64 = 0;
    let mut bit: u64 = 1 << ((63 - n.leading_zeros().min(63)) & !1);
    while bit != 0 {
        if rem >= root + bit {
            rem -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    root as u32
}

/// The original's FastDistance2D: |(x, y)| within about 3.5%.
fn fast_distance(x: i32, y: i32) -> i32 {
    let (x, y) = (x.abs(), y.abs());
    let mn = x.min(y);
    x + y - (mn >> 1) - (mn >> 2) + (mn >> 4)
}

/// r^-3 for gravity, from r^2 = m 2^s: 2^40 / m^1.5 for m = 64 i, i in
/// 256..=1024 (m from 2^14 to 2^16). Interpolated, it is within 0.01%.
static INV_R3: [u32; 769] = inv_r3_table();

const fn inv_r3_table() -> [u32; 769] {
    let mut t = [0u32; 769];
    let mut i = 0;
    while i < 769 {
        let m = (256 + i as u64) * 64;
        // m^1.5 in Q16: m times sqrt(m) in Q16.
        let m15 = m * (m << 32).isqrt();
        t[i] = ((1u64 << 56) / m15) as u32;
        i += 1;
    }
    t
}

/// 2^12 k / r^3 for r^2 (any units), from the table: no division. k is
/// given scaled down by 2^ks, to stay below 2^31.
#[inline]
fn pull(k: u64, ks: u32, r2: u64) -> u64 {
    let bits = 64 - r2.leading_zeros();
    // An even shift leaves m in 2^14..2^16 (closer than that, the pull is
    // capped: at 8 units, as m is at least 2^14).
    let s = if bits > 16 { (bits - 15) & !1 } else { 0 };
    let m = ((r2 >> s) as u32).max(1 << 14);
    let i = (m >> 6) as usize - 256;
    let (a, b) = (INV_R3[i], INV_R3[i + 1]);
    let f = a - (((a - b) * (m & 63)) >> 6);
    let x = k * f as u64;
    // 1/r^3 = f / 2^40 / 2^(1.5 s).
    let sh = (28 + s * 3 / 2).saturating_sub(ks);
    if sh >= 64 { 0 } else { x >> sh }
}

/// Turns a µs, in Q48, for `num / den` turns a second (init time only).
fn rate(num: u128, den: u128) -> u64 {
    ((num << 48) / (den * 1_000_000)) as u64
}

/// The angle at `t` µs for a rate from [`rate`]. Only bits 16..47 of the
/// product are kept, and a wrapping multiply leaves those exact, so the
/// angle wraps as an angle should, for any t.
fn angle(t: i64, rate: u64) -> u32 {
    ((t as u64).wrapping_mul(rate) >> 16) as u32
}

/// num/den radians, as a turn fraction (init time only).
fn radians(num: u128, den: u128) -> u32 {
    // 2π = 6.283185307179586...
    ((num << 32) * 1_000_000_000_000 / (den * 6_283_185_307_180)) as u32
}

/// The generator: xorshift32, seeded per scene.
#[derive(Clone)]
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Rng {
        Rng(crate::draw::hash(seed) | 1)
    }

    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// Uniform in 0..1, Q16.
    fn unit(&mut self) -> i32 {
        (self.next() >> 16) as i32
    }

    /// The original's RandBell: -1..1 around 0, Q16, scaled by `scale_q16`.
    fn bell(&mut self, scale_q16: i32) -> i32 {
        let s = self.unit() + self.unit() + self.unit();
        let b = ONE - s * 2 / 3;
        ((b as i64 * scale_q16 as i64) >> 16) as i32
    }
}

// ------------------------------------------------------------ options

/// The original's colour modes, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Red,
    Magenta,
    Blue,
    Cyan,
    Green,
    Yellow,
    SlowCyclic,
    Cyclic,
    Tiedye,
    Rainbow,
    White,
    Multi,
    Dark,
}

pub const COLOURS: [(&str, Colour); 13] = [
    ("red", Colour::Red),
    ("magenta", Colour::Magenta),
    ("blue", Colour::Blue),
    ("cyan", Colour::Cyan),
    ("green", Colour::Green),
    ("yellow", Colour::Yellow),
    ("slowcyclic", Colour::SlowCyclic),
    ("cyclic", Colour::Cyclic),
    ("tiedye", Colour::Tiedye),
    ("rainbow", Colour::Rainbow),
    ("white", Colour::White),
    ("multi", Colour::Multi),
    ("dark", Colour::Dark),
];

impl Colour {
    pub fn parse(s: &str) -> Option<Colour> {
        COLOURS.iter().find(|(n, _)| *n == s).map(|&(_, c)| c)
    }
}

/// One flurry: a star, its sparks (streams) and their smoke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    /// Sparks, each a stream: 1..=MAX_STREAMS.
    pub streams: usize,
    pub colour: Colour,
    /// How fast a stream thickens (the original's streamExpansion).
    pub thickness: i32,
    /// The star's speed, 1/256ths (the original's rotSpeed; 256 = 1.0).
    pub speed: i32,
    /// Brightness, 1/256ths (256 = as the original).
    pub bright: i32,
}

pub const MAX_STREAMS: usize = 16;
/// Puffs alive at once at most, over a scene's flurries (the original's
/// NUMSMOKEPARTICLES, per flurry).
pub const MAX_PUFFS: usize = 3600;

const fn spec(streams: usize, colour: Colour, thickness: i32, speed: i32) -> Spec {
    Spec { streams, colour, thickness, speed, bright: 256 }
}

/// The original's presets (FlurryUtils.m), by name. "random" picks one.
pub const PRESETS: [(&str, &[Spec]); 5] = [
    ("classic", &[spec(5, Colour::Tiedye, 100, 256)]),
    ("rgb", &[spec(3, Colour::Red, 100, 205), spec(3, Colour::Green, 100, 205), spec(3, Colour::Blue, 100, 205)]),
    ("water", &[spec(1, Colour::Blue, 100, 512); 9]),
    ("fire", &[spec(12, Colour::SlowCyclic, 10000, 0)]),
    ("psychedelic", &[spec(10, Colour::Rainbow, 200, 512)]),
];

/// What a `flurry` line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// An index into PRESETS, or None: a random one each time it starts.
    pub preset: Option<usize>,
    pub streams: Option<usize>,
    pub colour: Option<Colour>,
    pub thickness: Option<i32>,
    pub speed: Option<i32>,
    pub bright: Option<i32>,
    /// None: a new seed each time it starts.
    pub seed: Option<u32>,
    /// The ring of puffs, shared out over the flurries (MAX_PUFFS by default).
    pub puffs: usize,
    /// Puffs each stream releases a second. The original releases one a
    /// frame, so 60 at 60 fps; None: that, but at most RELEASES a second
    /// over all the streams (see [`Options::rate_for`]).
    pub rate: Option<i32>,
}

/// The most puffs released a second over all a scene's streams, unless a
/// `rate=` says otherwise: the classic preset's 5 x 60. A preset with more
/// streams releases fewer from each, each brighter, to keep the Chumby at
/// its frame rate (measured on one: 25 fps at about half its CPU).
pub const RELEASES: i32 = 300;

impl Default for Options {
    fn default() -> Options {
        Options {
            preset: Some(0),
            streams: None,
            colour: None,
            thickness: None,
            speed: None,
            bright: None,
            seed: None,
            puffs: MAX_PUFFS,
            rate: None,
        }
    }
}

/// "12", "0.8" or "1.25" to 256ths; None if it isn't such a number.
fn parse_q8(t: &str) -> Option<i32> {
    let m = crate::manifest::parse_milli(t);
    (m >= 0).then(|| ((m as i64 * 256 + 500) / 1000).min(i32::MAX as i64) as i32)
}

impl Options {
    /// The words after `flurry PAGE`: `[PRESET] [streams=N] [colour=NAME]
    /// [speed=X] [thickness=N] [bright=X] [seed=N] [puffs=N] [rate=N]`. None (skip
    /// the line) for an unknown preset or a value out of range; an unknown
    /// key is ignored, so a later engine's options don't break this one.
    pub fn parse<'a>(words: impl Iterator<Item = &'a str>) -> Option<Options> {
        let mut o = Options::default();
        for (i, w) in words.enumerate() {
            let Some((k, v)) = w.split_once('=') else {
                if i > 0 {
                    return None;
                }
                o.preset = if w == "random" { None } else { Some(PRESETS.iter().position(|(n, _)| *n == w)?) };
                continue;
            };
            let num = || v.parse::<i64>().ok().filter(|_| v.bytes().all(|b| b.is_ascii_digit()));
            match k {
                "streams" => o.streams = Some(num().filter(|n| (1..=MAX_STREAMS as i64).contains(n))? as usize),
                "colour" | "color" => o.colour = Some(Colour::parse(v)?),
                "speed" => o.speed = Some(parse_q8(v).filter(|s| *s <= 4 * 256)?),
                "thickness" => o.thickness = Some(num().filter(|n| (1..=20000).contains(n))? as i32),
                "bright" => o.bright = Some(parse_q8(v).filter(|b| (16..=4 * 256).contains(b))?),
                "seed" => o.seed = Some(num().filter(|n| *n <= u32::MAX as i64)? as u32),
                "puffs" => o.puffs = num().filter(|n| (64..=MAX_PUFFS as i64).contains(n))? as usize,
                "rate" => o.rate = Some(num().filter(|n| (10..=121).contains(n))? as i32),
                _ => {}
            }
        }
        Some(o)
    }

    /// Puffs a second per stream, for these flurries.
    pub fn rate_for(&self, specs: &[Spec]) -> i32 {
        let streams: usize = specs.iter().map(|s| s.streams).sum();
        self.rate.unwrap_or((RELEASES / streams.max(1) as i32).clamp(20, 60))
    }

    /// The flurries to run, for a scene seeded with `seed`.
    pub fn specs(&self, seed: u32) -> Vec<Spec> {
        let p = self.preset.unwrap_or(crate::draw::hash(seed ^ 0x5eed) as usize % PRESETS.len());
        PRESETS[p]
            .1
            .iter()
            .map(|s| Spec {
                streams: self.streams.unwrap_or(s.streams),
                colour: self.colour.unwrap_or(s.colour),
                thickness: self.thickness.unwrap_or(s.thickness),
                speed: self.speed.unwrap_or(s.speed),
                bright: self.bright.unwrap_or(s.bright),
            })
            .collect()
    }
}

// ------------------------------------------------------------ the model

// The original's constants (Gl_saver.h), in this module's units.
/// gravity 1500000
const GRAVITY: i64 = 1_500_000;
/// streamBias 7: a puff is pulled 1 + 7 times as hard by its own spark.
const STREAM_BIAS: i64 = 8;
/// streamSpeed 450, units a second.
const STREAM_SPEED: i64 = 450;
/// seraphDistance 2000, in 1/16 units.
const SERAPH_DISTANCE: i32 = 2000 * 16;
/// fieldRange 1000 (the sparks), and the star's 250.
const FIELD_RANGE: i32 = 1000;
const STAR_RANGE: i32 = 250;
/// streamSize 25000.
const STREAM_SIZE: i32 = 25000;
/// A puff faster than 5000 units a second dies (in 1/256 units, squared).
const MAX_SPEED_SQ: i64 = (5000 * 256) * (5000 * 256);
/// fieldSpeed 12, over MAXANGLES 16384: the sparks' turns a second.
const FIELD_TURNS: (u128, u128) = (12, 16384);
/// incohesion 0.07 (a quarter of it, as used) and colorIncoherence 0.15, Q16.
const SPEED_JITTER: i32 = (0.07 / 4.0 * 65536.0) as i32;
const COLOUR_JITTER: i32 = (0.15 * 65536.0) as i32;
/// The original's random time offset: 0 to 300 s.
const SEED_US: u32 = 300_000_000;

/// A spark or the star: a point on the original's curve.
struct Mover {
    /// Position, 1/16 units.
    pos: [i32; 3],
    /// The curve's size: FIELD_RANGE or STAR_RANGE.
    range: i32,
    /// cf's offset: 2.0 for sparks, 0.75 for the star, Q16.
    cf_base: i32,
    /// mystery/1800 turns, and the rotations' fixed parts.
    phase: u32,
    rot1: u32,
    rot2: u32,
    /// Turns a µs (Q48): the curve, and its rotations (x0.501, x2.501).
    rate: u64,
    rate1: u64,
    rate2: u64,
}

impl Mover {
    /// mystery as a fraction num/den (the sparks' are whole, the star's not).
    fn new(num: u128, den: u128, range: i32, cf_base: i32, speed: i32) -> Mover {
        let sp = speed.max(0) as u128;
        let (fnum, fden) = FIELD_TURNS;
        Mover {
            pos: [0; 3],
            range,
            cf_base,
            phase: ((num << 32) / (den * 1800)) as u32,
            rot1: radians(num * 501, den * 1800 * 100),
            rot2: radians(num * 8501, den * 1800 * 100),
            rate: rate(fnum * sp, fden * 256),
            rate1: rate(fnum * sp * 501, fden * 256 * 1000),
            rate2: rate(fnum * sp * 2501, fden * 256 * 1000),
        }
    }

    /// UpdateSpark / UpdateStar: where it is at `t` µs.
    fn update(&mut self, t: i64) {
        let a = angle(t, self.rate);
        let p = self.phase;
        let cf = (cos(a.wrapping_mul(7)) + cos(a.wrapping_mul(3)) + cos(a.wrapping_mul(13))) / 6 + self.cf_base;
        let r = (self.range * 16) as i64;
        let m = |k: u32, j: u32| p.wrapping_add(a.wrapping_mul(j)).wrapping_mul(k);
        let x = (r * cf as i64 * cos(m(11, 3)) as i64) >> 32;
        let y = (r * cf as i64 * sin(m(12, 4)) as i64) >> 32;
        let z = (r * cos(m(23, 12)) as i64) >> 16;

        let rot = angle(t, self.rate1).wrapping_add(self.rot1);
        let (c, s) = (cos(rot) as i64, sin(rot) as i64);
        let x1 = (x * c - y * s) >> 16;
        let y1 = (y * c + x * s) >> 16;
        let z1 = z;
        let x2 = (x1 * c - z1 * s) >> 16;
        let y2 = y1;
        let z2 = (z1 * c + x1 * s) >> 16;
        let x3 = x2;
        let y3 = (y2 * c - z2 * s) >> 16;
        let z3 = ((z2 * c + y2 * s) >> 16) + SERAPH_DISTANCE as i64;

        let rot = angle(t, self.rate2).wrapping_add(self.rot2);
        let (c, s) = (cos(rot) as i64, sin(rot) as i64);
        self.pos = [((x3 * c - y3 * s) >> 16) as i32, ((y3 * c + x3 * s) >> 16) as i32, z3 as i32];
    }
}

/// A smoke puff (the original's SmokeParticle).
#[derive(Clone, Copy, Default)]
struct Puff {
    pos: [i32; 3],
    old: [i32; 3],
    /// Velocity, 1/256 units a second.
    vel: [i32; 3],
    /// RGBA, Q12.
    col: [i32; 4],
    /// When it was released (the flurry's time, µs).
    born: i64,
    /// Its texture frame, 0..63.
    anim: u8,
    live: bool,
    /// Where it was drawn last frame (screen x, y in Q8, and 2^31 / z),
    /// so the quad's back end needn't be projected again; None if it
    /// wasn't projected.
    shown: Option<(i32, i32, u32)>,
}

struct Flurry {
    spec: Spec,
    star: Mover,
    sparks: Vec<Mover>,
    /// Each spark's colour, Q12 (may be below 0: drawn as 0).
    spark_col: Vec<[i32; 3]>,
    puffs: Vec<Puff>,
    /// The oldest slot that may be alive, and how many from it may be.
    tail: usize,
    count: usize,
    first: bool,
    /// Releases a second per stream, and the part of one owed.
    rate: i64,
    owed: i64,
    old_star: [i32; 3],
    /// The original's flurryRandomSeed, µs: added to its time.
    seed_us: i64,
    /// Its time (fTime) and the frame before's, µs.
    time: i64,
    old_time: i64,
    frames: i64,
    /// The colour cycle's turns a µs (Q48), for the cycling modes.
    colour_rate: u64,
}

impl Flurry {
    fn new(spec: Spec, puffs: usize, per_sec: i32, rng: &mut Rng) -> Flurry {
        let seed_us = (rng.next() % SEED_US) as i64;
        // The star's mystery: RandFlt(0, 10), in 1/65536ths.
        let mystery = (rng.next() % (10 << 16)) as u128;
        let sparks: Vec<Mover> = (0..spec.streams)
            .map(|i| Mover::new((1800 * (i as u128 + 1)) / 13, 1, FIELD_RANGE, 2 * ONE, 256))
            .collect();
        let cycle_s = match spec.colour {
            Colour::Rainbow => (3, 2),
            Colour::Tiedye => (9, 2),
            Colour::SlowCyclic => (120, 1),
            _ => (20, 1),
        };
        let mut f = Flurry {
            spec,
            star: Mover::new(mystery, 65536, STAR_RANGE, ONE * 3 / 4, spec.speed),
            spark_col: vec![[0; 3]; sparks.len()],
            sparks,
            puffs: vec![Puff::default(); puffs],
            tail: 0,
            count: 0,
            first: true,
            rate: per_sec as i64,
            owed: 0,
            // InitSmoke: RandFlt(-100, 100).
            old_star: [0; 3].map(|_: i32| (rng.next() % 3200) as i32 - 1600),
            seed_us,
            time: seed_us,
            old_time: seed_us,
            frames: 0,
            colour_rate: rate(cycle_s.1, cycle_s.0),
        };
        f.update_sparks();
        f
    }

    /// The sparks' places and colours now (UpdateSpark).
    fn update_sparks(&mut self) {
        let t = self.time;
        let a = angle(t, self.sparks.first().map_or(0, |s| s.rate));
        let ct = t + self.seed_us; // the original adds the seed twice here
        let base = |shift: u32| -> i32 {
            let turn = match self.spec.colour {
                c if (c as u32) < Colour::SlowCyclic as u32 => (c as u32).wrapping_mul(0x2AAA_AAAB),
                _ => angle(ct, self.colour_rate),
            };
            (448 * (cos(turn.wrapping_add(shift)) + ONE)) >> 16
        };
        let b = match self.spec.colour {
            Colour::White => [768; 3],
            Colour::Multi => [256; 3],
            Colour::Dark => [0; 3],
            _ => [base(0), base(THIRD), base(THIRD.wrapping_mul(2))],
        };
        for (s, col) in self.sparks.iter_mut().zip(&mut self.spark_col) {
            s.update(t);
            let p = s.phase;
            let m = |k: u32, j: u32| p.wrapping_add(a.wrapping_mul(j)).wrapping_mul(k);
            col[0] = b[0] + ((256 * (ONE / 2 + cos(m(15, 3)) + sin(m(7, 1)))) >> 16);
            col[1] = b[1] + ((256 * (ONE / 2 + sin(m(1, 1)))) >> 16);
            col[2] = b[2] + ((256 * (ONE / 2 + cos(m(37, 1)))) >> 16);
        }
    }

    /// One step of the model to time `now` (the flurry's, µs).
    fn step(&mut self, now: i64, rng: &mut Rng) {
        self.frames += 1;
        self.old_time = self.time;
        self.time = now;
        // At least 1 ms, at most 200: a stall must not fling the puffs.
        let dt = (self.time - self.old_time).clamp(1000, 200_000);
        // drag = 0.9965^(85 dt) = exp(-0.298016 dt): the series, in Q16.
        let x = (dt * 19531 / 1_000_000) as i32; // 0.298016 in Q16, times dt in s
        let drag = ONE - x + ((x * x) >> 17) - ((((x * x) >> 16) * x) / (6 << 16));
        // 42.5 / (the frame rate since the start, seed included), Q16.
        let frm = ((self.time.max(0) as u64 * 2_785_280) / (self.frames as u64 * 1_000_000)).min(40_000 << 16) as i64;
        let dt_q16 = (dt << 16) / 1_000_000;

        self.star.update(self.time);
        self.update_sparks();
        self.release(dt, rng);
        self.old_star = self.star.pos;

        let n = self.sparks.len();
        // The pull's constant, in 1/256 units a second: G frm, scaled down
        // by 2^ks to fit pull()'s 31 bits (with the bias).
        let k = ((GRAVITY * 256 * frm) >> 16).max(1) as u64;
        let ks = (64 - k.leading_zeros()).saturating_sub(27);
        let (k, k_own) = (k >> ks, (k >> ks) * STREAM_BIAS as u64);
        let len = self.puffs.len();
        for n_in in 0..self.count {
            let slot = (self.tail + n_in) % len;
            let p = &mut self.puffs[slot];
            if !p.live {
                continue;
            }
            let mut v = [p.vel[0] as i64, p.vel[1] as i64, p.vel[2] as i64];
            for (j, s) in self.sparks.iter().enumerate() {
                let d = [(p.pos[0] - s.pos[0]) as i64, (p.pos[1] - s.pos[1]) as i64, (p.pos[2] - s.pos[2]) as i64];
                let r2 = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) as u64;
                if r2 > (1 << 38) {
                    continue; // over 32768 units away: no pull worth having
                }
                let kk = if slot % n == j { k_own } else { k };
                // dv = d k / r^3: pull() gives k / r^3 in 2^-12 units
                // (r^2 is in 1/256ths, d in 1/16ths: 2^-12 r^-3 over all).
                let a = pull(kk, ks, r2).min(1 << 42) as i64;
                for c in 0..3 {
                    let dv = (d[c] * a) >> 4;
                    v[c] -= dv.clamp(-(1 << 28), 1 << 28);
                }
            }
            for c in &mut v {
                *c = (*c * drag as i64) >> 16;
            }
            if v[0] * v[0] + v[1] * v[1] + v[2] * v[2] >= MAX_SPEED_SQ {
                p.live = false;
                continue;
            }
            p.old = p.pos;
            for ((vel, pos), &v) in p.vel.iter_mut().zip(&mut p.pos).zip(&v) {
                *vel = v as i32;
                *pos += ((v * dt_q16) >> 20) as i32;
            }
        }
    }

    /// Releases puffs at the star (UpdateSmoke's first half). The
    /// original releases one per stream each frame, so its look is that of
    /// 60 a second; at a lower frame rate this releases as many, spread
    /// along the star's path through the frame.
    fn release(&mut self, dt: i64, rng: &mut Rng) {
        if self.first {
            self.first = false;
            return;
        }
        self.owed += dt * self.rate;
        let k = (self.owed / 1_000_000).min(8) as i32;
        self.owed %= 1_000_000;
        let (from, to) = (self.old_star, self.star.pos);
        let len = self.puffs.len();
        for j in 1..=k {
            let sp = [0, 1, 2].map(|c| from[c] + (to[c] - from[c]) * j / k);
            // The star's step, x5 (in 1/256 units a second): as the
            // original's from one of its frames to the next.
            let base = [0, 1, 2].map(|c| (from[c] - to[c]) * 80 / k);
            let born = self.old_time + (self.time - self.old_time) * j as i64 / k as i64;
            for (s, col) in self.sparks.iter().zip(&self.spark_col) {
                let coherence = (ONE + rng.bell(SPEED_JITTER)).max(0) as i64;
                let d = [0, 1, 2].map(|c| (sp[c] - s.pos[c]) as i64);
                let r = isqrt((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) as u64).max(1) as i64;
                let f = (STREAM_SPEED * 256 * coherence) >> 16;
                let jitter =
                    |rng: &mut Rng, c: i32, scale: i32| ((c as i64 * (ONE + rng.bell(scale)) as i64) >> 16) as i32;
                let mut puff = Puff {
                    pos: sp,
                    old: sp,
                    vel: [0, 1, 2].map(|c| base[c] - (d[c] * f / r) as i32),
                    col: [0; 4],
                    born,
                    anim: (rng.next() & 63) as u8,
                    live: true,
                    shown: None,
                };
                for (pc, &c) in puff.col.iter_mut().zip(col) {
                    *pc = jitter(rng, c, COLOUR_JITTER);
                }
                puff.col[3] = jitter(rng, 3482, COLOUR_JITTER / 2); // 0.85
                let at = (self.tail + self.count) % len;
                if self.count == len {
                    self.tail = (self.tail + 1) % len; // the ring is full: the oldest goes
                } else {
                    self.count += 1;
                }
                self.puffs[at] = puff;
            }
        }
    }
}

// ------------------------------------------------------------ the texture

/// The smoke texture: 64 frames of 32x32 (the original's 256x256 atlas),
/// each with its mipmaps down to 1x1. Each texel is t*t/255 (OpenGL
/// modulated both the colour and the alpha by t), in 64 steps. Every level
/// is laid out 32 texels a row, so the drawing loop's index is the same
/// shift for all of them.
struct Texture {
    levels: Vec<[u8; 1024]>,
}

const LEVELS: usize = 6;

impl Texture {
    /// MakeTexture, in integers.
    fn new(rng: &mut Rng) -> Texture {
        // 255 cos(r π / 31) inside r <= 15, r from the cell's centre.
        let disc = |i: usize, j: usize| -> i32 {
            let (a, b) = (2 * i as i64 - 31, 2 * j as i64 - 31);
            let r_q8 = isqrt(((a * a + b * b) << 16) as u64) as i64 / 2;
            if r_q8 > 15 << 8 {
                0
            } else {
                let turn = ((r_q8 << 32) / (62 << 8)) as u32; // r/62 turns
                (255 * cos(turn)) >> 16
            }
        };
        let mut frames: Vec<[[u8; 32]; 32]> = Vec::with_capacity(64);
        let mut small = [[0u8; 32]; 32];
        for n in 0..64 {
            if n == 63 {
                // The last: the one before and the first, averaged, so the
                // animation loops smoothly.
                for (row, first) in small.iter_mut().zip(&frames[0]) {
                    for (v, &f) in row.iter_mut().zip(first) {
                        *v = ((*v as u32 + f as u32) / 2) as u8;
                    }
                }
            } else {
                for (i, row) in small.iter_mut().enumerate() {
                    for (j, v) in row.iter_mut().enumerate() {
                        let t = disc(i, j);
                        *v = if n == 0 { t as u8 } else { ((t + 2 * *v as i32) / 3).min(255) as u8 };
                    }
                }
                speckle(&mut small, rng);
                smooth(&mut small);
                smooth(&mut small);
            }
            frames.push(small);
        }
        let mut levels = Vec::with_capacity(64 * LEVELS);
        for f in &frames {
            let mut lvl: Vec<u32> = f.iter().flatten().map(|&v| v as u32).collect();
            let mut size = 32;
            for _ in 0..LEVELS {
                let mut out = [0u8; 1024];
                for (k, &v) in lvl.iter().enumerate() {
                    out[k / size * 32 + k % size] = (v * v / 255 / 4) as u8;
                }
                levels.push(out);
                if size > 1 {
                    let h = size / 2;
                    lvl = (0..h * h)
                        .map(|k| {
                            let (y, x) = (k / h * 2, k % h * 2);
                            let at = |y: usize, x: usize| lvl[y * size + x];
                            (at(y, x) + at(y, x + 1) + at(y + 1, x) + at(y + 1, x + 1) + 2) / 4
                        })
                        .collect();
                    size = h;
                }
            }
        }
        Texture { levels }
    }

    /// Level `lv` of frame `frame`.
    fn level(&self, frame: usize, lv: usize) -> &[u8; 1024] {
        &self.levels[frame * LEVELS + lv]
    }
}

/// SpeckleTexture: random brighter and darker specks.
fn speckle(t: &mut [[u8; 32]; 32], rng: &mut Rng) {
    for row in t.iter_mut().take(30).skip(2) {
        for v in row.iter_mut().take(30).skip(2) {
            let mut speck = 1;
            while speck <= 32 && rng.next() & 1 != 0 {
                *v = (*v as u32 + speck).min(255) as u8;
                speck += speck;
            }
            speck = 1;
            while speck <= 32 && rng.next() & 1 != 0 {
                *v = (*v as i32 - speck as i32).max(0) as u8;
                speck += speck;
            }
        }
    }
}

/// SmoothTexture: (4 x centre + the 4 neighbours) / 8, inside the border.
fn smooth(t: &mut [[u8; 32]; 32]) {
    let s = *t;
    for i in 1..31 {
        for j in 1..31 {
            let v =
                s[i][j] as u32 * 4 + s[i - 1][j] as u32 + s[i + 1][j] as u32 + s[i][j - 1] as u32 + s[i][j + 1] as u32;
            t[i][j] = (v / 8) as u8;
        }
    }
}

// ------------------------------------------------------------ the scene

/// The light buffer: 8 bits a channel, as the original's framebuffer:
/// blue in bits 0-7, green 9-16, red 18-25. The spare bit above each
/// catches its overflow, so an add saturates per channel in a few steps.
const OVER: u32 = (1 << 8) | (1 << 17) | (1 << 26);
const RB: u32 = 0xFF | (0xFF << 18);
const GG: u32 = 0xFF << 9;

/// What a frame cost, in µs, for the stats and the bench.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cost {
    pub model_us: i64,
    pub draw_us: i64,
    pub pass_us: i64,
    pub puffs: u32,
    pub quads: u32,
    pub pixels: u32,
}

/// A running Flurry: its flurries and the light buffer they add to.
pub struct Scene {
    flurries: Vec<Flurry>,
    tex: Texture,
    rng: Rng,
    buf: Vec<u32>,
    /// Per row: 2, light to fade; 1, faded out, but the screen still shows
    /// the last of it; 0, black.
    rows: Vec<u8>,
    /// The light each texel value adds, for the quad being drawn.
    lut: [u32; 64],
    start: Option<i64>,
    /// The last frame's time (ms) and its length (µs).
    last: Option<i64>,
    dt: i64,
    /// Texture frames to step this frame (at 60 a second), and the remainder.
    anim_step: u8,
    anim_acc: i64,
    pub cost: Cost,
}

/// The rows a pass wrote, first and last; None if none.
pub type Rows = Option<(usize, usize)>;

impl Scene {
    /// A scene for `opt`, seeded with `seed` unless the options fix one.
    pub fn new(opt: &Options, seed: u32) -> Scene {
        let seed = opt.seed.unwrap_or(seed);
        let mut rng = Rng::new(seed);
        let tex = Texture::new(&mut rng);
        let specs = opt.specs(seed);
        let per = (opt.puffs / specs.len().max(1)).max(64);
        let rate = opt.rate_for(&specs);
        let flurries = specs.into_iter().map(|s| Flurry::new(s, per, rate, &mut rng)).collect();
        Scene {
            flurries,
            tex,
            rng,
            buf: vec![0; WU * HU],
            rows: vec![1; HU],
            lut: [0; 64],
            start: None,
            last: None,
            dt: 40_000,
            anim_step: 0,
            anim_acc: 0,
            cost: Cost::default(),
        }
    }

    /// Advances the model to `t` ms (any clock; the first call is the start).
    pub fn step(&mut self, t: i64) {
        let start = *self.start.get_or_insert(t);
        if let Some(l) = self.last {
            self.dt = ((t - l) * 1000).clamp(1000, 200_000);
        }
        self.last = Some(t);
        let el = (t - start) * 1000;
        for f in &mut self.flurries {
            f.step(f.seed_us + el, &mut self.rng);
        }
        self.anim_acc += self.dt * 60;
        self.anim_step = (self.anim_acc / 1_000_000).min(63) as u8;
        self.anim_acc %= 1_000_000;
    }

    /// Adds every live puff to the buffer (DrawSmoke_Scalar).
    pub fn draw(&mut self) {
        // (dt * 60)^(1/8), Q16: the light a frame, against the original at
        // 60 fps. Released at 60 a second, puffs leave as much light at 25
        // fps, nearly: their quads are longer. This was matched by the mean
        // light of frames at the same times, at 25 and at 60 fps.
        let mut gain = (self.dt * 60 * 65536 / 1_000_000) as u64;
        for _ in 0..3 {
            gain = isqrt(gain << 16) as u64;
        }
        let gain = gain as i64;
        let mut quads = 0;
        let mut pixels = 0;
        let mut puffs = 0;
        for f in &mut self.flurries {
            let exp = f.spec.thickness.max(0) as i64;
            // Widths in screen units: (streamSize + age * expansion) * W / 1024, Q8.
            let width = ((STREAM_SIZE as i64 * 256 + exp * 640) * W as i64) >> 10;
            let inv_width = (1i64 << 40) / width;
            // The expansion a µs, Q8 and 2^20: age * exp / 10^6 without a division.
            let grow = (exp << 28) / 1_000_000;
            // Fewer puffs than the original's 60 a second, each brighter, by
            // (60 / rate)^(3/8): matched by mean light, as the gain above.
            let r2 = isqrt((60u64 << 32) / f.rate.max(1) as u64) as u64;
            let r4 = isqrt(r2 << 16) as u64;
            let fewer = ((r4 * isqrt(r4 << 16) as u64) >> 16) as i64;
            let bright = (((gain * f.spec.bright as i64) >> 8) * fewer) >> 16;
            let len = f.puffs.len();
            for n_in in 0..f.count {
                let p = &mut f.puffs[(f.tail + n_in) % len];
                if !p.live {
                    continue;
                }
                let age = f.time - p.born; // µs
                let this = ((STREAM_SIZE as i64 * 256 + ((age * grow) >> 20)) * W as i64) >> 10;
                if this >= width {
                    p.live = false;
                    continue;
                }
                puffs += 1;
                // cm = 1.375 - this/width, Q16, then the frame's light.
                let cm = ((ONE as i64 * 11 / 8 - ((this * inv_width) >> 24)) * bright) >> 16;
                if let Some(n) =
                    draw_puff(&mut self.buf, &mut self.rows, &self.tex, &mut self.lut, p, this, cm, self.anim_step)
                {
                    quads += 1;
                    pixels += n;
                }
            }
            while f.count > 0 && !f.puffs[f.tail].live {
                f.tail = (f.tail + 1) % len;
                f.count -= 1;
            }
        }
        self.cost.puffs = puffs;
        self.cost.quads = quads;
        self.cost.pixels = pixels;
    }

    /// The buffer to the screen as RGB565, then darkened for the next
    /// frame. Every row if `all`, else only those with light (or just
    /// faded out); returns the rows written.
    pub fn pass(&mut self, out: &mut [u16], all: bool) -> Rows {
        // 1 - min(0.2, 5 dt), in 256ths.
        let k = 256 - (self.dt * 5 * 256 / 1_000_000).min(51) as u32;
        let mut span: Rows = None;
        for (y, state) in self.rows.iter_mut().enumerate() {
            if !all && *state == 0 {
                continue;
            }
            span = Some(span.map_or((y, y), |(a, _)| (a, y)));
            let row = &mut self.buf[y * WU..(y + 1) * WU];
            let o = &mut out[y * WU..(y + 1) * WU];
            let any = fade_row(row, o, k);
            *state = if any != 0 {
                2
            } else if *state == 2 {
                1
            } else {
                0
            };
        }
        span
    }

    /// One whole frame at `t` ms into `out`; see [`Scene::pass`]. `now_us`
    /// times the parts, for [`Scene::cost`].
    pub fn frame(&mut self, t: i64, out: &mut [u16], all: bool, now_us: impl Fn() -> i64) -> Rows {
        let t0 = now_us();
        self.step(t);
        let t1 = now_us();
        self.draw();
        let t2 = now_us();
        let rows = self.pass(out, all);
        let t3 = now_us();
        self.cost.model_us = t1 - t0;
        self.cost.draw_us = t2 - t1;
        self.cost.pass_us = t3 - t2;
        rows
    }
}

/// One row of the buffer to RGB565, and darkened by k/256; returns the
/// light left (0: none).
#[inline(never)]
fn fade_row(row: &mut [u32], out: &mut [u16], k: u32) -> u32 {
    let mut any = 0;
    for (p, o) in row.iter_mut().zip(out.iter_mut()) {
        let v = *p;
        *o = (((v >> 10) & 0xF800) | ((v >> 6) & 0x07E0) | ((v >> 3) & 0x001F)) as u16;
        if v != 0 {
            let rb = ((((v & RB) as u64 * k as u64) >> 8) as u32) & RB;
            let f = rb | ((((v & GG) * k) >> 8) & GG);
            *p = f;
            any |= f;
        }
    }
    any
}

/// A point's place on the screen, Q8, for r = 2^31 / z (z in 1/16 units):
/// x W / z + W/2, and OpenGL's y, which runs up the screen.
fn screen(x: i32, y: i32, r: u32) -> (i32, i32) {
    let f = r as i64 * (W as i64 * 256);
    ((((x as i64 * f) >> 31) + ((W as i64) << 7)) as i32, (((H as i64) << 7) - ((y as i64 * f) >> 31)) as i32)
}

/// Adds one puff as its stretched, textured quad. Some(pixels looked at),
/// or None if it wasn't drawn.
#[allow(clippy::too_many_arguments)]
fn draw_puff(
    buf: &mut [u32],
    rows: &mut [u8],
    tex: &Texture,
    lut: &mut [u32; 64],
    p: &mut Puff,
    this: i64,
    cm: i64,
    anim: u8,
) -> Option<u32> {
    let (z, oz) = (p.pos[2], p.old[2]);
    let last = p.shown.take();
    // Behind the near plane (25 units): not drawn, but still alive.
    if z < 25 * 16 || oz < 25 * 16 {
        return None;
    }
    let rz = (1u32 << 31) / z as u32;
    let (sx, sy) = screen(p.pos[0], p.pos[1], rz);
    p.shown = Some((sx, sy, rz));
    if !(-(50 << 8)..=(W + 50) << 8).contains(&sx) || !(-(50 << 8)..=(H + 50) << 8).contains(&sy) {
        return None;
    }
    // Where it was: as drawn last frame, or projected now. Far-off old
    // places are clamped: the quad is clipped to the screen anyway.
    let (ox, oy, orz) = last.unwrap_or_else(|| {
        let r = (1u32 << 31) / oz as u32;
        let (x, y) = screen(p.old[0], p.old[1], r);
        (x, y, r)
    });
    let lim = 1 << 20;
    let (ox, oy) = (ox.clamp(-lim, lim), oy.clamp(-lim, lim));
    // Half widths, Q8 pixels: this / z, at least one pixel.
    let w = (((this * 16) * rz as i64) >> 31).max(256) as i32;
    let ow = (((this * 16) * orz as i64) >> 31).max(256) as i32;

    p.anim = (p.anim + anim) & 63;
    let (dx, dy) = (sx - ox, sy - oy);
    let d = fast_distance(dx, dy);
    if d == 0 {
        return None; // the original's quad has no area then
    }

    // The colour, as OpenGL clamped it, times the alpha: 0..256 a channel.
    let a = (((p.col[3] as i64 * cm) >> 16).clamp(0, 4096)) as i32;
    let ch = |c: i32| ((((c as i64 * cm) >> 16).clamp(0, 4096) as i32 * a) >> 16) as u32;
    let (wr, wg, wb) = (ch(p.col[0]), ch(p.col[1]), ch(p.col[2]));
    if wr | wg | wb == 0 {
        return None;
    }

    // The direction of motion (by FastDistance, as the original), Q14.
    let rd = (1u32 << 30) / d as u32;
    let ux = (dx as i64 * rd as i64) >> 16;
    let uy = (dy as i64 * rd as i64) >> 16;
    // The front centre: new + d + w*u; the axis runs back 3d + 2w from it.
    let ex = ((w as i64 * ux) >> 14) as i32;
    let ey = ((w as i64 * uy) >> 14) as i32;
    let fx = sx + dx + ex;
    let fy = sy + dy + ey;
    let len = 3 * d + 2 * w;
    let hw2 = w + ow;
    // The corners, for the bounding box (A, B at the front, C, D at the back).
    let eox = ((ow as i64 * ux) >> 14) as i32;
    let eoy = ((ow as i64 * uy) >> 14) as i32;
    let (bx, by) = (ox - dx - ex, oy - dy - ey);
    let xs = [fx - ey, fx + ey, bx + eoy, bx - eoy];
    let ys = [fy + ex, fy - ex, by - eox, by + eox];
    let x0 = (*xs.iter().min()? >> 8).max(0);
    let x1 = ((*xs.iter().max()? >> 8) + 1).min(W - 1);
    let y0 = (*ys.iter().min()? >> 8).max(0);
    let y1 = ((*ys.iter().max()? >> 8) + 1).min(H - 1);
    if x1 < x0 || y1 < y0 {
        return None;
    }

    // Texture coordinates, Q16: s from the front (0) to the back (1) along
    // the axis, t from the A side (0) to the B side (1) across it.
    let (rl, rh) = (((1u32 << 30) / len as u32) as i64, ((1u32 << 30) / hw2 as u32) as i64);
    let dsx = -((ux * 1024 * rl) >> 30) as i32;
    let dsy = -((uy * 1024 * rl) >> 30) as i32;
    let dtx = ((uy * 1024 * rh) >> 30) as i32;
    let dty = -((ux * 1024 * rh) >> 30) as i32;
    // At the centre of pixel (x0, y0).
    let rx = ((x0 as i64) << 8) + 128 - fx as i64;
    let ry = ((y0 as i64) << 8) + 128 - fy as i64;
    let s00 = ((rx * dsx as i64 + ry * dsy as i64) >> 8) as i32;
    let t00 = (ONE / 2) + ((rx * dtx as i64 + ry * dty as i64) >> 8) as i32;

    // The mipmap: about as many texels across as pixels (OpenGL's
    // LINEAR_MIPMAP_NEAREST, without the linear).
    let q = (11585 / hw2.max(1)) as u32; // 32 sqrt(2) over the width
    let lv = if q == 0 { 0 } else { (31 - q.leading_zeros()).min(5) as usize };
    // The original's `animFrame && 7` (not & 7): column 1, but 0 for frame 0.
    let frame = (p.anim >> 3) as usize * 8 + (p.anim != 0) as usize;
    let texels = tex.level(frame, lv);
    let sh = 11 + lv as u32;

    // Texel v (0..63) stands for t*t of 4v + 2 (of 255): it adds w (4v + 2) / 256.
    let (mut r, mut g, mut b) = (wr * 2, wg * 2, wb * 2);
    for e in lut[1..].iter_mut() {
        r += wr * 4;
        g += wg * 4;
        b += wb * 4;
        *e = ((r >> 8) << 18) | ((g >> 8) << 9) | (b >> 8);
    }

    // Where along each row 0 <= s, t < 1: from the steps' inverses, a
    // pixel wide either way (each pixel is still checked).
    let inv = |d: i32| if d == 0 { 0 } else { d.signum() as i64 * ((1u32 << 31) / d.unsigned_abs()) as i64 };
    let (isx, itx) = (inv(dsx), inv(dtx));
    let mut touched = 0;
    for y in y0..=y1 {
        let s_row = s00 + (y - y0) * dsy;
        let t_row = t00 + (y - y0) * dty;
        let mut lo = 0i64;
        let mut hi = (x1 - x0) as i64;
        for (v0, dv, iv) in [(s_row, dsx, isx), (t_row, dtx, itx)] {
            if dv == 0 {
                if !(0..ONE).contains(&v0) {
                    hi = -1;
                }
                continue;
            }
            let a = ((-(v0 as i64)) * iv) >> 31;
            let b = ((ONE as i64 - v0 as i64) * iv) >> 31;
            lo = lo.max(a.min(b) - 1);
            hi = hi.min(a.max(b) + 1);
        }
        if hi < lo {
            continue;
        }
        let at = (y * W + x0) as usize;
        let row = &mut buf[at + lo as usize..=at + hi as usize];
        let (s, t) = (s_row + lo as i32 * dsx, t_row + lo as i32 * dtx);
        span(row, texels, lut, s, t, dsx, dtx, sh);
        rows[y as usize] = 2;
        touched += (hi - lo + 1) as u32;
    }
    Some(touched)
}

/// One row of a quad: each pixel inside it adds its texel's light,
/// saturating per channel (as the framebuffer did). Kept apart and small,
/// so the ARM926 can keep it all in registers.
#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn span(row: &mut [u32], tex: &[u8; 1024], lut: &[u32; 64], mut s: i32, mut t: i32, ds: i32, dt: i32, sh: u32) {
    for px in row {
        if (s as u32) < ONE as u32 && (t as u32) < ONE as u32 {
            let i = ((((t as u32) >> sh) << 5) | ((s as u32) >> sh)) as usize;
            let x = *px + lut[(tex[i & 1023] & 63) as usize];
            let of = x & OVER;
            *px = (x | (of - (of >> 8))) ^ of;
        }
        s += ds;
        t += dt;
    }
}

/// Adds two buffer colours, each channel saturating at 255.
#[cfg(test)]
fn add_sat(d: u32, c: u32) -> u32 {
    let x = d + c;
    let of = x & OVER;
    (x | (of - (of >> 8))) ^ of
}

/// A colour in the buffer's layout.
#[cfg(test)]
fn pack(r: u32, g: u32, b: u32) -> u32 {
    (r << 18) | (g << 9) | b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_is_accurate() {
        for k in 0..4096u32 {
            let a = k.wrapping_mul(1 << 20).wrapping_add(k * 977);
            let want = (a as f64 / 4294967296.0 * std::f64::consts::TAU).sin() * 65536.0;
            assert!((sin(a) as f64 - want).abs() < 4.0, "sin({a}) = {} want {want}", sin(a));
            let want = (a as f64 / 4294967296.0 * std::f64::consts::TAU).cos() * 65536.0;
            assert!((cos(a) as f64 - want).abs() < 4.0);
        }
    }

    #[test]
    fn square_roots() {
        for n in [0u64, 1, 2, 3, 4, 15, 16, 17, 1 << 20, 123456789, u32::MAX as u64, 1 << 40, (1 << 62) + 12345] {
            let r = isqrt(n) as u64;
            assert!(r * r <= n && (r + 1) * (r + 1) > n, "isqrt({n}) = {r}");
        }
        assert_eq!(isqrt(u64::MAX), u32::MAX);
    }

    #[test]
    fn saturating_add() {
        assert_eq!(add_sat(pack(255, 128, 1), pack(1, 128, 1)), pack(255, 255, 2));
        assert_eq!(add_sat(pack(128, 127, 0), pack(128, 1, 0)), pack(255, 128, 0));
        for (a, b) in [(200u32, 100u32), (1, 2), (128, 128), (255, 0), (127, 129), (255, 255)] {
            let p = add_sat(pack(a, b, a), pack(b, a, b));
            let s = (a + b).min(255);
            assert_eq!(p, pack(s, s, s));
        }
    }

    #[test]
    fn pull_is_k_over_r_cubed() {
        for r in [9.0f64, 30.0, 100.0, 1000.0, 3000.0, 30000.0] {
            for ks in [0u32, 5, 17] {
                let k = 123_456_789u64 >> 4;
                let r2 = (r * r * 256.0) as u64;
                let want = (k << ks) as f64 * 4096.0 / (r2 as f64).powf(1.5);
                let got = pull(k, ks, r2) as f64;
                assert!((got - want).abs() <= want * 0.002 + 1.0, "r {r} ks {ks}: {got} want {want}");
            }
        }
    }

    #[test]
    fn options() {
        let p = |s: &str| Options::parse(s.split_whitespace());
        assert_eq!(p(""), Some(Options::default()));
        assert_eq!(p("classic").unwrap().specs(1), vec![spec(5, Colour::Tiedye, 100, 256)]);
        let o = p("water streams=2 colour=rainbow speed=0.5 seed=7 later=1").unwrap();
        assert_eq!(o.seed, Some(7));
        assert_eq!(o.specs(0).len(), 9);
        assert!(o.specs(0).iter().all(|s| s.streams == 2 && s.colour == Colour::Rainbow && s.speed == 128));
        assert_eq!(p("random").unwrap().preset, None);
        assert_eq!(p("nonesuch"), None);
        assert_eq!(p("streams=0"), None);
        assert_eq!(p("streams=99"), None);
        assert_eq!(p("colour=plaid"), None);
        assert_eq!(p("classic rgb"), None);
        assert_eq!(p("puffs=10"), None);
        assert_eq!(p("speed=-1"), None);
    }

    /// The fixed-point curves against the original's in f64.
    #[test]
    fn spark_follows_the_original() {
        for (i, t) in [(0u128, 1_000_000i64), (4, 123_456_789), (11, 299_000_000)] {
            let mut m = Mover::new((1800 * (i + 1)) / 13, 1, FIELD_RANGE, 2 * ONE, 256);
            m.update(t);
            let ts = t as f64 / 1e6;
            let rps = std::f64::consts::TAU * 12.0 / 16384.0;
            let mystery = ((1800 * (i + 1)) / 13) as f64;
            let a = ts * rps;
            let p = std::f64::consts::TAU * mystery / 1800.0;
            let cf = ((7.0 * a).cos() + (3.0 * a).cos() + (13.0 * a).cos()) / 6.0 + 2.0;
            let x = 1000.0 * cf * (11.0 * (p + 3.0 * a)).cos();
            let y = 1000.0 * cf * (12.0 * (p + 4.0 * a)).sin();
            let z = 1000.0 * (23.0 * (p + 12.0 * a)).cos();
            let r = a * 0.501 + 5.01 * mystery / 1800.0;
            let (c, s) = (r.cos(), r.sin());
            let (x1, y1, z1) = (x * c - y * s, y * c + x * s, z);
            let (x2, y2, z2) = (x1 * c - z1 * s, y1, z1 * c + x1 * s);
            let (x3, y3, z3) = (x2, y2 * c - z2 * s, z2 * c + y2 * s + 2000.0);
            let r = a * 2.501 + 85.01 * mystery / 1800.0;
            let (c, s) = (r.cos(), r.sin());
            let want = [x3 * c - y3 * s, y3 * c + x3 * s, z3];
            for (k, (&got, want)) in m.pos.iter().zip(want).enumerate() {
                let got = got as f64 / 16.0;
                assert!((got - want).abs() < 2.0, "spark {i} at {t}: axis {k} {got} want {want}");
            }
        }
    }

    #[test]
    fn texture_is_a_soft_disc() {
        let t = Texture::new(&mut Rng::new(3));
        let l0 = t.level(9, 0);
        // Bright in the middle, dark at the corners.
        assert!(l0[16 * 32 + 16] > 25, "{}", l0[16 * 32 + 16]);
        assert_eq!(l0[0], 0);
        assert!(l0.iter().all(|&v| v < 64));
        let l2 = t.level(9, 2);
        assert!(l2[4 * 32 + 4] > 10 && l2[8] == 0, "an 8x8 level, 32 to a row");
    }

    /// Runs a scene for `secs` at `fps`, returning the buffer's total light.
    fn run(opt: &Options, seed: u32, secs: i64, fps: i64) -> (u64, Vec<u16>) {
        let mut s = Scene::new(opt, seed);
        let mut out = vec![0u16; WU * HU];
        let mut t = 5000;
        for _ in 0..secs * fps {
            s.frame(t, &mut out, false, || 0);
            t += 1000 / fps;
        }
        let light = s.buf.iter().map(|&p| (p >> 18 & 255) + (p >> 9 & 255) + (p & 255)).map(u64::from).sum();
        (light, out)
    }

    #[test]
    fn it_draws_and_is_repeatable() {
        let opt = Options { seed: Some(42), ..Options::default() };
        let (light, out) = run(&opt, 0, 6, 25);
        assert!(light > 10_000, "too dark: {light}");
        assert!(out.iter().filter(|&&p| p != 0).count() > 500);
        assert_eq!(run(&opt, 99, 6, 25).1, out, "a fixed seed gives the same frames");
    }

    #[test]
    fn every_preset_and_extremes_run() {
        for p in 0..PRESETS.len() {
            run(&Options { preset: Some(p), seed: Some(p as u32), ..Options::default() }, 0, 3, 25);
        }
        let wild = Options {
            streams: Some(MAX_STREAMS),
            thickness: Some(20000),
            speed: Some(1024),
            bright: Some(1024),
            puffs: 64,
            ..Options::default()
        };
        run(&wild, 5, 4, 25);
        run(&Options { thickness: Some(1), speed: Some(0), ..Options::default() }, 6, 3, 25);
        // A stalled clock and a big jump.
        let mut s = Scene::new(&Options::default(), 1);
        let mut out = vec![0u16; WU * HU];
        for t in [0, 0, 0, 40, 10_000, 10_040, 9_000, 9_040] {
            s.frame(t, &mut out, true, || 0);
        }
    }

    #[test]
    fn rows_fade_to_black_and_stop() {
        let mut s = Scene::new(&Options { seed: Some(1), ..Options::default() }, 0);
        let mut out = vec![0u16; WU * HU];
        for t in 0..100 {
            s.frame(t * 40, &mut out, false, || 0);
        }
        // Stop the flurry; everything fades and the rows go quiet.
        s.flurries.clear();
        for t in 100..200 {
            s.frame(t * 40, &mut out, false, || 0);
        }
        assert!(s.buf.iter().all(|&p| p == 0));
        assert!(out.iter().all(|&p| p == 0));
        assert_eq!(s.pass(&mut out, false), None);
    }
}
