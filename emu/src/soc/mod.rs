//! The i.MX21's peripherals and its physical address map.

pub mod aitc;
pub mod cp;
pub mod crm;
pub mod cspi;
pub mod gpio;
pub mod gpt;
pub mod lcdc;
pub mod nfc;
pub mod tsc2100;
pub mod uart;
pub mod usb;

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use crate::machine::Width;

const IRQ_UART: [u32; 4] = [20, 19, 18, 17];
const IRQ_GPT: [u32; 3] = [26, 25, 24];
const IRQ_NFC: u32 = 29;
const IRQ_GPIO: u32 = 8;
const IRQ_CSPI: [u32; 3] = [16, 15, 6];
const IRQ_USBHOST: u32 = 55;

pub struct Soc {
    pub aitc: aitc::Aitc,
    pub gpt: [gpt::Gpt; 3],
    pub uart: [uart::Uart; 4],
    pub crm: crm::Crm,
    pub usb: usb::Usb,
    pub nfc: nfc::Nfc,
    pub gpio: gpio::Gpio,
    pub lcdc: lcdc::Lcdc,
    /// PWM (0x10006000): control, sample, period, counter. Samples are
    /// taken at once, so the FIFO always has room.
    pwm: [u32; 4],
    /// CSPI1, CSPI2 and CSPI3 (the TSC2100's).
    pub cspi: [cspi::Cspi; 3],
    /// The crypto processor on UART3.
    pub cp: cp::Cp,
    pub touch: tsc2100::SharedTouch,
    tsc: std::rc::Rc<std::cell::RefCell<tsc2100::Tsc2100>>,
    /// Registers of blocks not modelled yet: they read back what was
    /// written, and their first accesses are logged.
    stub: HashMap<u32, u32>,
    logged: HashSet<u32>,
    pub log_mmio: bool,
    /// Logs every device access.
    pub log_io: Vec<(u32, u32)>,
    console_in: Option<Receiver<u8>>,
    /// The cycle time of the last device access or tick.
    now: u64,
    /// Console input from the control port, waiting for FIFO room.
    console_pending: std::collections::VecDeque<u8>,
    pub control: Option<Receiver<crate::control::Request>>,
    /// A tap's release: when, and who to answer.
    release: Option<(u64, std::sync::mpsc::Sender<String>)>,
}

/// Screen pixel to raw touch ADC values: the inverse of the neutral
/// calibration a settings partition gives (/psp/ts_settings
/// 0,4095,4095,-4095,0, the ADC's full range; see the README).
fn raw_touch(x: u32, y: u32) -> (u16, u16) {
    let x = x.min(319) as i64;
    let y = y.min(239) as i64;
    ((x * 4095 / 320) as u16, (4095 - y * 4095 / 240) as u16)
}

/// Address ranges to log every access to, from CHUMBY_LOG_IO: "all", or
/// comma-separated hex ranges ("10017000-10017fff,10015400-100154ff").
fn io_log_ranges() -> Vec<(u32, u32)> {
    let Ok(spec) = std::env::var("CHUMBY_LOG_IO") else { return Vec::new() };
    if spec == "all" || spec == "1" {
        return vec![(0, u32::MAX)];
    }
    spec.split(",")
        .filter_map(|r| {
            let (a, b) = r.split_once("-")?;
            Some((u32::from_str_radix(a, 16).ok()?, u32::from_str_radix(b, 16).ok()?))
        })
        .collect()
}

impl Soc {
    fn logs(&self, pa: u32) -> bool {
        self.log_io.iter().any(|&(a, b)| (a..=b).contains(&pa))
    }

    pub fn new(nfc: nfc::Nfc) -> Soc {
        let touch = tsc2100::SharedTouch::default();
        let tsc = std::rc::Rc::new(std::cell::RefCell::new(tsc2100::Tsc2100::new(touch.clone())));
        Soc {
            aitc: aitc::Aitc::new(),
            gpt: [gpt::Gpt::new(), gpt::Gpt::new(), gpt::Gpt::new()],
            uart: [uart::Uart::new(), uart::Uart::new(), uart::Uart::new(), uart::Uart::new()],
            crm: crm::Crm::new(),
            usb: usb::Usb::new(),
            nfc,
            gpio: gpio::Gpio::new(),
            lcdc: lcdc::Lcdc::new(),
            pwm: [0; 4],
            cspi: [
                cspi::Cspi::new(None),
                cspi::Cspi::new(None),
                cspi::Cspi::new(Some(Box::new(tsc2100::Port(tsc.clone())))),
            ],
            cp: cp::Cp::new(),
            touch,
            tsc,
            stub: HashMap::new(),
            logged: HashSet::new(),
            log_mmio: true,
            log_io: io_log_ranges(),
            console_in: None,
            now: 0,
            console_pending: Default::default(),
            control: None,
            release: None,
        }
    }

    /// Feeds the host's stdin to UART1, the console.
    pub fn attach_stdin(&mut self) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 256];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 {
                    break;
                }
                for &b in &buf[..n] {
                    if tx.send(b).is_err() {
                        return;
                    }
                }
            }
        });
        self.console_in = Some(rx);
    }

    fn stub_access(&mut self, pa: u32, val: Option<u32>, w: Width) -> u32 {
        let key = pa & !3;
        if self.log_mmio && self.logged.insert(pa) {
            let size = match w {
                Width::B => 8,
                Width::H => 16,
                Width::W => 32,
            };
            match val {
                Some(v) => eprintln!("[mmio] write{size} {pa:08x} = {v:08x} (unmodelled)"),
                None => eprintln!("[mmio] read{size}  {pa:08x} (unmodelled)"),
            }
        }
        match val {
            Some(v) => {
                let shift = (pa & 3) * 8;
                let mask = match w {
                    Width::B => 0xffu32 << shift,
                    Width::H => 0xffffu32 << shift,
                    Width::W => 0xffff_ffff,
                };
                let e = self.stub.entry(key).or_insert(0);
                *e = (*e & !mask) | ((v << shift) & mask);
                0
            }
            None => self.stub.get(&key).copied().unwrap_or(0) >> ((pa & 3) * 8),
        }
    }

    pub fn read(&mut self, pa: u32, w: Width, now: u64, ram: &mut [u8]) -> u32 {
        let v = self.read_inner(pa, w, now, ram);
        if self.logs(pa) {
            eprintln!("[io] r {pa:08x} -> {v:08x}");
        }
        v
    }

    fn read_inner(&mut self, pa: u32, w: Width, now: u64, _ram: &mut [u8]) -> u32 {
        let off = pa & 0xfff;
        let perclk1 = self.crm.perclk1_hz();
        match pa >> 12 {
            0x10003..=0x10005 => self.gpt[(pa >> 12) as usize - 0x10003].read(off, now, perclk1),
            0x1000a..=0x1000d => self.uart[(pa >> 12) as usize - 0x1000a].read(off),
            0x1000e => self.cspi[0].read(off),
            0x1000f => self.cspi[1].read(off),
            0x10017 => self.cspi[2].read(off),
            0x10024 | 0x10025 => self.usb.read(pa & 0x1fff),
            0x10021 => self.lcdc.read(off),
            0x10006 if off < 0x10 => {
                const PWMC_FIFOAV: u32 = 1 << 5;
                self.pwm[(off / 4) as usize] | if off == 0 { PWMC_FIFOAV } else { 0 }
            }
            0x10015 => self.gpio.read(off),
            0x10027 => self.crm.read(off),
            0xdf003 => self.nfc.read(off, w),
            0x10040 => self.aitc.read(off),
            _ => self.stub_access(pa, None, w),
        }
    }

    pub fn write(&mut self, pa: u32, val: u32, w: Width, now: u64, _ram: &mut [u8]) {
        self.now = now;
        if self.logs(pa) {
            eprintln!("[io] w {pa:08x} <- {val:08x}");
        }
        let off = pa & 0xfff;
        let perclk1 = self.crm.perclk1_hz();
        match pa >> 12 {
            0x10003..=0x10005 => self.gpt[(pa >> 12) as usize - 0x10003].write(off, val, now, perclk1),
            0x1000a..=0x1000d => {
                let n = (pa >> 12) as usize - 0x1000a;
                self.uart[n].write(off, val);
                self.flush_uarts();
            }
            0x10024 | 0x10025 => self.usb.write(pa & 0x1fff, val),
            0x1000e => self.cspi[0].write(off, val),
            0x1000f => self.cspi[1].write(off, val),
            0x10017 => {
                self.cspi[2].write(off, val);
                self.sync_pintdav();
            }
            0x10021 => self.lcdc.write(off, val),
            0x10006 if off < 0x10 => self.pwm[(off / 4) as usize] = val,
            0x10015 => self.gpio.write(off, val),
            0x10027 => self.crm.write(off, val),
            0xdf003 => self.nfc.write(off, val, w),
            0x10040 => self.aitc.write(off, val),
            _ => {
                self.stub_access(pa, Some(val), w);
            }
        }
    }

    fn flush_uarts(&mut self) {
        let console = &mut self.uart[0];
        if !console.tx.is_empty() {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&console.tx);
            let _ = out.flush();
            console.tx.clear();
        }
        for b in std::mem::take(&mut self.uart[2].tx) {
            self.cp.receive(b, self.now);
        }
        for (n, u) in self.uart.iter_mut().enumerate().skip(1) {
            if !u.tx.is_empty() {
                eprintln!("[uart{}] {:02x?}", n + 1, u.tx);
                u.tx.clear();
            }
        }
    }

    pub fn debug_state(&self) -> String {
        format!("gpt1: {:?}\ngpio:{}", self.gpt[0], self.gpio.debug())
    }

    /// Presses (at a screen pixel) or releases the touchscreen.
    fn set_touch(&mut self, at: Option<(u32, u32)>) {
        {
            let mut t = self.touch.lock().unwrap();
            match at {
                Some((x, y)) => {
                    let (rx, ry) = raw_touch(x, y);
                    *t = tsc2100::Touch { down: true, x: rx, y: ry };
                }
                None => t.down = false,
            }
        }
        self.sync_pintdav();
    }

    /// Copies the TSC2100's PINTDAV output to its GPIO pin, PE19.
    pub fn sync_pintdav(&mut self) {
        let low = self.tsc.borrow().pintdav_low();
        self.gpio.set_pin(4, 19, !low);
    }

    /// Moves the CP's replies into UART3 as its FIFO has room.
    fn feed_cp(&mut self, now: u64) {
        while self.uart[2].rx_space() > 0 {
            match self.cp.take(now) {
                Some(b) => self.uart[2].receive(b),
                None => break,
            }
        }
    }

    /// Brings time-driven devices up to `now`.
    pub fn tick(&mut self, now: u64, _ram: &mut [u8]) {
        let perclk1 = self.crm.perclk1_hz();
        for g in &mut self.gpt {
            g.sync(now, perclk1);
        }
        self.now = now;
        self.cp.uptime = (now / crate::machine::CPU_HZ) as u32;
        self.feed_cp(now);
    }

    /// The earliest cycle time at which a device needs attention.
    pub fn next_event(&self, _now: u64) -> u64 {
        let perclk1 = self.crm.perclk1_hz();
        let gpt = self.gpt.iter().map(|g| g.next_event(perclk1)).min().unwrap_or(u64::MAX);
        gpt.min(self.cp.next_due().unwrap_or(u64::MAX))
    }

    /// Moves host input into the devices. Returns true if any arrived.
    pub fn poll_host(&mut self, now: u64, ram: &[u8]) -> bool {
        let mut any = false;
        if let Some((at, _)) = &self.release
            && now >= *at
        {
            let (_, reply) = self.release.take().unwrap();
            self.set_touch(None);
            let _ = reply.send("ok".into());
            any = true;
        }
        while let Some(req) = self.control.as_ref().and_then(|c| c.try_recv().ok()) {
            any = true;
            use crate::control::Command;
            match req.cmd {
                Command::Down(x, y) => {
                    self.set_touch(Some((x, y)));
                    let _ = req.reply.send("ok".into());
                }
                Command::Up => {
                    self.set_touch(None);
                    let _ = req.reply.send("ok".into());
                }
                Command::Tap(x, y) => {
                    self.set_touch(Some((x, y)));
                    self.release = Some((now + crate::machine::CPU_HZ / 10, req.reply));
                }
                Command::Console(text) => {
                    self.console_pending.extend(text.bytes());
                    self.console_pending.push_back(b'\r');
                    let _ = req.reply.send("ok".into());
                }
                Command::Shot(path) => {
                    let (w, h, px) = self.lcdc.render(ram);
                    let r = std::fs::write(&path, crate::png::encode(w, h, &px));
                    let _ = req.reply.send(match r {
                        Ok(()) => "ok".into(),
                        Err(e) => format!("error: {e}"),
                    });
                }
            }
        }
        while self.uart[0].rx_space() > 0 {
            match self.console_pending.pop_front() {
                Some(b) => {
                    self.uart[0].receive(b);
                    any = true;
                }
                None => break,
            }
        }
        if let Some(rx) = &self.console_in {
            while self.uart[0].rx_space() > 0 {
                match rx.try_recv() {
                    Ok(b) => {
                        self.uart[0].receive(if b == b'\n' { b'\r' } else { b });
                        any = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.console_in = None;
                        break;
                    }
                }
            }
        }
        any
    }

    pub fn interrupt_lines(&mut self) -> (bool, bool) {
        let mut src = 0u64;
        for (n, u) in self.uart.iter().enumerate() {
            if u.irq() {
                src |= 1 << IRQ_UART[n];
            }
        }
        for (n, g) in self.gpt.iter().enumerate() {
            if g.irq() {
                src |= 1 << IRQ_GPT[n];
            }
        }
        for (n, c) in self.cspi.iter().enumerate() {
            if c.irq() {
                src |= 1 << IRQ_CSPI[n];
            }
        }
        if self.gpio.irq() {
            src |= 1 << IRQ_GPIO;
        }
        if self.nfc.irq() {
            src |= 1 << IRQ_NFC;
        }
        if self.usb.irq() {
            src |= 1 << IRQ_USBHOST;
        }
        self.aitc.sources = src;
        (self.aitc.irq(), self.aitc.fiq())
    }
}
