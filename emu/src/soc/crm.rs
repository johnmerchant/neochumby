//! CRM, the i.MX21 clock and reset module (at 0x10027000), and the system
//! control registers beside it (0x10027800). The PLLs are always locked;
//! the registers start as a bootloader leaves them: MPLL 266 MHz from the
//! 26 MHz oscillator, HCLK 133 MHz, PERCLK1-4 44.3 MHz, SPLL 288 MHz.

const OSC_HZ: u64 = 26_000_000;
const FPM_HZ: u64 = 32_768 * 512;

pub struct Crm {
    regs: [u32; 0x40],
    sys: [u32; 0x40],
}

/// Decodes an i.MX21 PLL control register 0.
fn pll(reg: u32, fref: u64) -> u64 {
    let mfi = ((reg >> 10) & 0xf).max(5) as i64;
    let mut mfn = (reg & 0x3ff) as i64;
    if mfn >= 0x200 {
        mfn -= 0x400;
    }
    let mfd = ((reg >> 16) & 0x3ff) as i64 + 1;
    let pd = ((reg >> 26) & 0xf) as i64 + 1;
    let f = fref as i64;
    ((2 * f * mfi + 2 * f * mfn / mfd) / pd) as u64
}

impl Crm {
    pub fn new() -> Crm {
        let mut regs = [0u32; 0x40];
        regs[0x00 / 4] = 0x1401_0607; // CSCR: USB_DIV 5, MCU_SEL osc, BCLKDIV 1, IPDIV 1, PLLs on
        regs[0x04 / 4] = 0x0019_1403; // MPCTL0: 266 MHz
        regs[0x08 / 4] = 1 << 15; // MPCTL1: locked
        regs[0x0c / 4] = 0x000c_1407; // SPCTL0: 288 MHz
        regs[0x10 / 4] = 1 << 15; // SPCTL1: locked
        regs[0x14 / 4] = 0x3f00; // OSC26MCTL
        regs[0x18 / 4] = 0x0000_0000; // PCDR0
        regs[0x1c / 4] = 0x0505_0505; // PCDR1: PERCLKn = MPLL / 6
        regs[0x20 / 4] = 0xffff_ffff; // PCCR0: all clocks on
        regs[0x24 / 4] = 0xffff_ffff; // PCCR1
        let mut sys = [0u32; 0x40];
        sys[0x04 / 4] = 0x101d_101d; // SIDR: an i.MX21 silicon ID
        sys[0x14 / 4] = 0xffff_ffcb; // FMCR
        Crm { regs, sys }
    }

    pub fn mpll_hz(&self) -> u64 {
        let fref = if self.regs[0] & (1 << 16) != 0 { OSC_HZ } else { FPM_HZ };
        pll(self.regs[1], fref)
    }

    pub fn perclk1_hz(&self) -> u64 {
        self.mpll_hz() / ((self.regs[0x1c / 4] & 0x3f) as u64 + 1)
    }

    pub fn read(&self, off: u32) -> u32 {
        if off >= 0x800 {
            return self.sys[((off - 0x800) / 4) as usize % 0x40];
        }
        self.regs[(off / 4) as usize % 0x40]
    }

    pub fn write(&mut self, off: u32, v: u32) {
        if off >= 0x800 {
            self.sys[((off - 0x800) / 4) as usize % 0x40] = v;
            return;
        }
        let i = (off / 4) as usize % 0x40;
        self.regs[i] = match off {
            // Lock flags stay set.
            0x08 | 0x10 => v | (1 << 15),
            _ => v,
        };
    }
}
