//! The Thumb instruction set of ARMv5TE.

use crate::arm::shift_reg;
use crate::cpu::add_with_carry;
use crate::machine::{Machine, Width};

impl Machine {
    /// r15 reads as the instruction's address + 4 in Thumb state.
    #[inline]
    fn treg(&self, n: usize) -> u32 {
        if n == 15 { self.cur_pc.wrapping_add(4) } else { self.cpu.r[n] }
    }

    fn t_add(&mut self, a: u32, b: u32, c: bool) -> u32 {
        let (r, c, v) = add_with_carry(a, b, c);
        self.cpu.set_nz(r);
        self.cpu.c = c;
        self.cpu.v = v;
        r
    }

    fn t_load(&mut self, addr: u32, w: Width) -> Option<u32> {
        let user = self.cpu.is_user();
        match self.load(addr, w, user) {
            Ok(v) => Some(v),
            Err(f) => {
                self.exc_data_abort(addr, f);
                None
            }
        }
    }

    fn t_store(&mut self, addr: u32, v: u32, w: Width) -> bool {
        let user = self.cpu.is_user();
        match self.store(addr, v, w, user) {
            Ok(()) => true,
            Err(f) => {
                self.exc_data_abort(addr, f);
                false
            }
        }
    }

    pub fn step_thumb(&mut self) {
        let pc = self.cpu.r[15];
        self.cur_pc = pc;
        let i = match self.fetch(pc, true) {
            Ok(i) => i,
            Err(fsr) => return self.exc_prefetch_abort(fsr),
        };
        if self.trace {
            eprintln!("{:08x}: {:04x}      cpsr={:08x}", pc, i, self.cpu.cpsr());
        }
        self.cpu.r[15] = pc.wrapping_add(2);
        let rd = (i & 7) as usize;
        let rs = ((i >> 3) & 7) as usize;
        match i >> 11 {
            0..=2 => {
                // LSL/LSR/ASR #imm
                let typ = i >> 11;
                let amt = (i >> 6) & 0x1f;
                let (r, c) = crate::arm::shift_imm(typ, self.cpu.r[rs], amt, self.cpu.c);
                self.cpu.r[rd] = r;
                self.cpu.set_nz(r);
                self.cpu.c = c;
            }
            3 => {
                let a = self.cpu.r[rs];
                let b = if i & (1 << 10) != 0 { (i >> 6) & 7 } else { self.cpu.r[((i >> 6) & 7) as usize] };
                self.cpu.r[rd] = if i & (1 << 9) != 0 { self.t_add(a, !b, true) } else { self.t_add(a, b, false) };
            }
            4..=7 => {
                let rd = ((i >> 8) & 7) as usize;
                let imm = i & 0xff;
                match (i >> 11) & 3 {
                    0 => {
                        self.cpu.r[rd] = imm;
                        self.cpu.set_nz(imm);
                    }
                    1 => {
                        self.t_add(self.cpu.r[rd], !imm, true);
                    }
                    2 => self.cpu.r[rd] = self.t_add(self.cpu.r[rd], imm, false),
                    _ => self.cpu.r[rd] = self.t_add(self.cpu.r[rd], !imm, true),
                }
            }
            8 => {
                if i & (1 << 10) == 0 {
                    self.thumb_alu(i)
                } else {
                    self.thumb_hireg(i)
                }
            }
            9 => {
                let rd = ((i >> 8) & 7) as usize;
                let addr = (self.cur_pc.wrapping_add(4) & !3).wrapping_add((i & 0xff) * 4);
                if let Some(v) = self.t_load(addr, Width::W) {
                    self.cpu.r[rd] = v;
                }
            }
            10 | 11 => {
                let ro = ((i >> 6) & 7) as usize;
                let addr = self.cpu.r[rs].wrapping_add(self.cpu.r[ro]);
                let v = self.cpu.r[rd];
                match (i >> 9) & 7 {
                    0 => {
                        self.t_store(addr, v, Width::W);
                    }
                    1 => {
                        self.t_store(addr, v, Width::H);
                    }
                    2 => {
                        self.t_store(addr, v, Width::B);
                    }
                    3 => {
                        if let Some(v) = self.t_load(addr, Width::B) {
                            self.cpu.r[rd] = v as u8 as i8 as i32 as u32;
                        }
                    }
                    4 => {
                        if let Some(v) = self.t_load(addr, Width::W) {
                            self.cpu.r[rd] = v;
                        }
                    }
                    5 => {
                        if let Some(v) = self.t_load(addr, Width::H) {
                            self.cpu.r[rd] = v;
                        }
                    }
                    6 => {
                        if let Some(v) = self.t_load(addr, Width::B) {
                            self.cpu.r[rd] = v;
                        }
                    }
                    _ => {
                        if let Some(v) = self.t_load(addr, Width::H) {
                            self.cpu.r[rd] = v as u16 as i16 as i32 as u32;
                        }
                    }
                }
            }
            12..=17 => {
                let imm = (i >> 6) & 0x1f;
                let base = self.cpu.r[rs];
                let (addr, w) = match i >> 11 {
                    12 | 13 => (base.wrapping_add(imm * 4), Width::W),
                    14 | 15 => (base.wrapping_add(imm), Width::B),
                    _ => (base.wrapping_add(imm * 2), Width::H),
                };
                if i & (1 << 11) != 0 {
                    if let Some(v) = self.t_load(addr, w) {
                        self.cpu.r[rd] = v;
                    }
                } else {
                    self.t_store(addr, self.cpu.r[rd], w);
                }
            }
            18 | 19 => {
                let rd = ((i >> 8) & 7) as usize;
                let addr = self.cpu.r[13].wrapping_add((i & 0xff) * 4);
                if i & (1 << 11) != 0 {
                    if let Some(v) = self.t_load(addr, Width::W) {
                        self.cpu.r[rd] = v;
                    }
                } else {
                    self.t_store(addr, self.cpu.r[rd], Width::W);
                }
            }
            20 => self.cpu.r[((i >> 8) & 7) as usize] = (self.cur_pc.wrapping_add(4) & !3).wrapping_add((i & 0xff) * 4),
            21 => self.cpu.r[((i >> 8) & 7) as usize] = self.cpu.r[13].wrapping_add((i & 0xff) * 4),
            22 | 23 => self.thumb_misc(i),
            24 | 25 => self.thumb_ldmia_stmia(i),
            26 | 27 => {
                let cond = (i >> 8) & 0xf;
                match cond {
                    0xf => self.exc_swi(),
                    0xe => self.exc_undefined(),
                    _ => {
                        if self.cpu.cond(cond) {
                            let off = ((i as u8 as i8 as i32) << 1) as u32;
                            self.cpu.r[15] = self.cur_pc.wrapping_add(4).wrapping_add(off);
                        }
                    }
                }
            }
            28 => {
                let off = ((((i & 0x7ff) << 21) as i32) >> 20) as u32;
                self.cpu.r[15] = self.cur_pc.wrapping_add(4).wrapping_add(off);
            }
            29 => {
                // BLX suffix: to ARM.
                if i & 1 != 0 {
                    return self.exc_undefined();
                }
                let target = self.cpu.r[14].wrapping_add((i & 0x7ff) << 1) & !3;
                self.cpu.r[14] = self.cur_pc.wrapping_add(2) | 1;
                self.cpu.t = false;
                self.cpu.r[15] = target;
            }
            30 => {
                let off = ((((i & 0x7ff) << 21) as i32) >> 9) as u32;
                self.cpu.r[14] = self.cur_pc.wrapping_add(4).wrapping_add(off);
            }
            _ => {
                let target = self.cpu.r[14].wrapping_add((i & 0x7ff) << 1);
                self.cpu.r[14] = self.cur_pc.wrapping_add(2) | 1;
                self.cpu.r[15] = target & !1;
            }
        }
    }

    fn thumb_alu(&mut self, i: u32) {
        let rd = (i & 7) as usize;
        let rs = ((i >> 3) & 7) as usize;
        let a = self.cpu.r[rd];
        let b = self.cpu.r[rs];
        let c = self.cpu.c;
        let logical = |m: &mut Machine, r: u32| {
            m.cpu.set_nz(r);
            r
        };
        match (i >> 6) & 0xf {
            0x0 => self.cpu.r[rd] = logical(self, a & b),
            0x1 => self.cpu.r[rd] = logical(self, a ^ b),
            op @ (0x2 | 0x3 | 0x4 | 0x7) => {
                let typ = match op {
                    0x2 => 0,
                    0x3 => 1,
                    0x4 => 2,
                    _ => 3,
                };
                let (r, c) = shift_reg(typ, a, b & 0xff, c);
                self.cpu.c = c;
                self.cpu.r[rd] = logical(self, r);
            }
            0x5 => self.cpu.r[rd] = self.t_add(a, b, c),
            0x6 => self.cpu.r[rd] = self.t_add(a, !b, c),
            0x8 => {
                logical(self, a & b);
            }
            0x9 => self.cpu.r[rd] = self.t_add(0, !b, true),
            0xa => {
                self.t_add(a, !b, true);
            }
            0xb => {
                self.t_add(a, b, false);
            }
            0xc => self.cpu.r[rd] = logical(self, a | b),
            0xd => self.cpu.r[rd] = logical(self, a.wrapping_mul(b)),
            0xe => self.cpu.r[rd] = logical(self, a & !b),
            _ => self.cpu.r[rd] = logical(self, !b),
        }
    }

    fn thumb_hireg(&mut self, i: u32) {
        let rd = ((i & 7) | ((i >> 4) & 8)) as usize;
        let rm = ((i >> 3) & 0xf) as usize;
        let b = self.treg(rm);
        match (i >> 8) & 3 {
            0 => {
                let r = self.treg(rd).wrapping_add(b);
                if rd == 15 {
                    self.cpu.r[15] = r & !1;
                } else {
                    self.cpu.r[rd] = r;
                }
            }
            1 => {
                self.t_add(self.treg(rd), !b, true);
            }
            2 => {
                if rd == 15 {
                    self.cpu.r[15] = b & !1;
                } else {
                    self.cpu.r[rd] = b;
                }
            }
            _ => {
                if i & 0x80 != 0 {
                    self.cpu.r[14] = self.cur_pc.wrapping_add(2) | 1;
                }
                self.branch_exchange(b);
            }
        }
    }

    fn thumb_misc(&mut self, i: u32) {
        match (i >> 8) & 0xf {
            0x0 => {
                let off = (i & 0x7f) * 4;
                self.cpu.r[13] =
                    if i & 0x80 != 0 { self.cpu.r[13].wrapping_sub(off) } else { self.cpu.r[13].wrapping_add(off) };
            }
            0x4 | 0x5 => {
                // PUSH {list, lr}
                let list = i & 0xff;
                let lr = i & 0x100 != 0;
                let n = list.count_ones() + lr as u32;
                let start = self.cpu.r[13].wrapping_sub(4 * n);
                let user = self.cpu.is_user();
                let mut addr = start;
                for r in 0..8 {
                    if list & (1 << r) != 0 {
                        if let Err(f) = self.store_word_aligned(addr, self.cpu.r[r], user) {
                            return self.exc_data_abort(addr, f);
                        }
                        addr = addr.wrapping_add(4);
                    }
                }
                if lr && let Err(f) = self.store_word_aligned(addr, self.cpu.r[14], user) {
                    return self.exc_data_abort(addr, f);
                }
                self.cpu.r[13] = start;
            }
            0xc | 0xd => {
                // POP {list, pc}
                let list = i & 0xff;
                let pc = i & 0x100 != 0;
                let user = self.cpu.is_user();
                let mut addr = self.cpu.r[13];
                let mut vals = [0u32; 9];
                for r in 0..9 {
                    if (r < 8 && list & (1 << r) != 0) || (r == 8 && pc) {
                        vals[r] = match self.load_word_aligned(addr, user) {
                            Ok(v) => v,
                            Err(f) => return self.exc_data_abort(addr, f),
                        };
                        addr = addr.wrapping_add(4);
                    }
                }
                self.cpu.r[13] = addr;
                for r in 0..8 {
                    if list & (1 << r) != 0 {
                        self.cpu.r[r] = vals[r];
                    }
                }
                if pc {
                    self.branch_exchange(vals[8]);
                }
            }
            0xe => self.exc_prefetch_abort(0x2),
            _ => self.exc_undefined(),
        }
    }

    fn thumb_ldmia_stmia(&mut self, i: u32) {
        let rn = ((i >> 8) & 7) as usize;
        let list = i & 0xff;
        let user = self.cpu.is_user();
        let mut addr = self.cpu.r[rn];
        if i & (1 << 11) != 0 {
            let mut vals = [0u32; 8];
            for r in 0..8 {
                if list & (1 << r) != 0 {
                    vals[r] = match self.load_word_aligned(addr, user) {
                        Ok(v) => v,
                        Err(f) => return self.exc_data_abort(addr, f),
                    };
                    addr = addr.wrapping_add(4);
                }
            }
            self.cpu.r[rn] = addr;
            for r in 0..8 {
                if list & (1 << r) != 0 {
                    self.cpu.r[r] = vals[r];
                }
            }
        } else {
            for r in 0..8 {
                if list & (1 << r) != 0 {
                    if let Err(f) = self.store_word_aligned(addr, self.cpu.r[r], user) {
                        return self.exc_data_abort(addr, f);
                    }
                    addr = addr.wrapping_add(4);
                }
            }
            self.cpu.r[rn] = addr;
        }
    }
}
