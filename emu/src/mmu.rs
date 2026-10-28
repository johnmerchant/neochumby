//! CP15 (the ARM926EJ-S system control coprocessor) and the ARMv5 MMU:
//! sections, coarse and fine page tables, domains and access permissions,
//! with a software TLB in front of the table walk.

use crate::machine::Machine;

/// ARM926EJ-S r0p4, as the Chumby reports in /proc/cpuinfo.
const MIDR: u32 = 0x4106_9264;
/// 16 KB I and D caches, 4-way, 32-byte lines, write-back, format C.
const CACHE_TYPE: u32 = 0x1d15_2152;

pub const CTRL_M: u32 = 1 << 0;
pub const CTRL_A: u32 = 1 << 1;
pub const CTRL_S: u32 = 1 << 8;
pub const CTRL_R: u32 = 1 << 9;
pub const CTRL_V: u32 = 1 << 13;

/// Fault status codes (FSR[3:0]).
pub const FSR_ALIGN: u32 = 0x1;
const FSR_TRANS_SECTION: u32 = 0x5;
const FSR_TRANS_PAGE: u32 = 0x7;
const FSR_DOMAIN_SECTION: u32 = 0x9;
const FSR_DOMAIN_PAGE: u32 = 0xb;
const FSR_PERM_SECTION: u32 = 0xd;
const FSR_PERM_PAGE: u32 = 0xf;

pub struct Cp15 {
    pub ctrl: u32,
    pub ttbr: u32,
    pub dacr: u32,
    pub dfsr: u32,
    pub ifsr: u32,
    pub far: u32,
    pub fcse_pid: u32,
    pub context_id: u32,
    pub dcache_lockdown: u32,
    pub icache_lockdown: u32,
    pub tlb_lockdown: u32,
}

impl Cp15 {
    pub fn new() -> Cp15 {
        Cp15 {
            ctrl: 0x0005_0078,
            ttbr: 0,
            dacr: 0,
            dfsr: 0,
            ifsr: 0,
            far: 0,
            fcse_pid: 0,
            context_id: 0,
            dcache_lockdown: 0,
            icache_lockdown: 0,
            tlb_lockdown: 0,
        }
    }

    pub fn vector_base(&self) -> u32 {
        if self.ctrl & CTRL_V != 0 { 0xffff_0000 } else { 0 }
    }
}

pub const TLB_SIZE: usize = 4096;

/// One cached 4 KB translation. Permissions are kept as the AP-derived
/// client permissions plus the domain, so a DACR change needs no flush.
#[derive(Clone, Copy, Default)]
pub struct TlbEntry {
    /// Virtual page address | 1 when valid.
    tag: u32,
    /// Physical page address.
    pa: u32,
    /// Client permissions: bit 0 privileged read, 1 privileged write,
    /// 2 user read, 3 user write.
    perm: u8,
    domain: u8,
}

/// Access permissions for an AP value, as client-domain permission bits.
fn ap_perms(ap: u32, ctrl: u32) -> u8 {
    match ap {
        0 => match (ctrl & CTRL_S != 0, ctrl & CTRL_R != 0) {
            (true, false) => 0b0001,
            (false, true) => 0b0101,
            _ => 0,
        },
        1 => 0b0011,
        2 => 0b0111,
        _ => 0b1111,
    }
}

#[inline]
fn need_bit(write: bool, user: bool) -> u8 {
    match (write, user) {
        (false, false) => 1,
        (true, false) => 2,
        (false, true) => 4,
        (true, true) => 8,
    }
}

/// Outcome of a table walk.
struct Walk {
    pa: u32,
    domain: u32,
    perm: u8,
    /// True for sections, for the fault status codes.
    section: bool,
    /// The translation covers its whole 4 KB page with one permission and
    /// may be cached.
    cacheable: bool,
}

impl Machine {
    pub fn tlb_flush(&mut self) {
        for e in self.tlb.iter_mut() {
            e.tag = 0;
        }
        self.fetch_page = u32::MAX;
    }

    fn tlb_flush_va(&mut self, va: u32) {
        let idx = (va >> 12) as usize & (TLB_SIZE - 1);
        self.tlb[idx].tag = 0;
        self.fetch_page = u32::MAX;
    }

    /// Translates a virtual address, or returns the fault status (with the
    /// domain in bits 7:4).
    #[inline]
    pub fn translate(&mut self, va: u32, write: bool, user: bool) -> Result<u32, u32> {
        if self.cp15.ctrl & CTRL_M == 0 {
            return Ok(va);
        }
        let va = if va < 0x0200_0000 { va | self.cp15.fcse_pid } else { va };
        let e = self.tlb[(va >> 12) as usize & (TLB_SIZE - 1)];
        if e.tag == (va & !0xfff) | 1 {
            let dom = (self.cp15.dacr >> (2 * e.domain as u32)) & 3;
            if dom == 3 || (dom == 1 && e.perm & need_bit(write, user) != 0) {
                return Ok(e.pa | (va & 0xfff));
            }
        }
        self.translate_slow(va, write, user)
    }

    fn translate_slow(&mut self, va: u32, write: bool, user: bool) -> Result<u32, u32> {
        let w = self.walk(va)?;
        let dom = (self.cp15.dacr >> (2 * w.domain)) & 3;
        let fsr_dom = w.domain << 4;
        match dom {
            3 => {}
            1 => {
                if w.perm & need_bit(write, user) == 0 {
                    return Err(fsr_dom | if w.section { FSR_PERM_SECTION } else { FSR_PERM_PAGE });
                }
            }
            _ => {
                return Err(fsr_dom | if w.section { FSR_DOMAIN_SECTION } else { FSR_DOMAIN_PAGE });
            }
        }
        if w.cacheable {
            let idx = (va >> 12) as usize & (TLB_SIZE - 1);
            self.tlb[idx] =
                TlbEntry { tag: (va & !0xfff) | 1, pa: w.pa & !0xfff, perm: w.perm, domain: w.domain as u8 };
        }
        Ok(w.pa)
    }

    fn walk(&mut self, va: u32) -> Result<Walk, u32> {
        let l1_addr = (self.cp15.ttbr & 0xffff_c000) | ((va >> 20) << 2);
        let l1 = self.phys_read32(l1_addr);
        let domain = (l1 >> 5) & 0xf;
        let ctrl = self.cp15.ctrl;
        match l1 & 3 {
            0 => Err(FSR_TRANS_SECTION),
            2 => Ok(Walk {
                pa: (l1 & 0xfff0_0000) | (va & 0x000f_ffff),
                domain,
                perm: ap_perms((l1 >> 10) & 3, ctrl),
                section: true,
                cacheable: true,
            }),
            kind => {
                let l2_addr = if kind == 1 {
                    (l1 & 0xffff_fc00) | (((va >> 12) & 0xff) << 2)
                } else {
                    (l1 & 0xffff_f000) | (((va >> 10) & 0x3ff) << 2)
                };
                let l2 = self.phys_read32(l2_addr);
                let fault = (domain << 4) | FSR_TRANS_PAGE;
                let (pa, ap, uniform) = match l2 & 3 {
                    0 => return Err(fault),
                    1 => {
                        let sub = (va >> 14) & 3;
                        let aps = (l2 >> 4) & 0xff;
                        ((l2 & 0xffff_0000) | (va & 0xffff), (aps >> (2 * sub)) & 3, aps == (aps & 3) * 0x55)
                    }
                    2 => {
                        let sub = (va >> 10) & 3;
                        let aps = (l2 >> 4) & 0xff;
                        ((l2 & 0xffff_f000) | (va & 0xfff), (aps >> (2 * sub)) & 3, aps == (aps & 3) * 0x55)
                    }
                    _ => {
                        if kind == 1 {
                            return Err(fault);
                        }
                        ((l2 & 0xffff_fc00) | (va & 0x3ff), (l2 >> 4) & 3, false)
                    }
                };
                Ok(Walk { pa, domain, perm: ap_perms(ap, ctrl), section: false, cacheable: uniform })
            }
        }
    }

    /// MRC p15: returns the value for Rd.
    pub fn cp15_read(&mut self, opc1: u32, crn: u32, crm: u32, opc2: u32) -> Option<u32> {
        let _ = opc1;
        Some(match (crn, crm, opc2) {
            (0, 0, 1) => CACHE_TYPE,
            (0, 0, 2) => 0,
            (0, _, _) => MIDR,
            (1, 0, 0) => self.cp15.ctrl,
            (2, 0, 0) => self.cp15.ttbr,
            (3, 0, 0) => self.cp15.dacr,
            (5, 0, 0) => self.cp15.dfsr,
            (5, 0, 1) => self.cp15.ifsr,
            (6, 0, 0) => self.cp15.far,
            // Test-and-clean: every line is always clean, so Z is set.
            (7, 10, 3) | (7, 14, 3) => 1 << 30,
            (7, _, _) => 0,
            (9, 0, 0) => self.cp15.dcache_lockdown,
            (9, 0, 1) => self.cp15.icache_lockdown,
            (9, 1, _) => 0,
            (10, 0, _) => self.cp15.tlb_lockdown,
            (13, 0, 0) => self.cp15.fcse_pid,
            (13, 0, 1) => self.cp15.context_id,
            (15, _, _) => 0,
            _ => return None,
        })
    }

    /// MCR p15. Returns false for an undefined register.
    pub fn cp15_write(&mut self, opc1: u32, crn: u32, crm: u32, opc2: u32, val: u32) -> bool {
        let _ = opc1;
        match (crn, crm, opc2) {
            (1, 0, 0) => {
                let old = self.cp15.ctrl;
                self.cp15.ctrl = (val & 0x0000_f387) | 0x0005_0078;
                if (old ^ self.cp15.ctrl) & (CTRL_M | CTRL_S | CTRL_R) != 0 {
                    self.tlb_flush();
                }
            }
            (2, 0, 0) => {
                self.cp15.ttbr = val & 0xffff_c000;
            }
            (3, 0, 0) => {
                self.cp15.dacr = val;
                self.fetch_page = u32::MAX;
            }
            (5, 0, 0) => self.cp15.dfsr = val,
            (5, 0, 1) => self.cp15.ifsr = val,
            (6, 0, 0) => self.cp15.far = val,
            // Wait for interrupt.
            (7, 0, 4) | (7, 8, 2) => self.wait_for_interrupt(),
            // Cache maintenance: the emulated memory is always coherent.
            (7, _, _) => {}
            (8, _, 0) => self.tlb_flush(),
            (8, _, 1) => self.tlb_flush_va(val),
            (8, _, _) => self.tlb_flush(),
            (9, 0, 0) => self.cp15.dcache_lockdown = val,
            (9, 0, 1) => self.cp15.icache_lockdown = val,
            (9, 1, _) => {}
            (10, 0, _) => self.cp15.tlb_lockdown = val,
            (13, 0, 0) => {
                self.cp15.fcse_pid = val & 0xfe00_0000;
                self.tlb_flush();
            }
            (13, 0, 1) => self.cp15.context_id = val,
            (15, _, _) => {}
            _ => return false,
        }
        true
    }
}
