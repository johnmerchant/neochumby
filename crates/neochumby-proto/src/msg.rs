//! The messages, after the handshake. Each is a tag byte and its fields:
//! strings as a u16 length and UTF-8, ports as u16, all big-endian.

use std::io;

use crate::{MAX_MSG, invalid};

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
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        let s = |b: &mut Vec<u8>, s: &str| {
            b.extend_from_slice(&(s.len() as u16).to_be_bytes());
            b.extend_from_slice(s.as_bytes());
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
            Msg::Dev => b.push(10),
            Msg::Log(lines) => {
                b.push(11);
                b.extend_from_slice(&(lines.len() as u16).to_be_bytes());
                for l in lines {
                    s(&mut b, l);
                }
            }
            Msg::Restart => b.push(12),
            Msg::Shot => b.push(13),
            Msg::Screen { width, height } => {
                b.push(14);
                b.extend_from_slice(&width.to_be_bytes());
                b.extend_from_slice(&height.to_be_bytes());
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
            10 => Msg::Dev,
            11 => {
                let n = u16_(&mut rest)?;
                let mut lines = Vec::new();
                for _ in 0..n {
                    lines.push(string(&mut rest)?);
                }
                Msg::Log(lines)
            }
            12 => Msg::Restart,
            13 => Msg::Shot,
            14 => {
                let width = u16_(&mut rest)?;
                Msg::Screen { width, height: u16_(&mut rest)? }
            }
            _ => return Err(invalid("unknown message")),
        };
        if !rest.is_empty() {
            return Err(invalid("trailing bytes"));
        }
        Ok(m)
    }
}
