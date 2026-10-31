//! The clock's face, the same whether it is built as WebAssembly (drawn
//! through the runner's host API) or natively (drawn on the framebuffer):
//! the time in big block digits, as tty-clock draws them, with the seconds,
//! and the date under it. Each frame is drawn whole, as tty-clock redraws.

/// Where a face is drawn.
pub trait Canvas {
    /// Fills a rectangle with an RGB565 colour.
    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, c: u16);
    /// Draws `s` in the 8x8 font, each pixel `scale` pixels square.
    fn text(&mut self, x: u32, y: u32, s: &str, scale: u32, c: u16);
}

pub const W: u32 = 320;
pub const H: u32 = 240;

/// RGB565 from 8-bit channels.
pub const fn rgb(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
}

const BG: u16 = rgb(0, 0, 0);
/// tty-clock's -C 4, blue, as CLOCK runs it.
const FG: u16 = rgb(60, 140, 208);
const DIM: u16 = rgb(140, 140, 140);

/// tty-clock's digits: 3 cells wide, 5 high, a bit a cell (top row first,
/// left cell the high bit).
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b001, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// A cell's side, in pixels: HH:MM:SS is 6 digits of 3, 2 colons of 1 and
/// 7 gaps of 1, 27 cells, in 320.
const CELL: u32 = 11;

/// The civil date of `days` since 1970-01-01: (year, month, day).
pub fn civil(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's days_from_civil, inverted.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Draws the face for local time `t`, seconds since the epoch.
pub fn draw(c: &mut impl Canvas, t: i64) {
    let secs = t.rem_euclid(86400);
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    c.fill(0, 0, W, H, BG);
    let top = 70;
    let mut x = (W - 27 * CELL) / 2;
    let digits = [h / 10, h % 10, m / 10, m % 10, s / 10, s % 10];
    for (i, d) in digits.iter().enumerate() {
        if i == 2 || i == 4 {
            // The colon: two cells, one above the middle and one below.
            c.fill(x, top + CELL, CELL, CELL, FG);
            c.fill(x, top + 3 * CELL, CELL, CELL, FG);
            x += 2 * CELL;
        }
        for (row, bits) in DIGITS[*d as usize].iter().enumerate() {
            for col in 0..3 {
                if bits & (0b100 >> col) != 0 {
                    c.fill(x + col * CELL, top + row as u32 * CELL, CELL, CELL, FG);
                }
            }
        }
        x += 4 * CELL;
    }
    let (y, mo, d) = civil(t.div_euclid(86400));
    let date = format!("{y:04}-{mo:02}-{d:02}");
    let dw = date.len() as u32 * 16;
    c.text((W - dw) / 2, top + 6 * CELL + 10, &date, 2, DIM);
}

/// Times frames: every `every` frames, the mean draw time, for the log.
pub struct Meter {
    every: u32,
    frames: u32,
    total_us: u64,
}

impl Meter {
    pub fn new(every: u32) -> Meter {
        Meter { every, frames: 0, total_us: 0 }
    }

    /// Adds a frame that took `us`; a line to log once `every` have.
    pub fn add(&mut self, us: u64) -> Option<String> {
        self.frames += 1;
        self.total_us += us;
        if self.frames < self.every {
            return None;
        }
        let line = format!(
            "draw {:.2} ms/frame over {} frames",
            self.total_us as f64 / 1000.0 / self.frames as f64,
            self.frames
        );
        (self.frames, self.total_us) = (0, 0);
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Count {
        fills: u32,
        texts: Vec<String>,
    }

    impl Canvas for Count {
        fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, _: u16) {
            assert!(x + w <= W && y + h <= H, "({x}, {y}) {w}x{h} off the screen");
            self.fills += 1;
        }
        fn text(&mut self, _: u32, _: u32, s: &str, _: u32, _: u16) {
            self.texts.push(s.to_string());
        }
    }

    #[test]
    fn draws_on_the_screen() {
        let mut c = Count::default();
        // 2026-10-31 23:59:58
        draw(&mut c, 1_793_491_198);
        assert_eq!(c.texts, ["2026-10-31"]);
        // The background, 4 colon cells, and the digits' cells.
        assert!(c.fills > 40, "{}", c.fills);
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(11016), (2000, 2, 29));
        assert_eq!(civil(-1), (1969, 12, 31));
    }

    #[test]
    fn the_meter_says_the_mean() {
        let mut m = Meter::new(2);
        assert_eq!(m.add(1000), None);
        assert_eq!(m.add(3000).as_deref(), Some("draw 2.00 ms/frame over 2 frames"));
        assert_eq!(m.add(500), None);
    }
}
