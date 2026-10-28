//! The messages, after the handshake. Each is a tag byte and its fields:
//! strings as a u16 length and UTF-8, ports as u16, sizes as u64, hashes as
//! their 32 bytes, all big-endian.
//!
//! Tags taken: 0-9 the core (pairing, companions, settings); 10-18 sync
//! (delivery=sync, #10), with 19-31 left for it and anything near it;
//! 32-39 the developer link (crate::dev): 32-36 its log, restart and
//! screenshot, 37-39 input (taps and the top button, opt-in).

use std::io;

use crate::{MAX_MSG, invalid};

/// One file of a synced set (`delivery=sync`): its path in the tree, its
/// size, and its SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SyncEntry {
    pub path: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// Chumby, first: who it is.
    Hello { name: String },
    /// neochumbyd: paired; carry on.
    Welcome,
    /// neochumbyd: this key is not paired.
    Unpaired,
    /// Chumby: asks to pair, under this name; both now show the code.
    Pair { name: String },
    /// neochumbyd: approved; mount the apps from here.
    Paired { nfs_port: u16, path: String },
    /// neochumbyd: not approved.
    Refused,
    /// Chumby: connect me to this app's companion (apps/<app> in the tree).
    Open { app: String },
    /// Either way, once open: one frame for, or from, the companion. What
    /// it holds is the app's business.
    Data(Vec<u8>),
    /// neochumbyd: the companion has gone (or the app has none).
    Closed,
    /// neochumbyd, right after `Welcome`: this Chumby's settings as its
    /// owner chose them on the server, as key/value pairs (`start` names the
    /// start app). A Chumby that has no use for them reads past them.
    Config(Vec<(String, String)>),
    /// Chumby, with `delivery=sync`, after `Config`: starts a round. `live`
    /// is the digest of its live set's manifest (zeros if it has none).
    SyncHave { live: [u8; 32] },
    /// neochumbyd: the live set is the tree already; the round is over.
    SyncUpToDate,
    /// neochumbyd: the tree differs; send the standby set's manifest.
    SyncWant,
    /// Either way, part of a manifest, in path order: the Chumby's standby
    /// set after `SyncWant`, then the tree's from neochumbyd. `last` ends it.
    SyncManifest { entries: Vec<SyncEntry>, last: bool },
    /// neochumbyd: one of the files the standby set lacks. Its `size` bytes
    /// follow as `SyncData`.
    SyncFile(SyncEntry),
    /// neochumbyd: the next bytes of the file being sent.
    SyncData(Vec<u8>),
    /// neochumbyd: every file the standby set lacked has been sent.
    SyncEnd,
    /// How a round ended. From the Chumby, at its end: `ok` once the set
    /// is live, and what happened, for neochumbyd's log. From neochumbyd, in
    /// place of `SyncUpToDate` or `SyncWant`: never `ok`, a tree it will not
    /// send, and why.
    SyncDone { ok: bool, detail: String },
    /// Chumby, between `SyncEnd` and its `SyncDone`: still working (hashing
    /// files kept from a round cut short), `done` of `total` bytes. Each
    /// one restarts neochumbyd's wait.
    SyncProgress { done: u64, total: u64 },
    /// Chumby, after `Config`, in place of `Open`: this connection is the
    /// developer link ([`crate::dev`]). The Chumby streams its log on it,
    /// and neochumbyd may ask for a restart or a screenshot.
    Dev,
    /// Chumby, on the developer link: lines of its log, oldest first, each
    /// without its newline. neochumbyd keeps the last few thousand.
    Log(Vec<String>),
    /// neochumbyd, on the developer link: end the apps and services and
    /// start the session again from the tree (`neochumbyd restart NAME`).
    Restart,
    /// neochumbyd, on the developer link: send the screen
    /// (`neochumbyd shot NAME OUT.png`).
    Shot,
    /// Chumby, answering `Shot`: the screen is this size, and follows as
    /// `Data`, width x height RGB565 pixels, little-endian, row by row, in
    /// as many messages as it takes. A width or height of 0: no screen.
    Screen { width: u16, height: u16 },
    /// neochumbyd, on the developer link of a Chumby set `dev=on`: touch
    /// the screen at (x, y), in screen pixels, for `hold_ms` (at most
    /// [`crate::dev::MAX_HOLD_MS`]), then let go (`neochumbyd tap`).
    Tap { x: u16, y: u16, hold_ms: u16 },
    /// neochumbyd, on the developer link of a Chumby set `dev=on`: the top
    /// button goes down, or comes up (`neochumbyd button`).
    Button { down: bool },
    /// Chumby, answering each `Tap` (once it has let go) or `Button`:
    /// done, or why not.
    Done { ok: bool, why: String },
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        let s = |b: &mut Vec<u8>, s: &str| {
            b.extend_from_slice(&(s.len() as u16).to_be_bytes());
            b.extend_from_slice(s.as_bytes());
        };
        let entry = |b: &mut Vec<u8>, e: &SyncEntry| {
            s(b, &e.path);
            b.extend_from_slice(&e.size.to_be_bytes());
            b.extend_from_slice(&e.sha256);
        };
        match self {
            Msg::Hello { name } => {
                b.push(0);
                s(&mut b, name);
            }
            Msg::Welcome => b.push(1),
            Msg::Unpaired => b.push(2),
            Msg::Pair { name } => {
                b.push(3);
                s(&mut b, name);
            }
            Msg::Paired { nfs_port, path } => {
                b.push(4);
                b.extend_from_slice(&nfs_port.to_be_bytes());
                s(&mut b, path);
            }
            Msg::Refused => b.push(5),
            Msg::Open { app } => {
                b.push(6);
                s(&mut b, app);
            }
            Msg::Data(d) => {
                b.push(7);
                b.extend_from_slice(d);
            }
            Msg::Closed => b.push(8),
            Msg::Config(pairs) => {
                b.push(9);
                b.extend_from_slice(&(pairs.len() as u16).to_be_bytes());
                for (k, v) in pairs {
                    s(&mut b, k);
                    s(&mut b, v);
                }
            }
            Msg::SyncHave { live } => {
                b.push(10);
                b.extend_from_slice(live);
            }
            Msg::SyncUpToDate => b.push(11),
            Msg::SyncWant => b.push(12),
            Msg::SyncManifest { entries, last } => {
                b.push(13);
                b.push(*last as u8);
                b.extend_from_slice(&(entries.len() as u16).to_be_bytes());
                for e in entries {
                    entry(&mut b, e);
                }
            }
            Msg::SyncFile(e) => {
                b.push(14);
                entry(&mut b, e);
            }
            Msg::SyncData(d) => {
                b.push(15);
                b.extend_from_slice(d);
            }
            Msg::SyncEnd => b.push(16),
            Msg::SyncDone { ok, detail } => {
                b.push(17);
                b.push(*ok as u8);
                s(&mut b, detail);
            }
            Msg::SyncProgress { done, total } => {
                b.push(18);
                b.extend_from_slice(&done.to_be_bytes());
                b.extend_from_slice(&total.to_be_bytes());
            }
            Msg::Dev => b.push(32),
            Msg::Log(lines) => {
                b.push(33);
                b.extend_from_slice(&(lines.len() as u16).to_be_bytes());
                for l in lines {
                    s(&mut b, l);
                }
            }
            Msg::Restart => b.push(34),
            Msg::Shot => b.push(35),
            Msg::Screen { width, height } => {
                b.push(36);
                b.extend_from_slice(&width.to_be_bytes());
                b.extend_from_slice(&height.to_be_bytes());
            }
            Msg::Tap { x, y, hold_ms } => {
                b.push(37);
                for v in [x, y, hold_ms] {
                    b.extend_from_slice(&v.to_be_bytes());
                }
            }
            Msg::Button { down } => b.extend_from_slice(&[38, *down as u8]),
            Msg::Done { ok, why } => {
                b.extend_from_slice(&[39, *ok as u8]);
                s(&mut b, why);
            }
        }
        b
    }

    pub fn decode(b: &[u8]) -> io::Result<Msg> {
        if b.len() > MAX_MSG {
            return Err(invalid("message too long"));
        }
        let (&tag, mut rest) = b.split_first().ok_or_else(|| invalid("empty message"))?;
        let u16_ = |r: &mut &[u8]| -> io::Result<u16> {
            let (h, t) = r.split_at_checked(2).ok_or_else(|| invalid("short message"))?;
            *r = t;
            Ok(u16::from_be_bytes([h[0], h[1]]))
        };
        let string = |r: &mut &[u8]| -> io::Result<String> {
            let n = u16_(r)? as usize;
            let (h, t) = r.split_at_checked(n).ok_or_else(|| invalid("short message"))?;
            *r = t;
            String::from_utf8(h.to_vec()).map_err(|_| invalid("not UTF-8"))
        };
        let take = |r: &mut &[u8], n: usize| -> io::Result<Vec<u8>> {
            let (h, t) = r.split_at_checked(n).ok_or_else(|| invalid("short message"))?;
            *r = t;
            Ok(h.to_vec())
        };
        let flag = |r: &mut &[u8]| -> io::Result<bool> {
            match take(r, 1)?[0] {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(invalid("not a flag")),
            }
        };
        let hash = |r: &mut &[u8]| -> io::Result<[u8; 32]> { Ok(take(r, 32)?.try_into().expect("32 bytes")) };
        let entry = |r: &mut &[u8]| -> io::Result<SyncEntry> {
            let path = string(r)?;
            let size = u64::from_be_bytes(take(r, 8)?.try_into().expect("8 bytes"));
            Ok(SyncEntry { path, size, sha256: hash(r)? })
        };
        let m = match tag {
            0 => Msg::Hello { name: string(&mut rest)? },
            1 => Msg::Welcome,
            2 => Msg::Unpaired,
            3 => Msg::Pair { name: string(&mut rest)? },
            4 => {
                let nfs_port = u16_(&mut rest)?;
                Msg::Paired { nfs_port, path: string(&mut rest)? }
            }
            5 => Msg::Refused,
            6 => Msg::Open { app: string(&mut rest)? },
            7 => {
                let d = rest.to_vec();
                rest = &[];
                Msg::Data(d)
            }
            8 => Msg::Closed,
            9 => {
                let n = u16_(&mut rest)?;
                let mut pairs = Vec::new();
                for _ in 0..n {
                    let k = string(&mut rest)?;
                    pairs.push((k, string(&mut rest)?));
                }
                Msg::Config(pairs)
            }
            10 => Msg::SyncHave { live: hash(&mut rest)? },
            11 => Msg::SyncUpToDate,
            12 => Msg::SyncWant,
            13 => {
                let last = flag(&mut rest)?;
                let n = u16_(&mut rest)?;
                let mut entries = Vec::new();
                for _ in 0..n {
                    entries.push(entry(&mut rest)?);
                }
                Msg::SyncManifest { entries, last }
            }
            14 => Msg::SyncFile(entry(&mut rest)?),
            15 => {
                let d = rest.to_vec();
                rest = &[];
                Msg::SyncData(d)
            }
            16 => Msg::SyncEnd,
            17 => {
                let ok = flag(&mut rest)?;
                Msg::SyncDone { ok, detail: string(&mut rest)? }
            }
            18 => {
                let done = u64::from_be_bytes(take(&mut rest, 8)?.try_into().expect("8 bytes"));
                let total = u64::from_be_bytes(take(&mut rest, 8)?.try_into().expect("8 bytes"));
                Msg::SyncProgress { done, total }
            }
            32 => Msg::Dev,
            33 => {
                let n = u16_(&mut rest)?;
                let mut lines = Vec::new();
                for _ in 0..n {
                    lines.push(string(&mut rest)?);
                }
                Msg::Log(lines)
            }
            34 => Msg::Restart,
            35 => Msg::Shot,
            36 => {
                let width = u16_(&mut rest)?;
                Msg::Screen { width, height: u16_(&mut rest)? }
            }
            37 => {
                let (x, y) = (u16_(&mut rest)?, u16_(&mut rest)?);
                let hold_ms = u16_(&mut rest)?;
                if hold_ms > crate::dev::MAX_HOLD_MS {
                    return Err(invalid("too long a hold"));
                }
                Msg::Tap { x, y, hold_ms }
            }
            38 => Msg::Button { down: flag(&mut rest)? },
            39 => {
                let ok = flag(&mut rest)?;
                Msg::Done { ok, why: string(&mut rest)? }
            }
            _ => return Err(invalid("unknown message")),
        };
        if !rest.is_empty() {
            return Err(invalid("trailing bytes"));
        }
        Ok(m)
    }
}
