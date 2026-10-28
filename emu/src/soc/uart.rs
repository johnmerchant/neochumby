//! The i.MX21 UARTs (at 0x1000A000 + 0x1000 * n). Transmitted bytes leave
//! at once, so the transmitter always reads as empty; received bytes wait
//! in a 32-byte FIFO.

use std::collections::VecDeque;

const UCR1_UARTEN: u32 = 1 << 0;
const UCR1_TXMPTYEN: u32 = 1 << 6;
const UCR1_RRDYEN: u32 = 1 << 9;
const UCR1_TRDYEN: u32 = 1 << 13;
const UCR2_SRST: u32 = 1 << 0;
const UCR2_RXEN: u32 = 1 << 1;
const UCR4_DREN: u32 = 1 << 0;
const UCR4_TCEN: u32 = 1 << 3;

const USR1_TRDY: u32 = 1 << 13;
const USR1_RRDY: u32 = 1 << 9;
const USR2_RDR: u32 = 1 << 0;
const USR2_TXDC: u32 = 1 << 3;
const USR2_TXFE: u32 = 1 << 14;

const UTS_TXEMPTY: u32 = 1 << 6;
const UTS_RXEMPTY: u32 = 1 << 5;

const RX_FIFO: usize = 32;

pub struct Uart {
    ucr: [u32; 4],
    ufcr: u32,
    uesc: u32,
    utim: u32,
    ubir: u32,
    ubmr: u32,
    onems: u32,
    uts: u32,
    usr1_sticky: u32,
    usr2_sticky: u32,
    rx: VecDeque<u8>,
    /// Bytes the guest has transmitted, for the host side to collect.
    pub tx: Vec<u8>,
}

impl Uart {
    pub fn new() -> Uart {
        Uart {
            ucr: [0, UCR2_SRST, 0x0700, 0x8000],
            ufcr: 0x0801,
            uesc: 0x2b,
            utim: 0,
            ubir: 0,
            ubmr: 0,
            onems: 0,
            uts: 0,
            usr1_sticky: 0,
            usr2_sticky: 0,
            rx: VecDeque::new(),
            tx: Vec::new(),
        }
    }

    fn enabled(&self) -> bool {
        self.ucr[0] & UCR1_UARTEN != 0
    }

    /// Room in the receive FIFO.
    pub fn rx_space(&self) -> usize {
        if self.enabled() && self.ucr[1] & UCR2_RXEN != 0 { RX_FIFO - self.rx.len() } else { 0 }
    }

    pub fn receive(&mut self, b: u8) {
        if self.rx.len() < RX_FIFO {
            self.rx.push_back(b);
        }
    }

    fn usr1(&self) -> u32 {
        let mut v = USR1_TRDY | self.usr1_sticky;
        let rxtl = (self.ufcr & 0x3f).max(1) as usize;
        if self.rx.len() >= rxtl {
            v |= USR1_RRDY;
        }
        v
    }

    fn usr2(&self) -> u32 {
        let mut v = USR2_TXDC | USR2_TXFE | self.usr2_sticky;
        if !self.rx.is_empty() {
            v |= USR2_RDR;
        }
        v
    }

    pub fn irq(&self) -> bool {
        if !self.enabled() {
            return false;
        }
        let (u1, u2) = (self.usr1(), self.usr2());
        (self.ucr[0] & UCR1_TRDYEN != 0 && u1 & USR1_TRDY != 0)
            || (self.ucr[0] & UCR1_RRDYEN != 0 && u1 & USR1_RRDY != 0)
            || (self.ucr[0] & UCR1_TXMPTYEN != 0 && u2 & USR2_TXFE != 0)
            || (self.ucr[3] & UCR4_DREN != 0 && u2 & USR2_RDR != 0)
            || (self.ucr[3] & UCR4_TCEN != 0 && u2 & USR2_TXDC != 0)
    }

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x00..=0x3c => match self.rx.pop_front() {
                Some(b) => (1 << 15) | b as u32,
                None => 0,
            },
            0x80 => self.ucr[0],
            0x84 => self.ucr[1],
            0x88 => self.ucr[2],
            0x8c => self.ucr[3],
            0x90 => self.ufcr,
            0x94 => self.usr1(),
            0x98 => self.usr2(),
            0x9c => self.uesc,
            0xa0 => self.utim,
            0xa4 => self.ubir,
            0xa8 => self.ubmr,
            0xac => 0,
            0xb0 => self.onems,
            0xb4 => {
                let mut v = self.uts | UTS_TXEMPTY;
                if self.rx.is_empty() {
                    v |= UTS_RXEMPTY;
                }
                v
            }
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x40..=0x7c => {
                if self.enabled() {
                    self.tx.push(v as u8);
                }
            }
            0x80 => self.ucr[0] = v,
            0x84 => {
                if v & UCR2_SRST == 0 {
                    // Software reset; it completes at once.
                    self.rx.clear();
                    self.usr1_sticky = 0;
                    self.usr2_sticky = 0;
                }
                self.ucr[1] = v | UCR2_SRST;
            }
            0x88 => self.ucr[2] = v,
            0x8c => self.ucr[3] = v,
            0x90 => self.ufcr = v,
            0x94 => self.usr1_sticky &= !v,
            0x98 => self.usr2_sticky &= !v,
            0x9c => self.uesc = v,
            0xa0 => self.utim = v,
            0xa4 => self.ubir = v,
            0xa8 => self.ubmr = v,
            0xb0 => self.onems = v,
            0xb4 => self.uts = v & !(UTS_TXEMPTY | UTS_RXEMPTY),
            _ => {}
        }
    }
}
