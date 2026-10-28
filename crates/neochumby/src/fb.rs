//! The Chumby Classic's screen: /dev/fb0, 320x240, RGB565, 640-byte lines.

use std::fs::OpenOptions;
use std::io;

use font8x8::UnicodeFonts;
use neochumby_sys::mm::DeviceMap;

pub const W: usize = 320;
pub const H: usize = 240;

/// The screen, mapped. It is Send, so an app can keep it in a static Mutex.
pub struct Fb {
    map: DeviceMap,
}

pub const fn rgb(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
}

impl Fb {
    pub fn open() -> io::Result<Fb> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
        Ok(Fb { map: DeviceMap::new(file, W * H * 2)? })
    }

    pub fn pixels(&mut self) -> &mut [u16] {
        self.map.as_u16_mut()
    }

    pub fn clear(&mut self, c: u16) {
        self.pixels().fill(c);
    }

    pub fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: u16) {
        let px = self.pixels();
        for row in y.min(H)..(y + h).min(H) {
            px[row * W + x.min(W)..row * W + (x + w).min(W)].fill(c);
        }
    }

    /// 8x8 glyphs, each pixel `scale` pixels square.
    pub fn text(&mut self, x: usize, y: usize, s: &str, scale: usize, c: u16) {
        for (i, ch) in s.chars().enumerate() {
            let Some(glyph) = font8x8::BASIC_FONTS.get(ch) else { continue };
            let gx = x + i * 8 * scale;
            for (gy, bits) in glyph.iter().enumerate() {
                for bit in 0..8 {
                    if bits & (1 << bit) != 0 {
                        self.rect(gx + bit * scale, y + gy * scale, scale, scale, c);
                    }
                }
            }
        }
    }

    pub fn text_width(s: &str, scale: usize) -> usize {
        s.chars().count() * 8 * scale
    }

    /// Copies 8-bit indexed pixels to the screen through `pal`, two pixels
    /// to each 32-bit store. `stride` is the source's bytes per line; what
    /// falls outside the screen is left out.
    pub fn blit_indexed(&mut self, pal: &Palette, src: &[u8], width: usize, height: usize, stride: usize) {
        let height = height.min(H);
        let width = width.min(W) & !1;
        let px = self.map.as_u32_mut();
        for y in 0..height {
            let line = &src[y * stride..y * stride + width];
            let dst = &mut px[y * W / 2..y * W / 2 + width / 2];
            for (d, p) in dst.iter_mut().zip(line.as_chunks::<2>().0) {
                // Little-endian: the left pixel is the low half.
                *d = pal.0[p[0] as usize] as u32 | (pal.0[p[1] as usize] as u32) << 16;
            }
        }
    }
}

/// A 256-colour palette in RGB565, for apps that render indexed pixels.
pub struct Palette([u16; 256]);

impl Palette {
    pub const fn new() -> Palette {
        Palette([0; 256])
    }

    /// Takes up to 256 RGB triples, 8 bits a component.
    pub fn set_rgb(&mut self, rgb: &[u8]) {
        for (i, c) in rgb.as_chunks::<3>().0.iter().take(256).enumerate() {
            self.0[i] = self::rgb(c[0], c[1], c[2]);
        }
    }
}

impl Default for Palette {
    fn default() -> Palette {
        Palette::new()
    }
}
