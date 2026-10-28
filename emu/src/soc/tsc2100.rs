//! The TI TSC2100 on CSPI3: the touchscreen ADC and audio codec. A frame
//! is a 16-bit command (bit 15 read, bits 14:11 page, 10:5 register)
//! followed by data words, the register advancing with each. Page 0 holds
//! the conversion results, page 1 the touch controls and page 2 the audio
//! controls, which are kept as written.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use super::cspi::SpiDevice;

/// The touch point, in raw ADC units, as the host's pointer sets it.
#[derive(Default, Clone, Copy)]
pub struct Touch {
    pub down: bool,
    pub x: u16,
    pub y: u16,
}

pub type SharedTouch = Arc<Mutex<Touch>>;

const ADC: usize = 64;
const STATUS: usize = 64 + 1;
/// ADC register: the converter is idle (results are ready).
const ADC_ADST: u16 = 1 << 14;
const ADC_PSTCM: u16 = 1 << 15;
const STATUS_DAVAIL: u16 = 1 << 11;

/// The result registers each scan mode (ADC bits 13:10) converts. A
/// result's status bit is STATUS bit (10 - register) for X, Y, Z1, Z2
/// (registers 0-3) and (11 - register) for the rest (5-10).
fn scan_results(mode: u16) -> &'static [usize] {
    match mode {
        1 => &[0, 1],
        2 => &[0, 1, 2, 3],
        3 => &[0],
        4 => &[1],
        5 => &[2, 3],
        6 => &[5],
        7 => &[6],
        8 => &[7],
        9 => &[8],
        10 => &[9],
        11 => &[5, 6, 7, 8],
        12 => &[10],
        _ => &[],
    }
}

fn status_bit(reg: usize) -> u16 {
    if reg < 4 { 1 << (10 - reg) } else { 1 << (11 - reg) }
}

pub struct Tsc2100 {
    regs: [u16; 4 * 64],
    pub touch: SharedTouch,
    /// A conversion has finished and its results are unread.
    dav: bool,
    /// Status bits of results converted and not yet read.
    ready: u16,
}

/// The TSC2100 as the CSPI sees it; the SoC keeps another handle to read
/// its PINTDAV output.
pub struct Port(pub Rc<RefCell<Tsc2100>>);

impl SpiDevice for Port {
    fn frame(&mut self, words: &[u32], bits: u32) -> Vec<u32> {
        self.0.borrow_mut().frame(words, bits)
    }
}

impl Tsc2100 {
    pub fn new(touch: SharedTouch) -> Tsc2100 {
        let mut t = Tsc2100 { regs: [0; 256], touch, dav: false, ready: 0 };
        t.reset();
        t
    }

    /// The PINTDAV output, active low: by the mode in STATUS bits 15:14,
    /// it signals pen down, data available, or either.
    pub fn pintdav_low(&self) -> bool {
        let down = self.touch.lock().unwrap().down;
        match self.regs[STATUS] >> 14 {
            0 => down || self.dav,
            2 => down,
            _ => self.dav,
        }
    }

    fn reset(&mut self) {
        self.regs = [0; 256];
        self.dav = false;
        self.ready = 0;
        // ADC: host-controlled mode, no touch; REF: internal reference.
        self.regs[ADC] = 0x4000;
        self.regs[64 + 3] = 0x0002;
    }

    fn read_reg(&mut self, idx: usize) -> u16 {
        let t = *self.touch.lock().unwrap();
        match idx {
            0 => t.x,
            1 => t.y,
            // Z1/Z2 give a firm press while down.
            2 => {
                if t.down {
                    0x0400
                } else {
                    0
                }
            }
            3 => {
                if t.down {
                    0x0800
                } else {
                    0x0fff
                }
            }
            // Battery and aux inputs: mid-scale; temperature: a room's.
            5..=8 => 0x0800,
            9 | 10 => 0x0700,
            ADC => (self.regs[ADC] & !ADC_PSTCM) | ADC_ADST | if t.down { ADC_PSTCM } else { 0 },
            STATUS => {
                let mut s = (self.regs[STATUS] & 0xc000) | self.ready;
                if self.ready != 0 {
                    s |= STATUS_DAVAIL;
                }
                s
            }
            _ => self.regs[idx],
        }
    }
}

impl Tsc2100 {
    fn frame(&mut self, words: &[u32], _bits: u32) -> Vec<u32> {
        let mut out = vec![0u32; words.len()];
        let Some(&cmd) = words.first() else { return out };
        let read = cmd & 0x8000 != 0;
        let page = ((cmd >> 11) & 0xf) as usize;
        let mut addr = ((cmd >> 5) & 0x3f) as usize;
        for (i, w) in words.iter().enumerate().skip(1) {
            let idx = (page & 3) * 64 + addr;
            if read {
                out[i] = self.read_reg(idx) as u32;
                if idx < 64 {
                    self.ready &= !status_bit(idx);
                    if self.ready == 0 {
                        self.dav = false;
                    }
                }
            } else if idx == 64 + 4 {
                if *w as u16 == 0xbb00 {
                    self.reset();
                }
            } else {
                self.regs[idx] = *w as u16;
                // A scan mode (ADSCM, bits 13:10) in the ADC register
                // starts a conversion, which finishes at once.
                if idx == ADC {
                    let results = scan_results((*w as u16 >> 10) & 0xf);
                    for &r in results {
                        self.ready |= status_bit(r);
                    }
                    if !results.is_empty() {
                        self.dav = true;
                    }
                }
            }
            addr = (addr + 1) & 0x3f;
        }
        out
    }
}
