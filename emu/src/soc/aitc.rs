//! AITC, the i.MX21 interrupt controller (at 0x10040000): 64 sources, each
//! normal (IRQ) or fast (FIQ), with 16 priority levels for normal ones.

pub struct Aitc {
    intcntl: u32,
    nimask: u32,
    enable: u64,
    inttype: u64,
    nipriority: [u32; 8],
    force: u64,
    /// Source levels from the peripherals.
    pub sources: u64,
}

impl Aitc {
    pub fn new() -> Aitc {
        Aitc { intcntl: 0, nimask: 0x1f, enable: 0, inttype: 0, nipriority: [0; 8], force: 0, sources: 0 }
    }

    fn pending(&self) -> u64 {
        (self.sources | self.force) & self.enable
    }

    fn priority(&self, n: u32) -> u32 {
        (self.nipriority[(n / 8) as usize] >> ((n % 8) * 4)) & 0xf
    }

    /// The highest-priority pending normal interrupt and its level; ties go
    /// to the higher number.
    fn normal_vector(&self) -> Option<(u32, u32)> {
        let pend = self.pending() & !self.inttype;
        let mut best: Option<(u32, u32)> = None;
        for n in 0..64 {
            if pend & (1 << n) != 0 {
                let p = self.priority(n);
                // NIMASK masks levels at or below its value; 0x1f masks none.
                if self.nimask < 16 && p <= self.nimask {
                    continue;
                }
                if best.is_none_or(|(_, bp)| p >= bp) {
                    best = Some((n, p));
                }
            }
        }
        best
    }

    pub fn irq(&self) -> bool {
        // NIDIS (INTCNTL bit 22) disables normal interrupts.
        self.intcntl & (1 << 22) == 0 && self.normal_vector().is_some()
    }

    pub fn fiq(&self) -> bool {
        self.intcntl & (1 << 21) == 0 && self.pending() & self.inttype != 0
    }

    pub fn read(&self, off: u32) -> u32 {
        let hi = |v: u64| (v >> 32) as u32;
        let lo = |v: u64| v as u32;
        match off {
            0x00 => self.intcntl,
            0x04 => self.nimask,
            0x10 => hi(self.enable),
            0x14 => lo(self.enable),
            0x18 => hi(self.inttype),
            0x1c => lo(self.inttype),
            0x20..=0x3c => self.nipriority[(7 - (off - 0x20) / 4) as usize],
            0x40 => match self.normal_vector() {
                Some((n, p)) => (n << 16) | p,
                None => 0xffff_0000 | 0xffff,
            },
            0x44 => {
                let pend = self.pending() & self.inttype;
                if pend == 0 { 0xffff_ffff } else { (63 - pend.leading_zeros()) << 16 }
            }
            0x48 => hi(self.sources),
            0x4c => lo(self.sources),
            0x50 => hi(self.force),
            0x54 => lo(self.force),
            0x58 => hi(self.pending() & !self.inttype),
            0x5c => lo(self.pending() & !self.inttype),
            0x60 => hi(self.pending() & self.inttype),
            0x64 => lo(self.pending() & self.inttype),
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        let set_hi = |r: &mut u64, v: u32| *r = (*r & 0xffff_ffff) | ((v as u64) << 32);
        let set_lo = |r: &mut u64, v: u32| *r = (*r & !0xffff_ffff) | v as u64;
        match off {
            0x00 => self.intcntl = v,
            0x04 => self.nimask = v & 0x1f,
            0x08 => self.enable |= 1 << (v & 63),
            0x0c => self.enable &= !(1 << (v & 63)),
            0x10 => set_hi(&mut self.enable, v),
            0x14 => set_lo(&mut self.enable, v),
            0x18 => set_hi(&mut self.inttype, v),
            0x1c => set_lo(&mut self.inttype, v),
            0x20..=0x3c => self.nipriority[(7 - (off - 0x20) / 4) as usize] = v,
            0x50 => set_hi(&mut self.force, v),
            0x54 => set_lo(&mut self.force, v),
            _ => {}
        }
    }
}
