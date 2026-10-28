//! The messages, after the handshake. Each is a tag byte and its fields:
//! strings as a u16 length and UTF-8, ports as u16, sizes as u64, hashes as
//! their 32 bytes, all big-endian.

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
    /// home screen). A Chumby that has no use for them reads past them.
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
    /// Chumby: how the round ended: `ok` once the set is live, and what
    /// happened, for neochumbyd's log.
    SyncDone { ok: bool, detail: String },
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
            _ => return Err(invalid("unknown message")),
        };
        if !rest.is_empty() {
            return Err(invalid("trailing bytes"));
        }
        Ok(m)
    }
}
