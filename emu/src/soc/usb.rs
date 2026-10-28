//! The i.MX21 USB OTG block (at 0x10024000): so far its control registers
//! and a host controller root hub with three empty ports. The ETD-based
//! transfer engine is not modelled yet.

const RST_CTRL: u32 = 0x10;
const CLK_CTRL: u32 = 0x0c;
const ROOTHUBA: u32 = 0xe8;
const PORTSTAT0: u32 = 0xf4;

pub struct Usb {
    regs: Vec<u32>,
}

impl Usb {
    pub fn new() -> Usb {
        let mut regs = vec![0u32; 0x2000 / 4];
        // Three downstream ports, individually powered.
        regs[(ROOTHUBA / 4) as usize] = 0x0000_0103;
        Usb { regs }
    }

    pub fn read(&self, off: u32) -> u32 {
        match off {
            // Resets complete at once.
            RST_CTRL => 0,
            _ => self.regs[(off / 4) as usize % self.regs.len()],
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        let i = (off / 4) as usize % self.regs.len();
        match off {
            RST_CTRL => {}
            // The clock enables read back as written.
            CLK_CTRL => self.regs[i] = v,
            ROOTHUBA => self.regs[i] = (self.regs[i] & 0xff) | (v & !0xff),
            o if (PORTSTAT0..PORTSTAT0 + 12).contains(&o) => {
                // Port status is write-1-to-act; with nothing connected the
                // only state that sticks is port power (bit 8).
                if v & (1 << 8) != 0 {
                    self.regs[i] |= 1 << 8;
                }
                if v & (1 << 9) != 0 {
                    self.regs[i] &= !(1 << 8);
                }
            }
            _ => self.regs[i] = v,
        }
    }

    pub fn irq(&self) -> bool {
        false
    }
}
