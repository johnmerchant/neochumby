//! The effects: each draws the frame `t` of the way from one screen to the
//! other, `t` from 0 (all `from`) to [`T1`] (all `to`). Integer arithmetic
//! only, and whole spans copied where they can be: the ARM926 has no FPU,
//! and a frame should take a few milliseconds of its 266 MHz.

pub const W: usize = 320;
pub const H: usize = 240;
/// The whole of `t`.
pub const T1: u32 = 1024;

pub const ALL: &[&str] = &["slide", "iris", "dissolve", "pixelate", "blinds", "melt", "glitch", "crt", "tiles"];

const WHITE: u16 = 0xffff;
const BLACK: u16 = 0;
/// The edge of the iris and the dissolve's sparks.
const GLOW: u16 = rgb(120, 255, 230);

const fn rgb(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
}

/// xorshift32: enough for jitter, seeded from the clock.
pub struct Rng(u32);

impl Rng {
    pub fn seeded() -> Rng {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.subsec_nanos());
        Rng((nanos ^ std::process::id().rotate_left(16)) | 1)
    }

    pub fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    /// 0 to `n` - 1.
    pub fn below(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }
}

/// Smoothstep: slow, fast, slow.
fn ease(t: u32) -> u32 {
    let t = t.min(T1);
    t * t / T1 * (3 * T1 - 2 * t) / T1
}

/// Slow, then faster: falling.
fn ease_in(t: u32) -> u32 {
    let t = t.min(T1);
    t * t / T1
}

/// `t` within [start, start + len) of T1, as 0..=T1.
fn window(t: u32, start: u32, len: u32) -> u32 {
    (t.saturating_sub(start) * T1 / len.max(1)).min(T1)
}

/// `c` moved `k` of T1 of the way towards white.
fn brighten(c: u16, k: u32) -> u16 {
    let (r, g, b) = ((c >> 11) as u32, ((c >> 5) & 63) as u32, (c & 31) as u32);
    let r = r + (31 - r) * k / T1;
    let g = g + (63 - g) * k / T1;
    let b = b + (31 - b) * k / T1;
    ((r << 11) | (g << 5) | b) as u16
}

fn row(px: &[u16], y: usize) -> &[u16] {
    &px[y * W..(y + 1) * W]
}

/// 8x8 ordered dither, 0..64: which pixels a dissolve turns first.
const BAYER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

pub enum Effect {
    /// The new screen pushes the old one off, leftwards; rightwards back to
    /// the app list.
    Slide { back: bool },
    /// A circle opening from the middle, with a glowing rim.
    Iris,
    /// Pixel by pixel in an ordered dither, each spark as it turns.
    Dissolve,
    /// Blocks growing to 16 pixels, then shrinking on the new screen.
    Pixelate,
    /// Venetian blinds, opening top to bottom.
    Blinds,
    /// The old screen melts down the new one, column by column.
    Melt { delay: Vec<u32> },
    /// Torn lines and split colour, worst at the cut.
    Glitch { rng: Rng },
    /// An old television switched off, then on.
    Crt,
    /// Tiles flipping over in a diagonal wave.
    Tiles,
}

impl Effect {
    /// `name`, one of [`ALL`], into the app `to`.
    pub fn new(name: &str, to: &str, rng: &mut Rng) -> Effect {
        match name {
            "slide" => Effect::Slide { back: to == "home" },
            "iris" => Effect::Iris,
            "dissolve" => Effect::Dissolve,
            "pixelate" => Effect::Pixelate,
            "blinds" => Effect::Blinds,
            "melt" => {
                // Two-pixel columns, each starting a little before or after
                // its neighbour.
                let mut delay = Vec::with_capacity(W / 2);
                let mut d = rng.below(T1 / 6) as i32;
                for _ in 0..W / 2 {
                    d = (d + rng.below(41) as i32 - 20).clamp(0, (T1 / 4) as i32);
                    delay.push(d as u32);
                }
                Effect::Melt { delay }
            }
            "glitch" => Effect::Glitch { rng: Rng(rng.next() | 1) },
            "crt" => Effect::Crt,
            _ => Effect::Tiles,
        }
    }

    /// How long it plays.
    pub fn millis(&self) -> u32 {
        match self {
            Effect::Slide { .. } | Effect::Iris => 600,
            Effect::Glitch { .. } => 700,
            Effect::Melt { .. } | Effect::Crt => 1000,
            _ => 800,
        }
    }

    pub fn render(&mut self, t: u32, from: &[u16], to: &[u16], out: &mut [u16]) {
        match self {
            Effect::Slide { back } => slide(ease(t), *back, from, to, out),
            Effect::Iris => iris(ease(t), from, to, out),
            Effect::Dissolve => dissolve(t, from, to, out),
            Effect::Pixelate => pixelate(t, from, to, out),
            Effect::Blinds => blinds(t, from, to, out),
            Effect::Melt { delay } => melt(t, delay, from, to, out),
            Effect::Glitch { rng } => glitch(t, rng, from, to, out),
            Effect::Crt => crt(t, from, to, out),
            Effect::Tiles => tiles(t, from, to, out),
        }
    }
}

fn slide(t: u32, back: bool, from: &[u16], to: &[u16], out: &mut [u16]) {
    let off = (t as usize * W / T1 as usize).min(W);
    for y in 0..H {
        let (f, n, o) = (row(from, y), row(to, y), &mut out[y * W..(y + 1) * W]);
        if back {
            o[..off].copy_from_slice(&n[W - off..]);
            o[off..].copy_from_slice(&f[..W - off]);
        } else {
            o[..W - off].copy_from_slice(&f[off..]);
            o[W - off..].copy_from_slice(&n[..off]);
        }
    }
}

fn iris(t: u32, from: &[u16], to: &[u16], out: &mut [u16]) {
    const RIM: u32 = 4;
    // Past the corners, rim and all, at the end.
    let reach = ((W * W + H * H) / 4).isqrt() as u32 + RIM + 1;
    let r = t * reach / T1;
    out.copy_from_slice(from);
    if t == 0 {
        return;
    }
    let (cx, cy) = (W as i32 / 2, H as i32 / 2);
    for y in 0..H {
        let dy = (y as i32 - cy).unsigned_abs();
        let o = &mut out[y * W..(y + 1) * W];
        let half = |r: u32| (r * r).checked_sub(dy * dy).map(|s| s.isqrt() as i32);
        let span = |h: i32| ((cx - h).max(0) as usize, ((cx + h).min(W as i32)) as usize);
        if let Some(h) = half(r + RIM) {
            let (a, b) = span(h);
            o[a..b].fill(GLOW);
        }
        if let Some(h) = half(r) {
            let (a, b) = span(h);
            o[a..b].copy_from_slice(&row(to, y)[a..b]);
        }
    }
}

fn dissolve(t: u32, from: &[u16], to: &[u16], out: &mut [u16]) {
    // Each of the 64 levels turns over 1/80 of the way, the last by 64/80.
    let level = t * 80 / T1;
    for y in 0..H {
        let b = &BAYER[y & 7];
        for x in 0..W {
            let i = y * W + x;
            let v = b[x & 7] as u32;
            out[i] = if v + 2 < level {
                to[i]
            } else if v < level {
                GLOW
            } else {
                from[i]
            };
        }
    }
}

fn pixelate(t: u32, from: &[u16], to: &[u16], out: &mut [u16]) {
    let half = T1 / 2;
    let (src, k) = if t < half { (from, ease(t * 2)) } else { (to, ease((T1 - t) * 2)) };
    let block = 1 + (k as usize * 15 / T1 as usize);
    for by in (0..H).step_by(block) {
        for bx in (0..W).step_by(block) {
            let c = src[(by + block / 2).min(H - 1) * W + (bx + block / 2).min(W - 1)];
            for y in by..(by + block).min(H) {
                out[y * W + bx..y * W + (bx + block).min(W)].fill(c);
            }
        }
    }
}

fn blinds(t: u32, from: &[u16], to: &[u16], out: &mut [u16]) {
    const BAND: usize = 20;
    let bands = H / BAND;
    // Each opens over half the time, the last starting half way.
    for i in 0..bands {
        let start = i as u32 * (T1 / 2) / (bands as u32 - 1);
        let open = ease(window(t, start, T1 / 2)) as usize * BAND / T1 as usize;
        for j in 0..BAND {
            let y = i * BAND + j;
            // Opening from the middle of the slat outwards.
            let shown = (2 * j as isize + 1 - BAND as isize).unsigned_abs() < open;
            let src = if shown { to } else { from };
            out[y * W..(y + 1) * W].copy_from_slice(row(src, y));
        }
    }
}

fn melt(t: u32, delay: &[u32], from: &[u16], to: &[u16], out: &mut [u16]) {
    let fall = T1 - T1 / 4;
    let drop: Vec<usize> =
        delay.iter().map(|d| (ease_in(window(t, *d, fall)) as usize * H / T1 as usize).min(H)).collect();
    for y in 0..H {
        for (c, d) in drop.iter().enumerate() {
            for x in [c * 2, c * 2 + 1] {
                out[y * W + x] = if y < *d { to[y * W + x] } else { from[(y - d) * W + x] };
            }
        }
    }
}

fn glitch(t: u32, rng: &mut Rng, from: &[u16], to: &[u16], out: &mut [u16]) {
    // Worst at the cut, from one screen to the other, and calm at either
    // end.
    let half = T1 / 2;
    let src = if t < half { from } else { to };
    let g = if t < half { t * 2 } else { (T1 - t) * 2 };
    let split = (g * 8 / T1) as usize;
    let mut y = 0;
    while y < H {
        let band = 1 + rng.below(12) as usize;
        let torn = rng.below(T1) < g;
        let shift = if torn { rng.below(1 + g * 60 / T1) as i32 - (g * 30 / T1) as i32 } else { 0 };
        // Now and then near the cut, a band from the other screen.
        let other = torn && rng.below(T1) < g / 3;
        let s = if other { if t < half { to } else { from } } else { src };
        let noise = torn && rng.below(T1) < g / 6;
        for yy in y..(y + band).min(H) {
            let r = row(s, yy);
            let o = &mut out[yy * W..(yy + 1) * W];
            for (x, p) in o.iter_mut().enumerate() {
                if noise {
                    *p = if rng.below(2) == 0 { BLACK } else { brighten(r[x], T1 / 2) };
                    continue;
                }
                let at = |dx: i32| r[(x as i32 + shift + dx).rem_euclid(W as i32) as usize];
                // Red one way, blue the other.
                *p = (at(split as i32) & 0xf800) | (at(0) & 0x07e0) | (at(-(split as i32)) & 0x001f);
            }
        }
        y += band;
    }
}

fn crt(t: u32, from: &[u16], to: &[u16], out: &mut [u16]) {
    out.fill(BLACK);
    // Off: squashed to a white line, the line to a dot, then dark; on: the
    // line again, opening out.
    let (off_end, dot_end, dark_end) = (T1 * 40 / 100, T1 * 52 / 100, T1 * 60 / 100);
    let (cx, cy) = (W / 2, H / 2);
    if t < off_end {
        let k = ease(window(t, 0, off_end));
        let h = (H - (H - 2) * k as usize / T1 as usize).max(2);
        squash(from, h, W, k, out);
    } else if t < dot_end {
        let k = ease(window(t, off_end, dot_end - off_end));
        let w = (W - (W - 2) * k as usize / T1 as usize).max(2);
        for y in cy - 1..cy + 1 {
            out[y * W + cx - w / 2..y * W + cx + w / 2].fill(WHITE);
        }
    } else if t >= dark_end {
        let k = ease(window(t, dark_end, T1 - dark_end));
        let h = (2 + (H - 2) * k as usize / T1 as usize).min(H);
        squash(to, h, W, T1 - k, out);
    }
}

/// `src` squashed to `h` lines about the middle, `w` wide, brightened by
/// `k`.
fn squash(src: &[u16], h: usize, w: usize, k: u32, out: &mut [u16]) {
    let top = (H - h) / 2;
    for y in 0..h {
        let sy = y * H / h;
        let o = &mut out[(top + y) * W..(top + y + 1) * W];
        for (x, p) in o[(W - w) / 2..(W + w) / 2].iter_mut().enumerate() {
            *p = brighten(src[sy * W + x * W / w], k);
        }
    }
}

fn tiles(t: u32, from: &[u16], to: &[u16], out: &mut [u16]) {
    const TILE: usize = 40;
    let (cols, rows) = (W / TILE, H / TILE);
    let flip = T1 * 2 / 5;
    out.fill(BLACK);
    for ty in 0..rows {
        for tx in 0..cols {
            // Corner to corner, each tile over 2/5 of the time.
            let start = ((tx + ty) as u32) * (T1 - flip) / ((cols + rows - 2) as u32);
            let p = ease(window(t, start, flip));
            // Edge on half way: the old face narrowing, then the new one
            // widening.
            let (src, k) = if p < T1 / 2 { (from, T1 - p * 2) } else { (to, p * 2 - T1) };
            let w = (TILE * k as usize / T1 as usize).max(if k > 0 { 1 } else { 0 });
            let (x0, y0) = (tx * TILE + (TILE - w) / 2, ty * TILE);
            for y in y0..y0 + TILE {
                for (i, p) in out[y * W + x0..y * W + x0 + w].iter_mut().enumerate() {
                    // The face, squeezed into its width.
                    *p = src[y * W + tx * TILE + i * TILE / w.max(1)];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_effect_starts_on_one_screen_and_ends_on_the_other() {
        let from: Vec<u16> = (0..W * H).map(|i| (i * 7) as u16 | 1).collect();
        let to: Vec<u16> = (0..W * H).map(|i| (i * 13) as u16 & !1).collect();
        let mut rng = Rng(12345);
        for name in ALL {
            for dest in ["doom", "home"] {
                let mut fx = Effect::new(name, dest, &mut rng);
                let mut out = vec![0; W * H];
                fx.render(T1, &from, &to, &mut out);
                assert!(out == to, "{name} into {dest} ends on the new screen");
                for t in (0..T1).step_by(37) {
                    fx.render(t, &from, &to, &mut out);
                }
                fx.render(0, &from, &to, &mut out);
                assert!(out == from, "{name} into {dest} starts on the old screen");
            }
        }
    }
}
