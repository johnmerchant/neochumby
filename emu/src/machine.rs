//! The whole Chumby: CPU, MMU, RAM and the i.MX21 peripherals, and the
//! loop that runs them.

use crate::cpu::{Cpu, MODE_ABT, MODE_FIQ, MODE_IRQ, MODE_SVC, MODE_UND};
use crate::mmu::{CTRL_A, Cp15, FSR_ALIGN, TLB_SIZE, TlbEntry};
use crate::soc::Soc;

pub const RAM_BASE: u32 = 0xc000_0000;
pub const RAM_SIZE: usize = 64 << 20;

/// The emulated core clock. One instruction takes one cycle.
pub const CPU_HZ: u64 = 266_000_000;

/// Recent PCs kept for diagnostics.
const HISTORY: usize = 64;

pub struct Machine {
    pub cpu: Cpu,
    pub cp15: Cp15,
    pub(crate) tlb: Box<[TlbEntry]>,
    pub ram: Vec<u8>,
    pub soc: Soc,
    /// Cycles since power-on.
    pub clock: u64,
    /// Address of the instruction being executed.
    pub cur_pc: u32,
    /// The virtual page the fetch fast path points at, or u32::MAX.
    pub(crate) fetch_page: u32,
    pub(crate) fetch_base: usize,
    pub(crate) fetch_user: bool,
    irq_line: bool,
    fiq_line: bool,
    /// Instructions retired, for statistics.
    pub icount: u64,
    pub trace: bool,
    pub history_on: bool,
    history: [u32; HISTORY],
    /// Turns tracing on when the PC reaches this address.
    pub trace_at: u32,
    pub stop: bool,
    /// When set, emulated time is held back to wall-clock time from this
    /// starting point (instant, cycle count).
    pub realtime: Option<(std::time::Instant, u64)>,
}

/// Data accesses: their width and whether they are made as User (LDRT).
#[derive(Clone, Copy, PartialEq)]
pub enum Width {
    B,
    H,
    W,
}

impl Machine {
    pub fn new(soc: Soc) -> Machine {
        Machine {
            cpu: Cpu::new(),
            cp15: Cp15::new(),
            tlb: vec![TlbEntry::default(); TLB_SIZE].into_boxed_slice(),
            ram: vec![0; RAM_SIZE],
            soc,
            clock: 0,
            cur_pc: 0,
            fetch_page: u32::MAX,
            fetch_base: 0,
            fetch_user: false,
            irq_line: false,
            fiq_line: false,
            icount: 0,
            trace: false,
            history_on: false,
            history: [0; HISTORY],
            trace_at: u32::MAX,
            stop: false,
            realtime: None,
        }
    }

    // ---- physical memory -------------------------------------------------

    #[inline]
    fn ram_offset(pa: u32, len: u32) -> Option<usize> {
        let off = pa.wrapping_sub(RAM_BASE);
        if off <= RAM_SIZE as u32 - len { Some(off as usize) } else { None }
    }

    pub fn phys_read32(&mut self, pa: u32) -> u32 {
        match Self::ram_offset(pa, 4) {
            Some(o) => u32::from_le_bytes(self.ram[o..o + 4].try_into().unwrap()),
            None => self.mmio_read(pa, Width::W),
        }
    }

    pub fn phys_read(&mut self, pa: u32, w: Width) -> u32 {
        match w {
            Width::W => self.phys_read32(pa),
            Width::H => match Self::ram_offset(pa, 2) {
                Some(o) => u16::from_le_bytes(self.ram[o..o + 2].try_into().unwrap()) as u32,
                None => self.mmio_read(pa, w) & 0xffff,
            },
            Width::B => match Self::ram_offset(pa, 1) {
                Some(o) => self.ram[o] as u32,
                None => self.mmio_read(pa, w) & 0xff,
            },
        }
    }

    pub fn phys_write(&mut self, pa: u32, val: u32, w: Width) {
        match w {
            Width::W => match Self::ram_offset(pa, 4) {
                Some(o) => self.ram[o..o + 4].copy_from_slice(&val.to_le_bytes()),
                None => self.mmio_write(pa, val, w),
            },
            Width::H => match Self::ram_offset(pa, 2) {
                Some(o) => self.ram[o..o + 2].copy_from_slice(&(val as u16).to_le_bytes()),
                None => self.mmio_write(pa, val & 0xffff, w),
            },
            Width::B => match Self::ram_offset(pa, 1) {
                Some(o) => self.ram[o] = val as u8,
                None => self.mmio_write(pa, val & 0xff, w),
            },
        }
    }

    fn mmio_read(&mut self, pa: u32, w: Width) -> u32 {
        let v = self.soc.read(pa, w, self.clock, &mut self.ram);
        self.update_irq();
        v
    }

    fn mmio_write(&mut self, pa: u32, val: u32, w: Width) {
        self.soc.write(pa, val, w, self.clock, &mut self.ram);
        self.update_irq();
    }

    pub fn update_irq(&mut self) {
        let (irq, fiq) = self.soc.interrupt_lines();
        self.irq_line = irq;
        self.fiq_line = fiq;
    }

    // ---- virtual memory --------------------------------------------------

    /// Loads from a virtual address. ARMv5 rules: with alignment checking
    /// off, an unaligned word load rotates the aligned word and an unaligned
    /// halfword reads the aligned halfword.
    #[inline]
    pub fn load(&mut self, va: u32, w: Width, user: bool) -> Result<u32, u32> {
        let mask = match w {
            Width::W => 3,
            Width::H => 1,
            Width::B => 0,
        };
        if va & mask != 0 && self.cp15.ctrl & CTRL_A != 0 {
            return Err(FSR_ALIGN);
        }
        let pa = self.translate(va, false, user)?;
        Ok(match w {
            Width::W => self.phys_read32(pa & !3).rotate_right((pa & 3) * 8),
            Width::H => self.phys_read(pa & !1, Width::H),
            Width::B => self.phys_read(pa, Width::B),
        })
    }

    #[inline]
    pub fn store(&mut self, va: u32, val: u32, w: Width, user: bool) -> Result<(), u32> {
        let mask = match w {
            Width::W => 3,
            Width::H => 1,
            Width::B => 0,
        };
        if va & mask != 0 && self.cp15.ctrl & CTRL_A != 0 {
            return Err(FSR_ALIGN);
        }
        let pa = self.translate(va, true, user)?;
        self.phys_write(pa & !mask, val, w);
        Ok(())
    }

    /// Word access for LDM/STM/LDRD/SWP, which ignore address bits 1:0
    /// unless alignment checking is on.
    #[inline]
    pub fn load_word_aligned(&mut self, va: u32, user: bool) -> Result<u32, u32> {
        let pa = self.translate(va & !3, false, user)?;
        Ok(self.phys_read32(pa))
    }

    #[inline]
    pub fn store_word_aligned(&mut self, va: u32, val: u32, user: bool) -> Result<(), u32> {
        let pa = self.translate(va & !3, true, user)?;
        self.phys_write(pa, val, Width::W);
        Ok(())
    }

    /// Fetches an instruction. A fast path keeps the RAM offset of the
    /// current code page.
    #[inline]
    pub fn fetch(&mut self, va: u32, thumb: bool) -> Result<u32, u32> {
        let user = self.cpu.is_user();
        let page = va & !0xfff;
        if page == self.fetch_page && user == self.fetch_user {
            let o = self.fetch_base + (va & 0xfff) as usize;
            return Ok(if thumb {
                u16::from_le_bytes([self.ram[o], self.ram[o + 1]]) as u32
            } else {
                u32::from_le_bytes(self.ram[o..o + 4].try_into().unwrap())
            });
        }
        let pa = self.translate(va, false, user)?;
        if let Some(base) = Self::ram_offset(pa & !0xfff, 0x1000) {
            self.fetch_page = page;
            self.fetch_base = base;
            self.fetch_user = user;
        }
        Ok(if thumb { self.phys_read(pa, Width::H) } else { self.phys_read32(pa) })
    }

    // ---- exceptions ------------------------------------------------------

    fn vector(&mut self, offset: u32) {
        self.cpu.r[15] = self.cp15.vector_base() + offset;
        self.fetch_page = u32::MAX;
    }

    pub fn exc_undefined(&mut self) {
        let lr = self.cur_pc + if self.cpu.t { 2 } else { 4 };
        self.cpu.enter_exception(MODE_UND, lr, false);
        self.vector(0x04);
    }

    pub fn exc_swi(&mut self) {
        let lr = self.cur_pc + if self.cpu.t { 2 } else { 4 };
        self.cpu.enter_exception(MODE_SVC, lr, false);
        self.vector(0x08);
    }

    pub fn exc_prefetch_abort(&mut self, fsr: u32) {
        self.cp15.ifsr = fsr;
        let lr = self.cur_pc + 4;
        self.cpu.enter_exception(MODE_ABT, lr, false);
        self.vector(0x0c);
    }

    /// A data abort. The base-restored model: the faulting instruction has
    /// changed no registers.
    pub fn exc_data_abort(&mut self, va: u32, fsr: u32) {
        self.cp15.dfsr = fsr;
        self.cp15.far = va;
        let lr = self.cur_pc + 8;
        self.cpu.enter_exception(MODE_ABT, lr, false);
        self.vector(0x10);
    }

    fn exc_irq(&mut self) {
        // r15 is the next instruction; LR_irq is that + 4.
        let lr = self.cpu.r[15] + 4;
        self.cpu.enter_exception(MODE_IRQ, lr, false);
        self.vector(0x18);
    }

    fn exc_fiq(&mut self) {
        let lr = self.cpu.r[15] + 4;
        self.cpu.enter_exception(MODE_FIQ, lr, true);
        self.vector(0x1c);
    }

    /// Sleeps while emulated time is ahead of wall-clock time.
    fn pace(&self) {
        if let Some((start, base)) = self.realtime {
            let emulated = (self.clock - base) as f64 / CPU_HZ as f64;
            let ahead = emulated - start.elapsed().as_secs_f64();
            if ahead > 0.0 {
                std::thread::sleep(std::time::Duration::from_secs_f64(ahead));
            }
        }
    }

    /// WFI: time skips ahead to the next device event unless an interrupt
    /// is already pending.
    pub fn wait_for_interrupt(&mut self) {
        loop {
            self.update_irq();
            if self.irq_line || self.fiq_line || self.stop {
                return;
            }
            let next = self.soc.next_event(self.clock);
            if next > self.clock {
                self.clock = next;
            }
            self.soc.tick(self.clock, &mut self.ram);
            self.pace();
            if self.soc.poll_host(self.clock, &self.ram) {
                // Host input arrived; let devices see it.
                self.soc.tick(self.clock, &mut self.ram);
            }
        }
    }

    // ---- the run loop ----------------------------------------------------

    /// Runs until the clock reaches `target` or something sets `stop`.
    pub fn run_until(&mut self, target: u64) {
        let mut next_poll = self.clock;
        while !self.stop && self.clock < target {
            let mut next_event = self.soc.next_event(self.clock);
            if self.clock >= next_event || self.clock >= next_poll {
                if self.clock >= next_poll {
                    self.pace();
                    self.soc.poll_host(self.clock, &self.ram);
                    next_poll = self.clock + CPU_HZ / 1000;
                }
                self.soc.tick(self.clock, &mut self.ram);
                self.update_irq();
                next_event = self.soc.next_event(self.clock);
            }
            if self.fiq_line && !self.cpu.f {
                self.exc_fiq();
            } else if self.irq_line && !self.cpu.i {
                self.exc_irq();
            }
            // A batch of instructions between event checks, cut short when
            // an interrupt becomes deliverable.
            let batch_end = next_event.min(next_poll).min(self.clock + 4096).min(target).max(self.clock + 1);
            while self.clock < batch_end {
                self.step();
                if (self.irq_line && !self.cpu.i) || (self.fiq_line && !self.cpu.f) || self.stop {
                    break;
                }
            }
        }
    }

    #[inline]
    pub fn step(&mut self) {
        if self.cpu.r[15] == self.trace_at {
            self.trace = true;
        }
        if self.history_on {
            self.history[self.icount as usize % HISTORY] = self.cpu.r[15] | self.cpu.t as u32;
        }
        self.icount += 1;
        self.clock += 1;
        if self.cpu.t {
            self.step_thumb();
        } else {
            self.step_arm();
        }
    }

    /// The CPU and interrupt state, for diagnostics.
    pub fn dump(&mut self) -> String {
        let mut s = String::new();
        for (i, r) in self.cpu.r.iter().enumerate() {
            s += &format!("r{i}={r:08x} ");
        }
        s += &format!(
            "\ncpsr={:08x} irq_line={} fiq_line={} next_event={} clock={} | {}",
            self.cpu.cpsr(),
            self.irq_line,
            self.fiq_line,
            self.soc.next_event(self.clock),
            self.clock,
            self.soc.debug_state()
        );
        if self.history_on {
            s += "\nrecent pcs:";
            for k in 0..HISTORY as u64 {
                let i = (self.icount + 1 + k) as usize % HISTORY;
                s += &format!(" {:08x}", self.history[i]);
            }
        }
        s
    }

    // ---- boot ------------------------------------------------------------

    /// Places a kernel image and ATAGs in RAM as a bootloader does, and sets
    /// the registers the kernel expects on entry.
    pub fn boot_linux(&mut self, kernel: &[u8], load_at: u32, machine_type: u32, cmdline: &str) {
        let off = (load_at - RAM_BASE) as usize;
        self.ram[off..off + kernel.len()].copy_from_slice(kernel);
        let atags = 0xc000_0100u32;
        let mut words: Vec<u32> = vec![
            5,
            0x5441_0001,
            1,
            4096,
            0, // ATAG_CORE
            4,
            0x5441_0002,
            RAM_SIZE as u32,
            RAM_BASE, // ATAG_MEM
        ];
        let mut cmd = cmdline.as_bytes().to_vec();
        cmd.push(0);
        while !cmd.len().is_multiple_of(4) {
            cmd.push(0);
        }
        words.push(2 + (cmd.len() / 4) as u32);
        words.push(0x5441_0009);
        for c in cmd.chunks(4) {
            words.push(u32::from_le_bytes(c.try_into().unwrap()));
        }
        words.extend([0, 0]); // ATAG_NONE
        for (i, w) in words.iter().enumerate() {
            self.phys_write(atags + 4 * i as u32, *w, Width::W);
        }
        self.cpu.r[0] = 0;
        self.cpu.r[1] = machine_type;
        self.cpu.r[2] = atags;
        self.cpu.r[15] = load_at;
    }
}
