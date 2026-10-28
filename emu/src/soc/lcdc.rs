//! The i.MX21 LCD controller (at 0x10021000): a background plane and a
//! graphic window overlaid on it with alpha and colour key, both RGB565
//! from RAM. The registers are kept as written; `render` composes a frame.

use crate::machine::RAM_BASE;

const LSSAR: usize = 0x00 / 4;
const LSR: usize = 0x04 / 4;
const LVPWR: usize = 0x08 / 4;
const LPCR: usize = 0x18 / 4;
const LGWSAR: usize = 0x50 / 4;
const LGWSR: usize = 0x54 / 4;
const LGWVPWR: usize = 0x58 / 4;
const LGWPOR: usize = 0x5c / 4;
const LGWPR: usize = 0x60 / 4;
const LGWCR: usize = 0x64 / 4;
const LISR: u32 = 0x40;

pub struct Lcdc {
    regs: [u32; 0x80 / 4],
    palette: [u32; 256],
}

/// Reads an RGB565 pixel from RAM as 0x00RRGGBB.
fn pixel(ram: &[u8], pa: u32) -> u32 {
    let off = pa.wrapping_sub(RAM_BASE) as usize;
    if off + 1 >= ram.len() {
        return 0;
    }
    let p = u16::from_le_bytes([ram[off], ram[off + 1]]) as u32;
    let r = (p >> 11) & 0x1f;
    let g = (p >> 5) & 0x3f;
    let b = p & 0x1f;
    (r << 19 | (r >> 2) << 16) | (g << 10 | (g >> 4) << 8) | (b << 3 | b >> 2)
}

impl Lcdc {
    pub fn new() -> Lcdc {
        Lcdc { regs: [0; 0x20], palette: [0; 256] }
    }

    pub fn read(&self, off: u32) -> u32 {
        match off {
            // Every frame has ended: the end-of-frame and graphic window
            // end-of-frame flags read as set.
            LISR => 0x0000_0001 | 0x0000_0020,
            0x800..=0xbff => self.palette[((off - 0x800) / 4) as usize],
            _ => self.regs.get((off / 4) as usize).copied().unwrap_or(0),
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x800..=0xbff => self.palette[((off - 0x800) / 4) as usize] = v,
            _ => {
                if let Some(r) = self.regs.get_mut((off / 4) as usize) {
                    *r = v;
                }
            }
        }
    }

    /// The panel size: (width, height).
    pub fn size(&self) -> (usize, usize) {
        let w = ((self.regs[LSR] >> 20) & 0x3f) as usize * 16;
        let h = (self.regs[LSR] & 0x3ff) as usize;
        if w == 0 || h == 0 { (320, 240) } else { (w, h) }
    }

    /// The controller is set up (a frame base and 16 bpp).
    pub fn active(&self) -> bool {
        self.regs[LSSAR] != 0 && (self.regs[LPCR] >> 25) & 7 == 5
    }

    /// Composes the visible frame as 0x00RRGGBB pixels.
    pub fn render(&self, ram: &[u8]) -> (usize, usize, Vec<u32>) {
        let (w, h) = self.size();
        let mut out = vec![0u32; w * h];
        if !self.active() {
            return (w, h, out);
        }
        let base = self.regs[LSSAR];
        let stride = (self.regs[LVPWR] & 0x7ff).max(1) * 4;
        for y in 0..h {
            for x in 0..w {
                out[y * w + x] = pixel(ram, base + y as u32 * stride + x as u32 * 2);
            }
        }
        let gwcr = self.regs[LGWCR];
        if gwcr & (1 << 22) != 0 && self.regs[LGWSAR] != 0 {
            let alpha = (gwcr >> 24) & 0xff;
            let key_on = gwcr & (1 << 23) != 0;
            let key = {
                // The key is 6 bits per channel.
                let r = (gwcr >> 12) & 0x3f;
                let g = (gwcr >> 6) & 0x3f;
                let b = gwcr & 0x3f;
                (r << 18) | (g << 10) | (b << 2)
            };
            let gw = ((self.regs[LGWSR] >> 20) & 0x3f) as usize * 16;
            let gh = (self.regs[LGWSR] & 0x3ff) as usize;
            let gx = ((self.regs[LGWPR] >> 16) & 0x3ff) as usize;
            let gy = (self.regs[LGWPR] & 0x3ff) as usize;
            let gstride = (self.regs[LGWVPWR] & 0x7ff).max(1) * 4;
            let goff = self.regs[LGWPOR] & 0x1f;
            for y in 0..gh {
                for x in 0..gw {
                    let (sx, sy) = (gx + x, gy + y);
                    if sx >= w || sy >= h {
                        continue;
                    }
                    let p = pixel(ram, self.regs[LGWSAR] + y as u32 * gstride + (x as u32 + goff) * 2);
                    if key_on && p & 0xfcfcfc == key {
                        continue;
                    }
                    let bg = out[sy * w + sx];
                    let mix = |s: u32| {
                        let a = (p >> s) & 0xff;
                        let b = (bg >> s) & 0xff;
                        ((a * alpha + b * (255 - alpha)) / 255) << s
                    };
                    out[sy * w + sx] = mix(16) | mix(8) | mix(0);
                }
            }
        }
        (w, h, out)
    }
}
