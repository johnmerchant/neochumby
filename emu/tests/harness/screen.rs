//! A screen dump (PPM, as both backends write it) and the checks the
//! scenarios make on it: colours at points and how many pixels in a
//! region pass a test. Checks are by region rather than against reference
//! images, so that only what a scenario means is pinned (a clock on the
//! screen doesn't break it), and no picture of anyone's artwork is kept.

use std::path::Path;

use super::Result;

pub type Rgb = (u8, u8, u8);

pub struct Screen {
    pub w: usize,
    pub h: usize,
    rgb: Vec<u8>,
}

/// The next whitespace-separated header field of a PPM, skipping comments.
fn field<'a>(data: &'a [u8], at: &mut usize) -> Option<&'a [u8]> {
    loop {
        while *at < data.len() && data[*at].is_ascii_whitespace() {
            *at += 1;
        }
        if data.get(*at) == Some(&b'#') {
            while *at < data.len() && data[*at] != b'\n' {
                *at += 1;
            }
            continue;
        }
        break;
    }
    let start = *at;
    while *at < data.len() && !data[*at].is_ascii_whitespace() {
        *at += 1;
    }
    (start < *at).then(|| &data[start..*at])
}

impl Screen {
    pub fn read(path: &Path) -> Result<Screen> {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let bad = || format!("{}: not a binary PPM", path.display());
        if data.get(..2) != Some(b"P6") {
            return Err(bad());
        }
        let mut at = 2;
        let mut next = || -> Result<usize> {
            let f = field(&data, &mut at).ok_or_else(bad)?;
            std::str::from_utf8(f).ok().and_then(|s| s.parse().ok()).ok_or_else(bad)
        };
        let (w, h, max) = (next()?, next()?, next()?);
        if max != 255 {
            return Err(format!("{}: {max}-level PPM, expected 255", path.display()));
        }
        // One whitespace byte, then the pixels.
        let start = at + 1;
        let rgb = data.get(start..start + w * h * 3).ok_or_else(bad)?.to_vec();
        Ok(Screen { w, h, rgb })
    }

    pub fn write_ppm(&self, path: &Path) -> Result<()> {
        let mut out = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        out.extend_from_slice(&self.rgb);
        std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn px(&self, x: usize, y: usize) -> Rgb {
        let i = (y * self.w + x) * 3;
        (self.rgb[i], self.rgb[i + 1], self.rgb[i + 2])
    }

    /// Whether the pixel at (x, y) is within `tol` of `want` in each
    /// channel; the screen is RGB565, so exact colours are rounded.
    pub fn near(&self, x: usize, y: usize, want: Rgb, tol: u8) -> bool {
        let p = self.px(x, y);
        p.0.abs_diff(want.0) <= tol && p.1.abs_diff(want.1) <= tol && p.2.abs_diff(want.2) <= tol
    }

    /// Fails, saying where, unless every point is near `want`.
    pub fn expect(&self, what: &str, points: &[(usize, usize)], want: Rgb) -> std::result::Result<(), String> {
        for &(x, y) in points {
            if !self.near(x, y, want, 16) {
                return Err(format!("{what}: ({x}, {y}) is {:?}, expected about {want:?}", self.px(x, y)));
            }
        }
        Ok(())
    }

    /// How many pixels in the rectangle pass `f`.
    pub fn count(&self, x: usize, y: usize, w: usize, h: usize, f: impl Fn(Rgb) -> bool) -> usize {
        let mut n = 0;
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                n += f(self.px(xx, yy)) as usize;
            }
        }
        n
    }

    /// The fraction of the whole screen that passes `f`.
    pub fn fraction(&self, f: impl Fn(Rgb) -> bool) -> f64 {
        self.count(0, 0, self.w, self.h, f) as f64 / (self.w * self.h) as f64
    }
}
