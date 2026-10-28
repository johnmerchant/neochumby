//! The Chumby's crypto processor (CP), a microcontroller on UART3
//! (/dev/ttyS2) that holds the device's identity and an uptime clock.
//!
//! Protocol, as the stock `cpi` speaks it: each `!` is answered with `?`
//! (the wake-up handshake; `cpi` skips stray `?`s). A request is
//! `!!!!` + a four-letter command, optional base64 argument lines ending
//! in `\n`, then `\r`. A reply is a four-letter tag, one base64 data line
//! ending in `\r`, and a final `\r`; or `FAIL\r` when refused.
//!
//! The emulated CP is hardware 3.8 with a fixed, made-up identity: no
//! real Chumby's GUID, serial or keys.

use std::collections::VecDeque;

use crate::machine::CPU_HZ;

/// A reply starts this long after the request that prompted it.
const LATENCY: u64 = CPU_HZ / 500;
/// One character at 38400 baud.
const CHAR_TIME: u64 = CPU_HZ / 3840;

/// A neutral putative ID (GUID): recognisably not a real device's.
const PIDX: [u8; 16] = [0xc4, 0x0b, 0xb1, 0xe0, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x0e, 0x40, 0x00, 0x00, 0x00, 0x01];
/// Hardware version 3.8: minor 0x08, major 0x03 at bytes 12-13.
const HWVR: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x08, 0x03, 0, 0];

pub struct Cp {
    line: Vec<u8>,
    /// Reply bytes and the cycle time each is due on the wire.
    out: VecDeque<(u64, u8)>,
    now: u64,
    log: bool,
    /// Seconds since power-on, for TIME.
    pub uptime: u32,
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

impl Cp {
    pub fn new() -> Cp {
        Cp {
            line: Vec::new(),
            out: VecDeque::new(),
            now: 0,
            log: std::env::var_os("CHUMBY_LOG_CP").is_some(),
            uptime: 0,
        }
    }

    /// Queues reply bytes, paced at the line rate after a short delay.
    fn send(&mut self, bytes: &[u8]) {
        let mut t = self.out.back().map_or(self.now + LATENCY, |&(t, _)| t.max(self.now + LATENCY));
        for &b in bytes {
            t += CHAR_TIME;
            self.out.push_back((t, b));
        }
    }

    /// The next reply byte, if it is due by `now`.
    pub fn take(&mut self, now: u64) -> Option<u8> {
        match self.out.front() {
            Some(&(t, b)) if t <= now => {
                self.out.pop_front();
                Some(b)
            }
            _ => None,
        }
    }

    /// When the next reply byte is due, if any.
    pub fn next_due(&self) -> Option<u64> {
        self.out.front().map(|&(t, _)| t)
    }

    pub fn receive(&mut self, b: u8, now: u64) {
        self.now = now;
        if b == b'!' {
            self.send(b"?");
        }
        if b == b'\r' {
            let req = std::mem::take(&mut self.line);
            self.request(&req);
        } else {
            self.line.push(b);
        }
    }

    fn reply(&mut self, tag: &str, data: Option<&[u8]>) {
        let mut s = tag.to_string();
        if let Some(d) = data {
            s += &b64(d);
            s.push('\r');
        }
        s.push('\r');
        if self.log {
            eprintln!("[cp] reply {s:?}");
        }
        self.send(s.as_bytes());
    }

    fn request(&mut self, req: &[u8]) {
        let text = String::from_utf8_lossy(req);
        if self.log {
            eprintln!("[cp] request {text:?}");
        }
        // The wake-up `!` runs straight into the `!!!!` prefix.
        if !text.starts_with("!!!!") {
            return;
        }
        let cmd = text.trim_start_matches('!').chars().take(4).collect::<String>();
        match cmd.as_str() {
            "HWVR" => self.reply("HVRS", Some(&HWVR)),
            "PIDX" => self.reply("PIDX", Some(&PIDX)),
            "SNUM" => self.reply("SNUM", Some(b"0000000000000000")),
            "VERS" => self.reply("VRSR", Some(&[1, 0, 0, 0, 0, 0])),
            "TIME" => {
                let t = self.uptime.to_le_bytes();
                self.reply("TIME", Some(&t))
            }
            "CKEY" => self.reply("CKEY", Some(&[0, 0, 0, 0])),
            "ALRM" => self.reply("ASET", None),
            "DOWN" | "RSET" => self.send(b"\r"),
            _ => self.send(b"FAIL\r"),
        }
    }
}
