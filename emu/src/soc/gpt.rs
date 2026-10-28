//! The i.MX21 general-purpose timers (GPT1-3, at 0x10003000/4000/5000):
//! a 32-bit up-counter with a prescaler, one compare and one capture
//! register, in restart or free-running mode.

use crate::machine::CPU_HZ;

const TCTL_TEN: u32 = 1 << 0;
const TCTL_COMPEN: u32 = 1 << 4;
const TCTL_CAPTEN: u32 = 1 << 5;
const TCTL_FRR: u32 = 1 << 8;
const TCTL_SWR: u32 = 1 << 15;
const TSTAT_COMP: u32 = 1 << 0;
const TSTAT_CAPT: u32 = 1 << 1;

#[derive(Debug)]
pub struct Gpt {
    tctl: u32,
    tprer: u32,
    tcmp: u32,
    tcr: u32,
    tstat: u32,
    count: u32,
    /// Cycle time the count was last brought up to date.
    synced: u64,
    /// Sub-tick remainder, in units of CPU cycles * timer input Hz.
    frac: u128,
}

impl Gpt {
    pub fn new() -> Gpt {
        Gpt { tctl: 0, tprer: 0, tcmp: 0xffff_ffff, tcr: 0, tstat: 0, count: 0, synced: 0, frac: 0 }
    }

    /// The counter's input clock in Hz, before the prescaler.
    fn input_hz(&self, perclk1: u64) -> u64 {
        if self.tctl & TCTL_TEN == 0 {
            return 0;
        }
        match (self.tctl >> 1) & 7 {
            1 => perclk1,
            2 => perclk1 / 4,
            4 => 32_768,
            _ => 0,
        }
    }

    fn divisor(&self) -> u128 {
        CPU_HZ as u128 * ((self.tprer & 0x7ff) as u128 + 1)
    }

    fn period(&self) -> u64 {
        if self.tctl & TCTL_FRR != 0 { 1 << 32 } else { self.tcmp as u64 + 1 }
    }

    /// Advances the counter to `now`, latching compare events.
    pub fn sync(&mut self, now: u64, perclk1: u64) {
        let hz = self.input_hz(perclk1);
        let elapsed = now.saturating_sub(self.synced);
        self.synced = now;
        if hz == 0 {
            self.frac = 0;
            return;
        }
        let total = self.frac + elapsed as u128 * hz as u128;
        let div = self.divisor();
        let mut ticks = (total / div) as u64;
        self.frac = total % div;
        let restart = self.tctl & TCTL_FRR == 0;
        while ticks > 0 {
            if restart && self.count == self.tcmp {
                // Restart mode: the tick after the compare returns to 0.
                self.count = 0;
                ticks -= 1;
                continue;
            }
            let to_cmp = self.ticks_to_compare();
            if ticks < to_cmp {
                self.count = self.count.wrapping_add(ticks as u32);
                break;
            }
            ticks -= to_cmp;
            self.count = self.tcmp;
            self.tstat |= TSTAT_COMP;
            // Whole periods past the compare each end on it again.
            ticks %= self.period();
        }
    }

    /// Ticks until the counter next equals the compare value.
    fn ticks_to_compare(&self) -> u64 {
        if self.tctl & TCTL_FRR == 0 && self.count == self.tcmp {
            return self.period();
        }
        match self.tcmp.wrapping_sub(self.count) {
            0 => 1 << 32,
            d => d as u64,
        }
    }

    pub fn irq(&self) -> bool {
        (self.tstat & TSTAT_COMP != 0 && self.tctl & TCTL_COMPEN != 0)
            || (self.tstat & TSTAT_CAPT != 0 && self.tctl & TCTL_CAPTEN != 0)
    }

    /// Cycle time of the next compare event, or u64::MAX.
    pub fn next_event(&self, perclk1: u64) -> u64 {
        let hz = self.input_hz(perclk1);
        if hz == 0 || self.tctl & TCTL_COMPEN == 0 {
            return u64::MAX;
        }
        let to_cmp = self.ticks_to_compare();
        let need = (to_cmp as u128 * self.divisor()).saturating_sub(self.frac);
        let cycles = need.div_ceil(hz as u128);
        self.synced.saturating_add(cycles.min(u64::MAX as u128 / 2) as u64)
    }

    pub fn read(&mut self, off: u32, now: u64, perclk1: u64) -> u32 {
        self.sync(now, perclk1);
        match off {
            0x00 => self.tctl,
            0x04 => self.tprer,
            0x08 => self.tcmp,
            0x0c => self.tcr,
            0x10 => self.count,
            0x14 => self.tstat,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32, now: u64, perclk1: u64) {
        self.sync(now, perclk1);
        match off {
            0x00 => {
                if v & TCTL_SWR != 0 {
                    *self = Gpt { synced: now, ..Gpt::new() };
                    return;
                }
                let was_on = self.tctl & TCTL_TEN != 0;
                self.tctl = v;
                if !was_on && v & TCTL_TEN != 0 {
                    // Enabling starts from zero.
                    self.count = 0;
                    self.frac = 0;
                }
            }
            0x04 => self.tprer = v & 0x7ff,
            0x08 => {
                self.tcmp = v;
                // In restart mode a compare write restarts the count.
                if self.tctl & TCTL_FRR == 0 {
                    self.count = 0;
                    self.frac = 0;
                }
            }
            0x14 => {
                // Write 1 to clear. A write of 0 clears too, as on the i.MX1
                // GPT that this block derives from.
                if v == 0 {
                    self.tstat = 0;
                } else {
                    self.tstat &= !v;
                }
            }
            _ => {}
        }
    }
}
