//! The ARM (32-bit) instruction set of ARMv5TE, as the ARM926EJ-S runs it.
//! Jazelle is absent: BXJ behaves as BX.

use crate::cpu::add_with_carry;
use crate::machine::{Machine, Width};

/// Barrel shifter with an immediate amount.
#[inline]
pub fn shift_imm(typ: u32, v: u32, amt: u32, c: bool) -> (u32, bool) {
    match typ {
        0 => {
            if amt == 0 {
                (v, c)
            } else {
                (v << amt, (v >> (32 - amt)) & 1 != 0)
            }
        }
        1 => {
            if amt == 0 {
                (0, v >> 31 != 0)
            } else {
                (v >> amt, (v >> (amt - 1)) & 1 != 0)
            }
        }
        2 => {
            if amt == 0 {
                (((v as i32) >> 31) as u32, v >> 31 != 0)
            } else {
                (((v as i32) >> amt) as u32, (v >> (amt - 1)) & 1 != 0)
            }
        }
        _ => {
            if amt == 0 {
                (((c as u32) << 31) | (v >> 1), v & 1 != 0)
            } else {
                (v.rotate_right(amt), (v >> (amt - 1)) & 1 != 0)
            }
        }
    }
}

/// Barrel shifter with a register amount (the bottom byte of Rs).
#[inline]
pub fn shift_reg(typ: u32, v: u32, amt: u32, c: bool) -> (u32, bool) {
    if amt == 0 {
        return (v, c);
    }
    match typ {
        0 => match amt {
            1..=31 => (v << amt, (v >> (32 - amt)) & 1 != 0),
            32 => (0, v & 1 != 0),
            _ => (0, false),
        },
        1 => match amt {
            1..=31 => (v >> amt, (v >> (amt - 1)) & 1 != 0),
            32 => (0, v >> 31 != 0),
            _ => (0, false),
        },
        2 => {
            if amt < 32 {
                (((v as i32) >> amt) as u32, (v >> (amt - 1)) & 1 != 0)
            } else {
                (((v as i32) >> 31) as u32, v >> 31 != 0)
            }
        }
        _ => {
            let r = amt & 31;
            if r == 0 { (v, v >> 31 != 0) } else { (v.rotate_right(r), (v >> (r - 1)) & 1 != 0) }
        }
    }
}

fn sat_add(a: i32, b: i32, q: &mut bool) -> i32 {
    a.checked_add(b).unwrap_or_else(|| {
        *q = true;
        if b < 0 { i32::MIN } else { i32::MAX }
    })
}

fn sat_sub(a: i32, b: i32, q: &mut bool) -> i32 {
    a.checked_sub(b).unwrap_or_else(|| {
        *q = true;
        if b > 0 { i32::MIN } else { i32::MAX }
    })
}

impl Machine {
    /// Reads a register as an operand: r15 reads as the instruction's
    /// address + 8.
    #[inline]
    fn reg(&self, n: u32) -> u32 {
        if n == 15 { self.cur_pc.wrapping_add(8) } else { self.cpu.r[n as usize] }
    }

    /// A branch from ARM or Thumb: the new PC, keeping the current state.
    #[inline]
    pub fn branch(&mut self, target: u32) {
        self.cpu.r[15] = if self.cpu.t { target & !1 } else { target & !3 };
    }

    /// A branch that may change state from bit 0 (BX, and loads to the PC).
    #[inline]
    pub fn branch_exchange(&mut self, target: u32) {
        self.cpu.t = target & 1 != 0;
        self.branch(target);
    }

    /// Copies SPSR into CPSR (exception return).
    pub fn restore_cpsr(&mut self) {
        if self.cpu.has_spsr() {
            let spsr = self.cpu.spsr();
            self.cpu.set_cpsr(spsr, 0xffff_ffff);
        }
    }

    pub fn step_arm(&mut self) {
        let pc = self.cpu.r[15];
        self.cur_pc = pc;
        let instr = match self.fetch(pc, false) {
            Ok(i) => i,
            Err(fsr) => return self.exc_prefetch_abort(fsr),
        };
        if self.trace {
            eprintln!("{:08x}: {:08x}  cpsr={:08x}", pc, instr, self.cpu.cpsr());
        }
        self.cpu.r[15] = pc.wrapping_add(4);
        let cond = instr >> 28;
        if cond != 0xe {
            if cond == 0xf {
                return self.arm_unconditional(instr);
            }
            if !self.cpu.cond(cond) {
                return;
            }
        }
        match (instr >> 25) & 7 {
            0 => {
                if instr & 0x90 == 0x90 {
                    self.arm_mul_extra(instr)
                } else if instr & 0x0190_0000 == 0x0100_0000 {
                    self.arm_misc(instr)
                } else {
                    self.arm_dp(instr)
                }
            }
            1 => {
                if instr & 0x0190_0000 == 0x0100_0000 {
                    if instr & 0x0020_0000 != 0 {
                        let rot = ((instr >> 8) & 0xf) * 2;
                        self.arm_msr(instr, (instr & 0xff).rotate_right(rot))
                    } else {
                        self.exc_undefined()
                    }
                } else {
                    self.arm_dp(instr)
                }
            }
            2 => self.arm_ldr_str(instr),
            3 => {
                if instr & 0x10 != 0 {
                    self.exc_undefined()
                } else {
                    self.arm_ldr_str(instr)
                }
            }
            4 => self.arm_ldm_stm(instr),
            5 => {
                if instr & (1 << 24) != 0 {
                    self.cpu.r[14] = pc.wrapping_add(4);
                }
                let off = (((instr << 8) as i32) >> 6) as u32;
                self.branch(pc.wrapping_add(8).wrapping_add(off));
            }
            6 => self.exc_undefined(),
            _ => {
                if instr & (1 << 24) != 0 {
                    self.exc_swi()
                } else if instr & 0x10 != 0 {
                    self.arm_mcr_mrc(instr)
                } else {
                    self.exc_undefined()
                }
            }
        }
    }

    fn arm_unconditional(&mut self, instr: u32) {
        if (instr >> 25) & 7 == 5 {
            // BLX <imm>: always to Thumb.
            let pc = self.cur_pc;
            self.cpu.r[14] = pc.wrapping_add(4);
            let off = (((instr << 8) as i32) >> 6) as u32;
            let h = (instr >> 23) & 2;
            self.cpu.t = true;
            self.cpu.r[15] = pc.wrapping_add(8).wrapping_add(off).wrapping_add(h);
        } else if instr & 0x0d70_f000 == 0x0550_f000 {
            // PLD: a hint.
        } else {
            self.exc_undefined()
        }
    }

    #[inline]
    fn shifter_operand(&self, instr: u32) -> (u32, bool) {
        if instr & (1 << 25) != 0 {
            let rot = ((instr >> 8) & 0xf) * 2;
            let v = (instr & 0xff).rotate_right(rot);
            (v, if rot == 0 { self.cpu.c } else { v >> 31 != 0 })
        } else {
            let rm = instr & 0xf;
            let typ = (instr >> 5) & 3;
            if instr & 0x10 == 0 {
                shift_imm(typ, self.reg(rm), (instr >> 7) & 0x1f, self.cpu.c)
            } else {
                let amt = self.cpu.r[((instr >> 8) & 0xf) as usize] & 0xff;
                let v = if rm == 15 { self.cur_pc.wrapping_add(12) } else { self.cpu.r[rm as usize] };
                shift_reg(typ, v, amt, self.cpu.c)
            }
        }
    }

    fn arm_dp(&mut self, instr: u32) {
        let op = (instr >> 21) & 0xf;
        let s = instr & (1 << 20) != 0;
        let rn = (instr >> 16) & 0xf;
        let rd = (instr >> 12) & 0xf;
        let (b, sc) = self.shifter_operand(instr);
        let a = if rn == 15 && instr & 0x0200_0010 == 0x10 { self.cur_pc.wrapping_add(12) } else { self.reg(rn) };
        let c = self.cpu.c;
        // (result, arithmetic carry/overflow if any)
        let (res, arith) = match op {
            0x0 | 0x8 => (a & b, None),
            0x1 | 0x9 => (a ^ b, None),
            0x2 | 0xa => {
                let (r, c, v) = add_with_carry(a, !b, true);
                (r, Some((c, v)))
            }
            0x3 => {
                let (r, c, v) = add_with_carry(b, !a, true);
                (r, Some((c, v)))
            }
            0x4 | 0xb => {
                let (r, c, v) = add_with_carry(a, b, false);
                (r, Some((c, v)))
            }
            0x5 => {
                let (r, c, v) = add_with_carry(a, b, c);
                (r, Some((c, v)))
            }
            0x6 => {
                let (r, c, v) = add_with_carry(a, !b, c);
                (r, Some((c, v)))
            }
            0x7 => {
                let (r, c, v) = add_with_carry(b, !a, c);
                (r, Some((c, v)))
            }
            0xc => (a | b, None),
            0xd => (b, None),
            0xe => (a & !b, None),
            _ => (!b, None),
        };
        let test = (0x8..=0xb).contains(&op);
        if s {
            if rd == 15 && !test {
                self.restore_cpsr();
            } else {
                self.cpu.set_nz(res);
                match arith {
                    Some((c, v)) => {
                        self.cpu.c = c;
                        self.cpu.v = v;
                    }
                    None => self.cpu.c = sc,
                }
            }
        }
        if !test {
            if rd == 15 {
                self.branch(res);
            } else {
                self.cpu.r[rd as usize] = res;
            }
        }
    }

    fn arm_mul_extra(&mut self, instr: u32) {
        if instr & 0x60 != 0 {
            return self.arm_extra_ls(instr);
        }
        let rd = ((instr >> 16) & 0xf) as usize;
        let rn = ((instr >> 12) & 0xf) as usize;
        let rs = ((instr >> 8) & 0xf) as usize;
        let rm = (instr & 0xf) as usize;
        let s = instr & (1 << 20) != 0;
        match (instr >> 23) & 0x1f {
            0 => {
                let mut res = self.cpu.r[rm].wrapping_mul(self.cpu.r[rs]);
                if instr & (1 << 21) != 0 {
                    res = res.wrapping_add(self.cpu.r[rn]);
                }
                self.cpu.r[rd] = res;
                if s {
                    self.cpu.set_nz(res);
                }
            }
            1 => {
                let (a, b) = (self.cpu.r[rm], self.cpu.r[rs]);
                let mut res: u64 = if instr & (1 << 22) != 0 {
                    (a as i32 as i64).wrapping_mul(b as i32 as i64) as u64
                } else {
                    (a as u64) * (b as u64)
                };
                if instr & (1 << 21) != 0 {
                    res = res.wrapping_add(((self.cpu.r[rd] as u64) << 32) | self.cpu.r[rn] as u64);
                }
                self.cpu.r[rn] = res as u32;
                self.cpu.r[rd] = (res >> 32) as u32;
                if s {
                    self.cpu.n = res >> 63 != 0;
                    self.cpu.z = res == 0;
                }
            }
            2 if instr & 0x0030_0f00 == 0 => {
                // SWP / SWPB
                let addr = self.cpu.r[rd];
                let byte = instr & (1 << 22) != 0;
                let user = self.cpu.is_user();
                let w = if byte { Width::B } else { Width::W };
                let old = match self.load(addr, w, user) {
                    Ok(v) => v,
                    Err(f) => return self.exc_data_abort(addr, f),
                };
                if let Err(f) = self.store(addr, self.cpu.r[rm], w, user) {
                    return self.exc_data_abort(addr, f);
                }
                self.cpu.r[rn] = old;
            }
            _ => self.exc_undefined(),
        }
    }

    fn arm_extra_ls(&mut self, instr: u32) {
        let p = instr & (1 << 24) != 0;
        let u = instr & (1 << 23) != 0;
        let w = instr & (1 << 21) != 0;
        let l = instr & (1 << 20) != 0;
        let rn = (instr >> 16) & 0xf;
        let rd = ((instr >> 12) & 0xf) as usize;
        let off = if instr & (1 << 22) != 0 {
            ((instr >> 4) & 0xf0) | (instr & 0xf)
        } else {
            self.cpu.r[(instr & 0xf) as usize]
        };
        let base = self.reg(rn);
        let offset_addr = if u { base.wrapping_add(off) } else { base.wrapping_sub(off) };
        let addr = if p { offset_addr } else { base };
        let wb = !p || w;
        let user = self.cpu.is_user();
        let sh = (instr >> 5) & 3;
        if l {
            let v = match sh {
                1 => self.load(addr, Width::H, user),
                2 => self.load(addr, Width::B, user).map(|v| v as u8 as i8 as i32 as u32),
                _ => self.load(addr, Width::H, user).map(|v| v as u16 as i16 as i32 as u32),
            };
            let v = match v {
                Ok(v) => v,
                Err(f) => return self.exc_data_abort(addr, f),
            };
            if wb {
                self.cpu.r[rn as usize] = offset_addr;
            }
            if rd == 15 {
                self.branch(v);
            } else {
                self.cpu.r[rd] = v;
            }
            return;
        }
        match sh {
            1 => {
                let v = if rd == 15 { self.cur_pc.wrapping_add(12) } else { self.cpu.r[rd] };
                if let Err(f) = self.store(addr, v, Width::H, user) {
                    return self.exc_data_abort(addr, f);
                }
            }
            2 => {
                // LDRD
                if rd & 1 != 0 || rd == 14 {
                    return self.exc_undefined();
                }
                if addr & 3 != 0 && self.cp15.ctrl & crate::mmu::CTRL_A != 0 {
                    return self.exc_data_abort(addr, crate::mmu::FSR_ALIGN);
                }
                let lo = match self.load_word_aligned(addr, user) {
                    Ok(v) => v,
                    Err(f) => return self.exc_data_abort(addr, f),
                };
                let hi = match self.load_word_aligned(addr.wrapping_add(4), user) {
                    Ok(v) => v,
                    Err(f) => return self.exc_data_abort(addr.wrapping_add(4), f),
                };
                if wb {
                    self.cpu.r[rn as usize] = offset_addr;
                }
                self.cpu.r[rd] = lo;
                self.cpu.r[rd + 1] = hi;
                return;
            }
            _ => {
                // STRD
                if rd & 1 != 0 || rd == 14 {
                    return self.exc_undefined();
                }
                if addr & 3 != 0 && self.cp15.ctrl & crate::mmu::CTRL_A != 0 {
                    return self.exc_data_abort(addr, crate::mmu::FSR_ALIGN);
                }
                let (lo, hi) = (self.cpu.r[rd], self.cpu.r[rd + 1]);
                if let Err(f) = self.store_word_aligned(addr, lo, user) {
                    return self.exc_data_abort(addr, f);
                }
                if let Err(f) = self.store_word_aligned(addr.wrapping_add(4), hi, user) {
                    return self.exc_data_abort(addr.wrapping_add(4), f);
                }
            }
        }
        if wb {
            self.cpu.r[rn as usize] = offset_addr;
        }
    }

    fn arm_misc(&mut self, instr: u32) {
        let rm = (instr & 0xf) as usize;
        let rd = ((instr >> 12) & 0xf) as usize;
        let rn = ((instr >> 16) & 0xf) as usize;
        let op = (instr >> 21) & 3;
        match (instr >> 4) & 0xf {
            0 => {
                if op & 1 == 0 {
                    // MRS
                    self.cpu.r[rd] = if instr & (1 << 22) != 0 { self.cpu.spsr() } else { self.cpu.cpsr() };
                } else {
                    self.arm_msr(instr, self.cpu.r[rm])
                }
            }
            1 if op == 1 => self.branch_exchange(self.cpu.r[rm]),
            1 if op == 3 => self.cpu.r[rd] = self.cpu.r[rm].leading_zeros(),
            2 if op == 1 => self.branch_exchange(self.cpu.r[rm]),
            3 if op == 1 => {
                let target = self.cpu.r[rm];
                self.cpu.r[14] = self.cur_pc.wrapping_add(4);
                self.branch_exchange(target);
            }
            5 => {
                let a = self.cpu.r[rm] as i32;
                let b = self.cpu.r[rn] as i32;
                let mut q = self.cpu.q;
                let r = match op {
                    0 => sat_add(a, b, &mut q),
                    1 => sat_sub(a, b, &mut q),
                    2 => {
                        let d = sat_add(b, b, &mut q);
                        sat_add(a, d, &mut q)
                    }
                    _ => {
                        let d = sat_add(b, b, &mut q);
                        sat_sub(a, d, &mut q)
                    }
                };
                self.cpu.q = q;
                self.cpu.r[rd] = r as u32;
            }
            7 if op == 1 => self.exc_prefetch_abort(0x2),
            x if x & 9 == 8 => self.arm_dsp_mul(instr),
            _ => self.exc_undefined(),
        }
    }

    /// SMLAxy, SMLAWy, SMULWy, SMLALxy, SMULxy.
    fn arm_dsp_mul(&mut self, instr: u32) {
        let rd = ((instr >> 16) & 0xf) as usize;
        let rn = ((instr >> 12) & 0xf) as usize;
        let rs = ((instr >> 8) & 0xf) as usize;
        let rm = (instr & 0xf) as usize;
        let x = instr & (1 << 5) != 0;
        let y = instr & (1 << 6) != 0;
        let half = |v: u32, top: bool| -> i32 { if top { (v as i32) >> 16 } else { v as i16 as i32 } };
        match (instr >> 21) & 3 {
            0 => {
                let p = half(self.cpu.r[rm], x) * half(self.cpu.r[rs], y);
                let mut q = self.cpu.q;
                let acc = self.cpu.r[rn] as i32;
                let r = match p.checked_add(acc) {
                    Some(r) => r,
                    None => {
                        q = true;
                        p.wrapping_add(acc)
                    }
                };
                self.cpu.q = q;
                self.cpu.r[rd] = r as u32;
            }
            1 => {
                let p = ((self.cpu.r[rm] as i32 as i64 * half(self.cpu.r[rs], y) as i64) >> 16) as i32;
                if x {
                    self.cpu.r[rd] = p as u32;
                } else {
                    let acc = self.cpu.r[rn] as i32;
                    let r = match p.checked_add(acc) {
                        Some(r) => r,
                        None => {
                            self.cpu.q = true;
                            p.wrapping_add(acc)
                        }
                    };
                    self.cpu.r[rd] = r as u32;
                }
            }
            2 => {
                let p = (half(self.cpu.r[rm], x) * half(self.cpu.r[rs], y)) as i64;
                let acc = (((self.cpu.r[rd] as u64) << 32) | self.cpu.r[rn] as u64) as i64;
                let r = acc.wrapping_add(p) as u64;
                self.cpu.r[rn] = r as u32;
                self.cpu.r[rd] = (r >> 32) as u32;
            }
            _ => {
                self.cpu.r[rd] = (half(self.cpu.r[rm], x) * half(self.cpu.r[rs], y)) as u32;
            }
        }
    }

    fn arm_msr(&mut self, instr: u32, val: u32) {
        let fields = (instr >> 16) & 0xf;
        let mut mask = 0u32;
        for i in 0..4 {
            if fields & (1 << i) != 0 {
                mask |= 0xff << (8 * i);
            }
        }
        if instr & (1 << 22) != 0 {
            self.cpu.set_spsr(val, mask);
        } else {
            self.cpu.set_cpsr(val, mask);
        }
    }

    fn arm_ldr_str(&mut self, instr: u32) {
        let p = instr & (1 << 24) != 0;
        let u = instr & (1 << 23) != 0;
        let byte = instr & (1 << 22) != 0;
        let w = instr & (1 << 21) != 0;
        let l = instr & (1 << 20) != 0;
        let rn = (instr >> 16) & 0xf;
        let rd = ((instr >> 12) & 0xf) as usize;
        let off = if instr & (1 << 25) == 0 {
            instr & 0xfff
        } else {
            shift_imm((instr >> 5) & 3, self.reg(instr & 0xf), (instr >> 7) & 0x1f, self.cpu.c).0
        };
        let base = self.reg(rn);
        let offset_addr = if u { base.wrapping_add(off) } else { base.wrapping_sub(off) };
        let addr = if p { offset_addr } else { base };
        let wb = !p || w;
        let user = self.cpu.is_user() || (!p && w);
        let width = if byte { Width::B } else { Width::W };
        if l {
            let v = match self.load(addr, width, user) {
                Ok(v) => v,
                Err(f) => return self.exc_data_abort(addr, f),
            };
            if wb {
                self.cpu.r[rn as usize] = offset_addr;
            }
            if rd == 15 {
                self.branch_exchange(v);
            } else {
                self.cpu.r[rd] = v;
            }
        } else {
            let v = if rd == 15 { self.cur_pc.wrapping_add(12) } else { self.cpu.r[rd] };
            if let Err(f) = self.store(addr, v, width, user) {
                return self.exc_data_abort(addr, f);
            }
            if wb {
                self.cpu.r[rn as usize] = offset_addr;
            }
        }
    }

    fn arm_ldm_stm(&mut self, instr: u32) {
        let p = instr & (1 << 24) != 0;
        let u = instr & (1 << 23) != 0;
        let s = instr & (1 << 22) != 0;
        let w = instr & (1 << 21) != 0;
        let l = instr & (1 << 20) != 0;
        let rn = ((instr >> 16) & 0xf) as usize;
        let list = instr & 0xffff;
        let n = list.count_ones();
        let base = self.cpu.r[rn];
        let (start, new_base) = match (p, u) {
            (false, true) => (base, base.wrapping_add(4 * n)),
            (true, true) => (base.wrapping_add(4), base.wrapping_add(4 * n)),
            (false, false) => (base.wrapping_sub(4 * n).wrapping_add(4), base.wrapping_sub(4 * n)),
            (true, false) => (base.wrapping_sub(4 * n), base.wrapping_sub(4 * n)),
        };
        let user_access = self.cpu.is_user();
        if start & 3 != 0 && self.cp15.ctrl & crate::mmu::CTRL_A != 0 {
            return self.exc_data_abort(start, crate::mmu::FSR_ALIGN);
        }
        let pc_in_list = list & 0x8000 != 0;
        let user_bank = s && !(l && pc_in_list);
        let mut addr = start;
        if l {
            let mut vals = [0u32; 16];
            for i in 0..16 {
                if list & (1 << i) != 0 {
                    vals[i] = match self.load_word_aligned(addr, user_access) {
                        Ok(v) => v,
                        Err(f) => return self.exc_data_abort(addr, f),
                    };
                    addr = addr.wrapping_add(4);
                }
            }
            if w {
                self.cpu.r[rn] = new_base;
            }
            for i in 0..15 {
                if list & (1 << i) != 0 {
                    if user_bank {
                        self.cpu.set_user_reg(i, vals[i]);
                    } else {
                        self.cpu.r[i] = vals[i];
                    }
                }
            }
            if pc_in_list {
                if s {
                    self.restore_cpsr();
                    self.branch(vals[15]);
                } else {
                    self.branch_exchange(vals[15]);
                }
            }
        } else {
            for i in 0..16 {
                if list & (1 << i) != 0 {
                    let v = if i == 15 {
                        self.cur_pc.wrapping_add(12)
                    } else if user_bank {
                        self.cpu.user_reg(i)
                    } else {
                        self.cpu.r[i]
                    };
                    if let Err(f) = self.store_word_aligned(addr, v, user_access) {
                        return self.exc_data_abort(addr, f);
                    }
                    addr = addr.wrapping_add(4);
                }
            }
            if w {
                self.cpu.r[rn] = new_base;
            }
        }
    }

    fn arm_mcr_mrc(&mut self, instr: u32) {
        let cp = (instr >> 8) & 0xf;
        if cp != 15 || self.cpu.is_user() {
            return self.exc_undefined();
        }
        let opc1 = (instr >> 21) & 7;
        let crn = (instr >> 16) & 0xf;
        let rd = ((instr >> 12) & 0xf) as usize;
        let opc2 = (instr >> 5) & 7;
        let crm = instr & 0xf;
        if instr & (1 << 20) != 0 {
            match self.cp15_read(opc1, crn, crm, opc2) {
                Some(v) => {
                    if rd == 15 {
                        self.cpu.n = v & (1 << 31) != 0;
                        self.cpu.z = v & (1 << 30) != 0;
                        self.cpu.c = v & (1 << 29) != 0;
                        self.cpu.v = v & (1 << 28) != 0;
                    } else {
                        self.cpu.r[rd] = v;
                    }
                }
                None => {
                    eprintln!("[cp15] undefined read c{crn},c{crm},{opc2} at {:08x}", self.cur_pc);
                    self.exc_undefined()
                }
            }
        } else {
            let v = if rd == 15 { self.cur_pc.wrapping_add(12) } else { self.cpu.r[rd] };
            if !self.cp15_write(opc1, crn, crm, opc2, v) {
                eprintln!("[cp15] undefined write c{crn},c{crm},{opc2} at {:08x}", self.cur_pc);
                self.exc_undefined()
            }
        }
    }
}
