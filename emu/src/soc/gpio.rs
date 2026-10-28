//! The i.MX21 GPIO ports A-F (at 0x10015000, 0x100 apart), sharing one
//! interrupt. Inputs idle high (pulled up) unless the host drives them.

const DDIR: usize = 0x00 / 4;
const DR: usize = 0x1c / 4;
const SSR: u32 = 0x24;
const ICR1: usize = 0x28 / 4;
const IMR: usize = 0x30 / 4;
const ISR: u32 = 0x34;

pub struct Gpio {
    regs: [[u32; 0x44 / 4]; 6],
    isr: [u32; 6],
    /// Levels the outside world drives on the pins.
    pins: [u32; 6],
    pmask: u32,
}

impl Gpio {
    pub fn new() -> Gpio {
        Gpio { regs: [[0; 0x11]; 6], isr: [0; 6], pins: [0xffff_ffff; 6], pmask: 0x3f }
    }

    fn level(&self, port: usize) -> u32 {
        let dir = self.regs[port][DDIR];
        (self.regs[port][DR] & dir) | (self.pins[port] & !dir)
    }

    /// The interrupt sense of a pin: 0 rising, 1 falling, 2 high, 3 low.
    fn sense(&self, port: usize, pin: usize) -> u32 {
        (self.regs[port][ICR1 + pin / 16] >> ((pin % 16) * 2)) & 3
    }

    /// Latches level-sensitive interrupts for the current levels.
    fn latch_levels(&mut self, port: usize) {
        let lv = self.level(port);
        for pin in 0..32 {
            let high = lv & (1 << pin) != 0;
            match self.sense(port, pin) {
                2 if high => self.isr[port] |= 1 << pin,
                3 if !high => self.isr[port] |= 1 << pin,
                _ => {}
            }
        }
    }

    /// Drives an input pin from outside, latching edges.
    pub fn set_pin(&mut self, port: usize, pin: usize, high: bool) {
        let old = self.level(port);
        if high {
            self.pins[port] |= 1 << pin;
        } else {
            self.pins[port] &= !(1 << pin);
        }
        let new = self.level(port);
        let bit = 1 << pin;
        if (old ^ new) & bit != 0 {
            match self.sense(port, pin) {
                0 if new & bit != 0 => self.isr[port] |= bit,
                1 if new & bit == 0 => self.isr[port] |= bit,
                _ => {}
            }
        }
        self.latch_levels(port);
    }

    /// Ports with interrupts unmasked: (port, IMR, ICR1, ICR2, levels).
    pub fn debug(&self) -> String {
        let mut s = String::new();
        for p in 0..6 {
            if self.regs[p][IMR] != 0 {
                s += &format!(
                    " port{}: imr={:08x} icr={:08x}/{:08x} level={:08x}",
                    (b"ABCDEF")[p] as char,
                    self.regs[p][IMR],
                    self.regs[p][ICR1],
                    self.regs[p][ICR1 + 1],
                    self.level(p)
                );
            }
        }
        s
    }

    pub fn irq(&self) -> bool {
        (0..6).any(|p| self.pmask & (1 << p) != 0 && self.isr[p] & self.regs[p][IMR] != 0)
    }

    pub fn read(&self, off: u32) -> u32 {
        if off == 0x600 {
            return self.pmask;
        }
        let port = (off / 0x100) as usize;
        let r = off % 0x100;
        if port >= 6 || r >= 0x44 {
            return 0;
        }
        match r {
            SSR => self.level(port),
            ISR => self.isr[port],
            _ => self.regs[port][(r / 4) as usize],
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        if off == 0x600 {
            self.pmask = v & 0x3f;
            return;
        }
        let port = (off / 0x100) as usize;
        let r = off % 0x100;
        if port >= 6 || r >= 0x44 {
            return;
        }
        match r {
            ISR => {
                self.isr[port] &= !v;
                self.latch_levels(port);
            }
            SSR => {}
            _ => {
                self.regs[port][(r / 4) as usize] = v;
                self.latch_levels(port);
            }
        }
    }
}
