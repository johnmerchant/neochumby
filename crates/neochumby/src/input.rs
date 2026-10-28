//! The Chumby Classic's inputs: the resistive touchscreen (TSC2100, evdev on
//! /dev/input/event0), the squeeze switch (/dev/switch, one byte) and the
//! accelerometer (/dev/accel, 56-byte records). Neither Chumby driver
//! supports poll(), so everything is read non-blocking, each tick.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::RawFd;
use std::os::unix::fs::OpenOptionsExt;

use neochumby_sys::fd;

use crate::fb;

/// The variable holding the top button's descriptor, which the session
/// hands to an app whose manifest says `button = true`.
pub const BUTTON_ENV: &str = "NEOCHUMBY_BUTTON";

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_PRESSURE: u16 = 0x18;
const BTN_TOUCH: u16 = 0x14a;

fn open_nonblock(path: &str) -> Option<File> {
    OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path).ok()
}

/// Raw touchscreen coordinates to screen pixels, from the stock firmware's
/// own calibration in /psp/ts_settings: "x0,xspan,y0,yspan,swap" with a
/// negative span for an inverted axis.
#[derive(Clone, Copy, Debug)]
pub struct Calibration {
    x0: i32,
    xspan: i32,
    y0: i32,
    yspan: i32,
    swap: bool,
}

impl Calibration {
    pub fn load() -> Calibration {
        let fallback = Calibration { x0: 169, xspan: 3733, y0: 4082, yspan: -4054, swap: false };
        let Ok(s) = std::fs::read_to_string("/psp/ts_settings") else { return fallback };
        let v: Vec<i32> = s.trim().split(',').filter_map(|f| f.trim().parse().ok()).collect();
        match v[..] {
            [x0, xspan, y0, yspan, swap] if xspan != 0 && yspan != 0 => {
                Calibration { x0, xspan, y0, yspan, swap: swap != 0 }
            }
            _ => fallback,
        }
    }

    pub fn map(&self, rx: i32, ry: i32) -> (i32, i32) {
        let (rx, ry) = if self.swap { (ry, rx) } else { (rx, ry) };
        let x = (rx - self.x0) * fb::W as i32 / self.xspan;
        let y = (ry - self.y0) * fb::H as i32 / self.yspan;
        (x.clamp(0, fb::W as i32 - 1), y.clamp(0, fb::H as i32 - 1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Touch {
    /// Finger down or moved, in screen pixels, with the raw reading.
    Down {
        x: i32,
        y: i32,
        raw: (i32, i32),
    },
    Up,
}

pub struct Touchscreen {
    file: Option<File>,
    cal: Calibration,
    raw: (i32, i32),
    pressed: bool,
    was_pressed: bool,
}

impl Touchscreen {
    pub fn open() -> Touchscreen {
        Touchscreen {
            file: open_nonblock("/dev/input/event0"),
            cal: Calibration::load(),
            raw: (0, 0),
            pressed: false,
            was_pressed: false,
        }
    }

    /// Every touch report since the last call, in order.
    pub fn poll(&mut self) -> Vec<Touch> {
        let mut out = Vec::new();
        let Some(f) = self.file.as_mut() else { return out };
        let mut buf = [0u8; 16 * 32];
        while let Ok(n) = f.read(&mut buf) {
            if n == 0 {
                break;
            }
            // The 2.6.16 kernel's 32-bit input_event: timeval (2 x u32),
            // type u16, code u16, value i32.
            for e in buf[..n].as_chunks::<16>().0 {
                let ty = u16::from_le_bytes([e[8], e[9]]);
                let code = u16::from_le_bytes([e[10], e[11]]);
                let value = i32::from_le_bytes([e[12], e[13], e[14], e[15]]);
                match (ty, code) {
                    (EV_ABS, ABS_X) => self.raw.0 = value,
                    (EV_ABS, ABS_Y) => self.raw.1 = value,
                    (EV_ABS, ABS_PRESSURE) => self.pressed = value > 0,
                    (EV_KEY, BTN_TOUCH) => self.pressed = value != 0,
                    (EV_SYN, _) => {
                        if self.pressed {
                            let (x, y) = self.cal.map(self.raw.0, self.raw.1);
                            out.push(Touch::Down { x, y, raw: self.raw });
                        } else if self.was_pressed {
                            out.push(Touch::Up);
                        }
                        self.was_pressed = self.pressed;
                    }
                    _ => {}
                }
            }
        }
        out
    }
}

/// The squeeze sensor in the case: true while squeezed.
pub struct Squeeze(Option<File>);

impl Squeeze {
    /// The switch the session handed over ([`BUTTON_ENV`]), else
    /// /dev/switch. Only one process at a time can have /dev/switch open,
    /// and the session keeps it, so an app that wants it says `button =
    /// true` in its manifest.
    pub fn open() -> Squeeze {
        let var = std::env::var(BUTTON_ENV).ok();
        Squeeze(handed_over(var.as_deref()).or_else(|| open_nonblock("/dev/switch")))
    }

    pub fn squeezed(&mut self) -> bool {
        let mut b = [0u8; 1];
        matches!(self.0.as_mut().map(|f| f.read(&mut b)), Some(Ok(1))) && b[0] != 0
    }
}

/// A copy of the switch's descriptor from the session (`var`, the value of
/// [`BUTTON_ENV`]), so that opening it twice doesn't close it twice.
/// Neither is passed on to programs this one runs.
fn handed_over(var: Option<&str>) -> Option<File> {
    let n: RawFd = var?.parse().ok()?;
    fd::handed_over(n).map(File::from)
}

/// One accelerometer record: 14 little-endian u32s, the first a version (3),
/// the second a timestamp, then instantaneous x/y/z, averaged x/y/z and
/// more. About 2048 is zero; 1 g is roughly 830 counts.
pub struct Accel(Option<File>);

impl Accel {
    pub fn open() -> Accel {
        Accel(open_nonblock("/dev/accel"))
    }

    /// The averaged x, y, z, centred on zero.
    pub fn read(&mut self) -> Option<[i32; 3]> {
        let mut b = [0u8; 56];
        let n = self.0.as_mut()?.read(&mut b).ok()?;
        if n < 32 {
            return None;
        }
        let word = |i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]) as i32;
        Some([word(5) - 2048, word(6) - 2048, word(7) - 2048])
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use super::*;

    #[test]
    fn squeeze_takes_the_button_it_was_handed() {
        let switch = File::open("/dev/null").unwrap();
        let var = switch.as_raw_fd().to_string();
        let mut a = Squeeze(handed_over(Some(&var)));
        let b = Squeeze(handed_over(Some(&var)));
        let fd = |s: &Squeeze| s.0.as_ref().map(|f| f.as_raw_fd());
        // Each a copy of it, not /dev/switch (which a host hasn't got).
        assert!(fd(&a).is_some() && fd(&b).is_some() && fd(&a) != fd(&b));
        assert!(!a.squeezed());
        drop(a);
        drop(b);
        // Still open: the copies were closed, not the one handed over.
        assert!(switch.metadata().is_ok());
        assert!(handed_over(Some("x")).is_none() && handed_over(None).is_none());
    }
}
