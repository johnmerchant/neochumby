//! The i.MX21 NAND flash controller (at 0xDF003000) and the chip behind
//! it: a Hynix 64 MiB small-page part (ID 0xAD 0x76), 512-byte pages with
//! 16 spare bytes, 32 pages to a 16 KiB block.
//!
//! The NFC has four 512-byte main buffers and four 16-byte spare buffers;
//! the driver writes a command or address byte and starts a cycle through
//! CONFIG2, and every cycle completes at once.
//!
//! The chip's contents are a NAND image of 528-byte records (data then
//! spare, as nanddump writes them). Programs and erases are written back to
//! the image file unless the image was opened read-only.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

pub const PAGE: usize = 512;
pub const SPARE: usize = 16;
pub const RECORD: usize = PAGE + SPARE;
pub const PAGES: usize = 131_072;
pub const PAGES_PER_BLOCK: usize = 32;

const CONFIG1_SP_EN: u16 = 1 << 2;
const CONFIG1_INT_MSK: u16 = 1 << 4;
const CONFIG2_FCMD: u16 = 1 << 0;
const CONFIG2_FADD: u16 = 1 << 1;
const CONFIG2_FDI: u16 = 1 << 2;
const CONFIG2_INT: u16 = 1 << 15;

/// NAND status: ready, not write-protected.
const STATUS_READY: u8 = 0xc0;

pub struct Nfc {
    buf: Vec<u8>,
    rba: u16,
    bufsize: u16,
    addr_reg: u16,
    cmd_reg: u16,
    config: u16,
    wr_prot: u16,
    unlock_start: u16,
    unlock_end: u16,
    config1: u16,
    config2: u16,
    chip: Chip,
}

struct Chip {
    data: Vec<u8>,
    file: Option<File>,
    cmd: u8,
    addr: Vec<u8>,
    /// Page and byte (0..528) the next data output starts from.
    page: usize,
    col: usize,
    /// The page register for programming.
    prog: Vec<u8>,
    prog_col: usize,
    /// The area (0, 256 or 512) the last 0x00/0x01/0x50 pointed at, which
    /// a program's column counts from.
    area: usize,
    status: u8,
}

impl Chip {
    fn decode_row(&self, bytes: &[u8]) -> usize {
        let mut row = 0usize;
        for (i, b) in bytes.iter().enumerate() {
            row |= (*b as usize) << (8 * i);
        }
        row % PAGES
    }

    fn command(&mut self, c: u8) {
        match c {
            0x00 | 0x01 | 0x50 | 0x80 | 0x60 | 0x90 => {
                self.cmd = c;
                self.addr.clear();
                match c {
                    0x80 => self.prog = vec![0xff; RECORD],
                    0x00 => self.area = 0,
                    0x01 => self.area = 256,
                    0x50 => self.area = PAGE,
                    _ => {}
                }
            }
            0x10 => {
                // Program: bits can only go from 1 to 0.
                let base = self.page * RECORD;
                for i in 0..RECORD {
                    self.data[base + i] &= self.prog[i];
                }
                self.write_back(base, RECORD);
                self.cmd = 0;
            }
            0xd0 => {
                let first = self.page - self.page % PAGES_PER_BLOCK;
                let base = first * RECORD;
                let len = PAGES_PER_BLOCK * RECORD;
                self.data[base..base + len].fill(0xff);
                self.write_back(base, len);
                self.cmd = 0;
            }
            0x70 => self.cmd = c,
            0xff => {
                self.cmd = 0;
                self.addr.clear();
            }
            _ => eprintln!("[nand] unknown command {c:02x}"),
        }
    }

    fn address(&mut self, a: u8) {
        self.addr.push(a);
        match self.cmd {
            0x00 | 0x01 | 0x50 | 0x80 => {
                if self.addr.len() >= 2 {
                    self.page = self.decode_row(&self.addr[1..]);
                }
                let span = if self.area == PAGE { SPARE } else { 256 };
                self.col = self.area + self.addr[0] as usize % span;
                self.prog_col = self.col;
            }
            0x60 => self.page = self.decode_row(&self.addr),
            _ => {}
        }
    }

    /// Reads `n` bytes from the current position, moving on through the
    /// page and into the next.
    fn output(&mut self, n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.data[self.page * RECORD + self.col]);
            self.col += 1;
            if self.col == RECORD {
                self.col = 0;
                self.page = (self.page + 1) % PAGES;
            }
        }
        out
    }

    fn input(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.prog_col < RECORD {
                self.prog[self.prog_col] = b;
                self.prog_col += 1;
            }
        }
    }

    fn write_back(&mut self, off: usize, len: usize) {
        if let Some(f) = &mut self.file {
            let r = f.seek(SeekFrom::Start(off as u64)).and_then(|_| f.write_all(&self.data[off..off + len]));
            if let Err(e) = r {
                eprintln!("[nand] write-back failed: {e}");
            }
        }
    }
}

impl Nfc {
    /// A controller with the given NAND image; `file` receives writes.
    pub fn new(mut data: Vec<u8>, file: Option<File>) -> Nfc {
        data.resize(PAGES * RECORD, 0xff);
        Nfc {
            buf: vec![0xff; 0x840],
            rba: 0,
            bufsize: 1,
            addr_reg: 0,
            cmd_reg: 0,
            config: 0,
            wr_prot: 0,
            unlock_start: 0,
            unlock_end: 0,
            config1: 0,
            config2: 0,
            chip: Chip {
                data,
                file,
                cmd: 0,
                addr: Vec::new(),
                page: 0,
                col: 0,
                prog: vec![0xff; RECORD],
                prog_col: 0,
                area: 0,
                status: STATUS_READY,
            },
        }
    }

    pub fn irq(&self) -> bool {
        self.config2 & CONFIG2_INT != 0 && self.config1 & CONFIG1_INT_MSK == 0
    }

    fn main_off(&self) -> usize {
        (self.rba as usize & 3) * PAGE
    }

    fn spare_off(&self) -> usize {
        0x800 + (self.rba as usize & 3) * SPARE
    }

    fn start(&mut self, v: u16) {
        let spare_only = self.config1 & CONFIG1_SP_EN != 0;
        if v & CONFIG2_FCMD != 0 {
            self.chip.command(self.cmd_reg as u8);
        } else if v & CONFIG2_FADD != 0 {
            self.chip.address(self.addr_reg as u8);
        } else if v & CONFIG2_FDI != 0 {
            let (m, s) = (self.main_off(), self.spare_off());
            if !spare_only {
                let main = self.buf[m..m + PAGE].to_vec();
                self.chip.input(&main);
            }
            let spare = self.buf[s..s + SPARE].to_vec();
            self.chip.input(&spare);
        } else {
            match (v >> 3) & 7 {
                1 => {
                    let (m, s) = (self.main_off(), self.spare_off());
                    if !spare_only {
                        let main = self.chip.output(PAGE);
                        self.buf[m..m + PAGE].copy_from_slice(&main);
                    }
                    let spare = self.chip.output(SPARE);
                    self.buf[s..s + SPARE].copy_from_slice(&spare);
                }
                2 => {
                    let m = self.main_off();
                    self.buf[m..m + 6].copy_from_slice(&[0xad, 0x76, 0xad, 0x76, 0xad, 0x76]);
                }
                4 => {
                    let m = self.main_off();
                    self.buf[m] = self.chip.status;
                    self.buf[m + 1] = self.chip.status;
                }
                _ => {}
            }
        }
        self.config2 = v | CONFIG2_INT;
        // The cycle is over: the start bits clear.
        self.config2 &= !0x3f;
    }

    pub fn read(&self, off: u32, w: crate::machine::Width) -> u32 {
        use crate::machine::Width;
        let off = off as usize;
        if off < self.buf.len() {
            return match w {
                Width::B => self.buf[off] as u32,
                Width::H => u16::from_le_bytes([self.buf[off], self.buf[off + 1]]) as u32,
                Width::W => u32::from_le_bytes(self.buf[off..off + 4].try_into().unwrap()),
            };
        }
        let r = |o: usize| -> u16 {
            match o {
                0xe00 => self.bufsize,
                0xe04 => self.rba,
                0xe06 => self.addr_reg,
                0xe08 => self.cmd_reg,
                0xe0a => self.config,
                // No ECC errors.
                0xe0c | 0xe0e | 0xe10 => 0,
                0xe12 => self.wr_prot,
                0xe14 => self.unlock_start,
                0xe16 => self.unlock_end,
                // Unlocked.
                0xe18 => 0x4,
                0xe1a => self.config1,
                0xe1c => self.config2,
                _ => 0,
            }
        };
        match w {
            Width::W => r(off & !3) as u32 | (r((off & !3) + 2) as u32) << 16,
            _ => r(off & !1) as u32,
        }
    }

    pub fn write(&mut self, off: u32, v: u32, w: crate::machine::Width) {
        use crate::machine::Width;
        let off = off as usize;
        if off < self.buf.len() {
            match w {
                Width::B => self.buf[off] = v as u8,
                Width::H => self.buf[off..off + 2].copy_from_slice(&(v as u16).to_le_bytes()),
                Width::W => self.buf[off..off + 4].copy_from_slice(&v.to_le_bytes()),
            }
            return;
        }
        if w == Width::W {
            self.write_reg(off & !3, v as u16);
            self.write_reg((off & !3) + 2, (v >> 16) as u16);
        } else {
            self.write_reg(off & !1, v as u16);
        }
    }

    fn write_reg(&mut self, off: usize, v: u16) {
        match off {
            0xe00 => self.bufsize = v,
            0xe04 => self.rba = v & 3,
            0xe06 => self.addr_reg = v,
            0xe08 => self.cmd_reg = v,
            0xe0a => self.config = v,
            0xe12 => self.wr_prot = v,
            0xe14 => self.unlock_start = v,
            0xe16 => self.unlock_end = v,
            0xe1a => self.config1 = v & !(1 << 6),
            0xe1c => {
                if v & 0x3f != 0 {
                    self.start(v);
                } else {
                    // Writing 0 to INT acknowledges it.
                    self.config2 = v;
                }
            }
            _ => {}
        }
    }
}
