//! The i.MX21 CSPI SPI masters (CSPI1/2 at 0x1000E000/F000, CSPI3 at
//! 0x10017000). Starting an exchange (XCH) shifts out every word in the
//! transmit FIFO at once, as one chip-select frame, and the replies land
//! in the receive FIFO.

use std::collections::VecDeque;

const CTRL_XCH: u32 = 1 << 9;
const CTRL_ENABLE: u32 = 1 << 10;

const INT_TE: u32 = 1 << 0;
const INT_TH: u32 = 1 << 1;
const INT_TF: u32 = 1 << 2;
const INT_TSHFE: u32 = 1 << 3;
const INT_RR: u32 = 1 << 4;
const INT_RH: u32 = 1 << 5;
const INT_RF: u32 = 1 << 6;
const INT_RO: u32 = 1 << 7;

const FIFO: usize = 8;

/// A device on an SPI bus. A frame is the words of one exchange.
pub trait SpiDevice {
    fn frame(&mut self, words: &[u32], bits: u32) -> Vec<u32>;
}

pub struct Cspi {
    ctrl: u32,
    int_en: u32,
    overflow: bool,
    period: u32,
    dma: u32,
    test: u32,
    tx: VecDeque<u32>,
    rx: VecDeque<u32>,
    pub device: Option<Box<dyn SpiDevice>>,
}

impl Cspi {
    pub fn new(device: Option<Box<dyn SpiDevice>>) -> Cspi {
        Cspi {
            ctrl: 0,
            int_en: 0,
            overflow: false,
            period: 0,
            dma: 0,
            test: 0,
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            device,
        }
    }

    fn status(&self) -> u32 {
        let mut s = INT_TSHFE;
        match self.tx.len() {
            0 => s |= INT_TE | INT_TH,
            n if n <= FIFO / 2 => s |= INT_TH,
            n if n >= FIFO => s |= INT_TF,
            _ => {}
        }
        if !self.rx.is_empty() {
            s |= INT_RR;
        }
        if self.rx.len() >= FIFO / 2 {
            s |= INT_RH;
        }
        if self.rx.len() >= FIFO {
            s |= INT_RF;
        }
        if self.overflow {
            s |= INT_RO;
        }
        s
    }

    pub fn irq(&self) -> bool {
        self.status() & (self.int_en >> 9) != 0
    }

    fn exchange(&mut self) {
        let words: Vec<u32> = self.tx.drain(..).collect();
        let bits = (self.ctrl & 0x1f) + 1;
        let replies = match &mut self.device {
            Some(d) => d.frame(&words, bits),
            None => vec![0; words.len()],
        };
        for r in replies {
            if self.rx.len() < FIFO {
                self.rx.push_back(r);
            } else {
                self.overflow = true;
            }
        }
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x00 => self.rx.pop_front().unwrap_or(0),
            0x08 => self.ctrl,
            0x0c => self.int_en | self.status(),
            // The first word written moves straight into the shift
            // register, so the TX count is one less than the words queued.
            0x10 => self.test | ((self.rx.len() as u32) << 4) | self.tx.len().saturating_sub(1) as u32,
            0x14 => self.period,
            0x18 => self.dma,
            0x1c => 0,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x04 => {
                if self.tx.len() < FIFO {
                    self.tx.push_back(v);
                }
            }
            0x08 => {
                self.ctrl = v & !CTRL_XCH;
                if v & CTRL_XCH != 0 && v & CTRL_ENABLE != 0 {
                    self.exchange();
                }
            }
            0x0c => {
                self.int_en = v & (0x1ff << 9);
                if v & INT_RO != 0 {
                    self.overflow = false;
                }
            }
            0x10 => self.test = v,
            0x14 => self.period = v,
            0x18 => self.dma = v,
            0x1c if v & 1 != 0 => {
                let dev = self.device.take();
                *self = Cspi::new(dev);
            }
            _ => {}
        }
    }
}
