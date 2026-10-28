//! ARM926EJ-S register file: the banked registers of each mode, the CPSR
//! and the SPSRs. Instruction execution lives in `arm.rs` and `thumb.rs`.

pub const MODE_USR: u32 = 0x10;
pub const MODE_FIQ: u32 = 0x11;
pub const MODE_IRQ: u32 = 0x12;
pub const MODE_SVC: u32 = 0x13;
pub const MODE_ABT: u32 = 0x17;
pub const MODE_UND: u32 = 0x1b;
#[allow(dead_code)] // the full set of modes; System mode is entered by software
pub const MODE_SYS: u32 = 0x1f;

/// The bank a mode's r13, r14 and SPSR live in. User and System share one.
fn bank(mode: u32) -> usize {
    match mode {
        MODE_FIQ => 1,
        MODE_IRQ => 2,
        MODE_SVC => 3,
        MODE_ABT => 4,
        MODE_UND => 5,
        _ => 0,
    }
}

pub struct Cpu {
    /// The current mode's view of r0-r15. r15 holds the address of the next
    /// instruction to fetch.
    pub r: [u32; 16],
    pub n: bool,
    pub z: bool,
    pub c: bool,
    pub v: bool,
    pub q: bool,
    pub i: bool,
    pub f: bool,
    pub t: bool,
    pub mode: u32,
    bank_r13: [u32; 6],
    bank_r14: [u32; 6],
    bank_spsr: [u32; 6],
    fiq_r8_12: [u32; 5],
    usr_r8_12: [u32; 5],
}

impl Cpu {
    /// The state at reset: Supervisor mode, ARM state, interrupts masked.
    pub fn new() -> Cpu {
        Cpu {
            r: [0; 16],
            n: false,
            z: false,
            c: false,
            v: false,
            q: false,
            i: true,
            f: true,
            t: false,
            mode: MODE_SVC,
            bank_r13: [0; 6],
            bank_r14: [0; 6],
            bank_spsr: [0; 6],
            fiq_r8_12: [0; 5],
            usr_r8_12: [0; 5],
        }
    }

    pub fn cpsr(&self) -> u32 {
        (self.n as u32) << 31
            | (self.z as u32) << 30
            | (self.c as u32) << 29
            | (self.v as u32) << 28
            | (self.q as u32) << 27
            | (self.i as u32) << 7
            | (self.f as u32) << 6
            | (self.t as u32) << 5
            | self.mode
    }

    /// Writes the CPSR fields selected by `mask` (MSR's field mask, as a
    /// byte mask). Control bits only change in privileged modes.
    pub fn set_cpsr(&mut self, val: u32, mask: u32) {
        if mask & 0xff00_0000 != 0 {
            self.n = val & (1 << 31) != 0;
            self.z = val & (1 << 30) != 0;
            self.c = val & (1 << 29) != 0;
            self.v = val & (1 << 28) != 0;
            self.q = val & (1 << 27) != 0;
        }
        if mask & 0xff != 0 && self.mode != MODE_USR {
            self.i = val & (1 << 7) != 0;
            self.f = val & (1 << 6) != 0;
            self.t = val & (1 << 5) != 0;
            self.switch_mode(val & 0x1f);
        }
    }

    /// The SPSR of the current mode; User and System have none and read the
    /// CPSR.
    pub fn spsr(&self) -> u32 {
        match bank(self.mode) {
            0 => self.cpsr(),
            b => self.bank_spsr[b],
        }
    }

    pub fn set_spsr(&mut self, val: u32, mask: u32) {
        let b = bank(self.mode);
        if b != 0 {
            self.bank_spsr[b] = (self.bank_spsr[b] & !mask) | (val & mask);
        }
    }

    pub fn has_spsr(&self) -> bool {
        bank(self.mode) != 0
    }

    /// Changes mode, swapping the banked registers.
    pub fn switch_mode(&mut self, new: u32) {
        let old = self.mode;
        if old == new {
            return;
        }
        let (ob, nb) = (bank(old), bank(new));
        if ob != nb {
            self.bank_r13[ob] = self.r[13];
            self.bank_r14[ob] = self.r[14];
            self.r[13] = self.bank_r13[nb];
            self.r[14] = self.bank_r14[nb];
        }
        if (old == MODE_FIQ) != (new == MODE_FIQ) {
            let (save, load) = if new == MODE_FIQ {
                (&mut self.usr_r8_12, &self.fiq_r8_12)
            } else {
                (&mut self.fiq_r8_12, &self.usr_r8_12)
            };
            save.copy_from_slice(&self.r[8..13]);
            self.r[8..13].copy_from_slice(load);
        }
        self.mode = new;
    }

    /// Enters an exception: banks the CPSR into the new mode's SPSR, sets
    /// LR and masks interrupts. The caller sets the PC.
    pub fn enter_exception(&mut self, mode: u32, lr: u32, mask_fiq: bool) {
        let cpsr = self.cpsr();
        self.switch_mode(mode);
        self.bank_spsr[bank(mode)] = cpsr;
        self.r[14] = lr;
        self.t = false;
        self.i = true;
        if mask_fiq {
            self.f = true;
        }
    }

    /// Reads a User-mode register from any mode (LDM/STM with the S bit).
    pub fn user_reg(&self, n: usize) -> u32 {
        match n {
            8..=12 if self.mode == MODE_FIQ => self.usr_r8_12[n - 8],
            13 | 14 if bank(self.mode) != 0 => {
                if n == 13 {
                    self.bank_r13[0]
                } else {
                    self.bank_r14[0]
                }
            }
            _ => self.r[n],
        }
    }

    pub fn set_user_reg(&mut self, n: usize, val: u32) {
        match n {
            8..=12 if self.mode == MODE_FIQ => self.usr_r8_12[n - 8] = val,
            13 if bank(self.mode) != 0 => self.bank_r13[0] = val,
            14 if bank(self.mode) != 0 => self.bank_r14[0] = val,
            _ => self.r[n] = val,
        }
    }

    pub fn cond(&self, cond: u32) -> bool {
        match cond {
            0x0 => self.z,
            0x1 => !self.z,
            0x2 => self.c,
            0x3 => !self.c,
            0x4 => self.n,
            0x5 => !self.n,
            0x6 => self.v,
            0x7 => !self.v,
            0x8 => self.c && !self.z,
            0x9 => !self.c || self.z,
            0xa => self.n == self.v,
            0xb => self.n != self.v,
            0xc => !self.z && self.n == self.v,
            0xd => self.z || self.n != self.v,
            _ => true,
        }
    }

    pub fn set_nz(&mut self, val: u32) {
        self.n = val & 0x8000_0000 != 0;
        self.z = val == 0;
    }

    pub fn is_user(&self) -> bool {
        self.mode == MODE_USR
    }
}

/// a + b + carry, with the carry and overflow out.
pub fn add_with_carry(a: u32, b: u32, carry: bool) -> (u32, bool, bool) {
    let sum = a as u64 + b as u64 + carry as u64;
    let res = sum as u32;
    let c = sum > 0xffff_ffff;
    let v = ((a ^ res) & (b ^ res)) & 0x8000_0000 != 0;
    (res, c, v)
}
