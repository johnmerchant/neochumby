//! How a Chumby and its neochumbyd talk, over TCP: TLS 1.3 by default, or
//! Noise_XX with the `noise` feature. The choice is made at build time for
//! both ends; nothing is negotiated.
//!
//! Each side has a static key, its [`Identity`], and presents its public
//! half in the handshake: TLS with raw public keys (RFC 7250, Ed25519), no
//! certificates. Each side then checks the other's key against the keys it
//! has paired with ([`Session::peer`]). Pairing is how those are learnt:
//! both sides show a code derived from the handshake ([`Session::code`]),
//! and someone who can see both says they match, which no one in the
//! middle can arrange.
//!
//! ChaCha20-Poly1305 either way: the Chumby's ARM926 has no AES
//! instructions. Blocking std I/O: the Chumby has one connection, and
//! neochumbyd gives each a thread.
//!
//! An app with a server half (its companion, run by neochumbyd) talks to it
//! through the session: the Chumby's half sends [`Msg::Open`], and frames
//! then go both ways as [`Msg::Data`], each one a [`frame`] on the
//! companion's stdin or stdout.
//!
//! A Chumby with `delivery=sync` keeps a copy of its tree on its stick, in
//! step with neochumbyd, through the `Sync*` messages (neochumby-sync has
//! the rounds they make up).
//!
//! A paired Chumby's session also keeps a connection of its own, the
//! developer link ([`dev`]): it sends [`Msg::Dev`], streams its log as
//! [`Msg::Log`], and answers [`Msg::Restart`] and [`Msg::Shot`], so that
//! working on a Chumby needs no shell on it; and, only on a Chumby its
//! owner set `dev=on`, [`Msg::Tap`] and [`Msg::Button`].
//!
//! The protocol has a [`VERSION`], agreed on as each connection starts
//! ([`version`]), so that each end keeps working with the other's older
//! builds. A Chumby's settings travel as [`Msg::Config`]'s pairs and are
//! read as [`Settings`]. What an app and its companion say to each other
//! is the app's own business; [`channel`] carries it as typed messages.

use std::fmt::Write as _;
use std::io;

pub mod channel;
pub mod dev;
mod msg;
pub mod settings;
pub mod version;
pub use msg::{Msg, SyncEntry};
pub use settings::{Delivery, Settings};

/// The newest protocol version this build speaks. Version 0 is the
/// protocol from before versions were numbered; [`version`] has how two
/// ends agree on one, and [`Msg`] what each version added.
///
/// - 1: [`Msg::Version`]; and [`Msg::Unknown`] in place of an error for a
///   tag not known, skipped where [`Msg`] says.
pub const VERSION: u16 = 1;

/// A companion's frames, on its stdin and stdout: a u32 big-endian length,
/// then that many bytes, at most [`MAX_FRAME`].
pub mod frame {
    use std::io::{self, Read, Write};

    /// What one [`crate::Msg::Data`] carries.
    pub const MAX_FRAME: usize = crate::MAX_MSG - 1;

    pub fn write(w: &mut impl Write, b: &[u8]) -> io::Result<()> {
        if b.len() > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too long"));
        }
        let mut f = Vec::with_capacity(4 + b.len());
        f.extend_from_slice(&(b.len() as u32).to_be_bytes());
        f.extend_from_slice(b);
        w.write_all(&f)?;
        w.flush()
    }

    /// The next frame, or None at the end of the stream.
    pub fn read(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
        let mut len = [0u8; 4];
        match r.read_exact(&mut len) {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            r => r?,
        }
        let n = u32::from_be_bytes(len) as usize;
        if n > MAX_FRAME {
            return Err(crate::invalid("frame too long"));
        }
        let mut b = vec![0u8; n];
        r.read_exact(&mut b)?;
        Ok(Some(b))
    }
}

#[cfg(feature = "noise")]
#[path = "noise.rs"]
mod backend;
#[cfg(not(feature = "noise"))]
#[path = "tls.rs"]
mod backend;

pub use backend::{Identity, Session};

/// neochumbyd's port for this protocol.
pub const API_PORT: u16 = 7340;

/// The most a [`Msg`] may take, encoded: what one Noise message carries.
pub const MAX_MSG: usize = 65535 - 16;

/// A public key, 32 bytes either way (Ed25519 or X25519).
pub type PublicKey = [u8; 32];

fn invalid(what: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// Bytes as lower-case hex.
pub fn hex(b: &[u8]) -> String {
    b.iter().fold(String::with_capacity(2 * b.len()), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The inverse of [`hex`].
pub fn unhex(s: &str) -> io::Result<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err(invalid("odd-length hex"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).unwrap_or("x"), 16).map_err(|_| invalid("not hex")))
        .collect()
}

/// A public key from its [`hex`].
pub fn unhex_key(s: &str) -> io::Result<PublicKey> {
    unhex(s)?.try_into().map_err(|_| invalid("not a 32-byte key"))
}

/// Six digits from 4 bytes both ends derived alike from a handshake.
fn six_digits(b: [u8; 4]) -> String {
    format!("{:06}", u32::from_be_bytes(b) % 1_000_000)
}

/// A Chumby's name, as it pairs and as its AirPlay receiver is called:
/// 1 to 32 of a-z, 0-9, '_' and '-'.
pub fn valid_name(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    #[test]
    fn handshake_and_messages() {
        let (server, chumby) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (sp, cp) = (server.public(), chumby.public());
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut s = Session::accept(l.accept().unwrap().0, &server).unwrap();
            assert_eq!(s.peer(), &cp);
            let m = s.recv().unwrap();
            // Nothing for a while: a timeout, and nothing lost by it.
            assert_eq!(s.recv_timeout(Duration::from_millis(50)).unwrap(), None);
            s.send(&Msg::Data(vec![7; 40000])).unwrap();
            s.send(&Msg::Closed).unwrap();
            (s.code(), m)
        });
        let mut c = Session::connect(TcpStream::connect(addr).unwrap(), &chumby).unwrap();
        assert_eq!(c.peer(), &sp);
        c.send(&Msg::Hello { name: "kitchen_chumby".into() }).unwrap();
        // Waits that end mid-message keep what came.
        let mut got = Vec::new();
        while got.len() < 2 {
            if let Some(m) = c.recv_timeout(Duration::from_millis(1)).unwrap() {
                got.push(m);
            }
        }
        assert_eq!(got, [Msg::Data(vec![7; 40000]), Msg::Closed]);
        let (code, m) = t.join().unwrap();
        assert_eq!(code, c.code());
        assert_eq!(code.len(), 6);
        assert_eq!(m, Msg::Hello { name: "kitchen_chumby".into() });
    }

    #[test]
    fn identities_round_trip() {
        let id = Identity::generate().unwrap();
        let again = Identity::from_secret(&id.secret()).unwrap();
        assert_eq!(again.public(), id.public());
        assert_eq!(unhex_key(&hex(&id.public())).unwrap(), id.public());
        assert!(valid_name("kitchen_chumby") && !valid_name("Kitchen") && !valid_name(""));
    }

    #[test]
    fn messages_round_trip() {
        for m in [
            Msg::Hello { name: "a".into() },
            Msg::Welcome,
            Msg::Unpaired,
            Msg::Pair { name: "b-1".into() },
            Msg::Paired { nfs_port: 2049, path: "/".into() },
            Msg::Refused,
            Msg::Open { app: "airplay".into() },
            Msg::Data(vec![1, 2, 3]),
            Msg::Closed,
            Msg::Config(vec![]),
            Msg::Config(vec![("start".into(), "wx".into()), ("delivery".into(), "nfs".into())]),
            Msg::SyncHave { live: [3; 32] },
            Msg::SyncUpToDate,
            Msg::SyncWant,
            Msg::SyncManifest { entries: vec![], last: true },
            Msg::SyncManifest {
                entries: vec![
                    SyncEntry { path: "neochumby".into(), size: 1 << 33, sha256: [1; 32] },
                    SyncEntry { path: "apps/home/manifest".into(), size: 0, sha256: [0xff; 32] },
                ],
                last: false,
            },
            Msg::SyncFile(SyncEntry { path: "apps/doom/doom1.wad".into(), size: 4196020, sha256: [9; 32] }),
            Msg::SyncData(vec![0; 1000]),
            Msg::SyncEnd,
            Msg::SyncDone { ok: false, detail: "no space".into() },
            Msg::SyncProgress { done: 1 << 20, total: 512 << 20 },
            Msg::Dev,
            Msg::Log(vec![]),
            Msg::Log(vec!["neochumby: opening home".into(), String::new(), "ünïcode".into()]),
            Msg::Restart,
            Msg::Shot,
            Msg::Screen { width: 320, height: 240 },
            Msg::Screen { width: 0, height: 0 },
            Msg::Tap { x: 0, y: 0, hold_ms: 0 },
            Msg::Tap { x: 319, y: 239, hold_ms: dev::MAX_HOLD_MS },
            Msg::Button { down: true },
            Msg::Button { down: false },
            Msg::Done { ok: true, why: String::new() },
            Msg::Done { ok: false, why: "dev is off".into() },
        ] {
            assert_eq!(Msg::decode(&m.encode()).unwrap(), m);
        }
        assert_eq!(Msg::decode(&[200]).unwrap(), Msg::Unknown { tag: 200, body: vec![] });
        assert!(Msg::decode(&[0, 9, 0]).is_err());
        assert!(Msg::decode(&[9, 0, 1]).is_err());
        assert!(Msg::decode(&[10, 0]).is_err(), "short hash");
        assert!(Msg::decode(&[13, 2, 0, 0]).is_err(), "a flag is 0 or 1");
        assert!(Msg::decode(&[16, 0]).is_err());
        assert!(Msg::decode(&[18, 0, 0, 0, 0, 0, 0, 0, 1]).is_err(), "short");
        // Short, long, and a count with too few lines.
        assert!(Msg::decode(&[36, 1, 64, 0]).is_err());
        assert!(Msg::decode(&[34, 0]).is_err());
        assert!(Msg::decode(&[33, 0, 2, 0, 1, b'x']).is_err());
        // The dev tags, where #10's sync messages don't reach.
        assert_eq!(Msg::Dev.encode(), [32]);
        assert_eq!(Msg::Shot.encode(), [35]);
        assert_eq!(Msg::Tap { x: 1, y: 2, hold_ms: 3 }.encode(), [37, 0, 1, 0, 2, 0, 3]);
        assert_eq!(Msg::Button { down: true }.encode(), [38, 1]);
        // Input: short, long, a hold past the limit, a bool that isn't one.
        assert!(Msg::decode(&[37, 0, 1, 0, 2, 0]).is_err());
        assert!(Msg::decode(&[37, 0, 1, 0, 2, 0, 3, 0]).is_err());
        let over = (dev::MAX_HOLD_MS + 1).to_be_bytes();
        assert!(Msg::decode(&[37, 0, 1, 0, 2, over[0], over[1]]).is_err());
        assert!(Msg::decode(&[38]).is_err());
        assert!(Msg::decode(&[38, 2]).is_err());
        assert!(Msg::decode(&[38, 1, 0]).is_err());
        assert!(Msg::decode(&[39, 1]).is_err());
        assert!(Msg::decode(&[39, 7, 0, 0]).is_err());
        assert!(Msg::decode(&[39, 0, 0, 2, b'x']).is_err());
    }

    /// Every message of version 0 as that version encodes it, byte for
    /// byte: deployed peers decode these, and send them, so they never
    /// change.
    fn version_0() -> Vec<(Msg, Vec<u8>)> {
        let e = |p: &str, size: u64, h: u8| SyncEntry { path: p.into(), size, sha256: [h; 32] };
        let entry = |p: &[u8], size: u64, h: u8| {
            [&(p.len() as u16).to_be_bytes()[..], p, &size.to_be_bytes(), &[h; 32]].concat()
        };
        vec![
            (Msg::Hello { name: "den".into() }, b"\x00\x00\x03den".to_vec()),
            (Msg::Welcome, vec![1]),
            (Msg::Unpaired, vec![2]),
            (Msg::Pair { name: "den".into() }, b"\x03\x00\x03den".to_vec()),
            (Msg::Paired { nfs_port: 2049, path: "/".into() }, b"\x04\x08\x01\x00\x01/".to_vec()),
            (Msg::Refused, vec![5]),
            (Msg::Open { app: "logs".into() }, b"\x06\x00\x04logs".to_vec()),
            (Msg::Data(vec![9, 8]), vec![7, 9, 8]),
            (Msg::Closed, vec![8]),
            (
                Msg::Config(vec![("start".into(), "wx".into()), ("dev".into(), "on".into())]),
                b"\x09\x00\x02\x00\x05start\x00\x02wx\x00\x03dev\x00\x02on".to_vec(),
            ),
            (Msg::SyncHave { live: [3; 32] }, [&[10][..], &[3; 32]].concat()),
            (Msg::SyncUpToDate, vec![11]),
            (Msg::SyncWant, vec![12]),
            (
                Msg::SyncManifest { entries: vec![e("a", 1, 2)], last: true },
                [&[13, 1, 0, 1][..], &entry(b"a", 1, 2)].concat(),
            ),
            (Msg::SyncFile(e("b", 5, 6)), [&[14][..], &entry(b"b", 5, 6)].concat()),
            (Msg::SyncData(vec![1]), vec![15, 1]),
            (Msg::SyncEnd, vec![16]),
            (Msg::SyncDone { ok: true, detail: "ok".into() }, b"\x11\x01\x00\x02ok".to_vec()),
            (Msg::SyncProgress { done: 1, total: 2 }, [&[18][..], &1u64.to_be_bytes(), &2u64.to_be_bytes()].concat()),
            (Msg::Dev, vec![32]),
            (Msg::Log(vec!["a".into()]), vec![33, 0, 1, 0, 1, b'a']),
            (Msg::Restart, vec![34]),
            (Msg::Shot, vec![35]),
            (Msg::Screen { width: 320, height: 240 }, vec![36, 1, 64, 0, 240]),
            (Msg::Tap { x: 1, y: 2, hold_ms: 3 }, vec![37, 0, 1, 0, 2, 0, 3]),
            (Msg::Button { down: true }, vec![38, 1]),
            (Msg::Done { ok: false, why: "no".into() }, b"\x27\x00\x00\x02no".to_vec()),
        ]
    }

    #[test]
    fn version_0_encodings_are_pinned() {
        for (m, bytes) in version_0() {
            assert_eq!(m.since(), Some(0), "{m:?}");
            assert_eq!(m.encode(), bytes, "{m:?}");
            assert_eq!(Msg::decode(&bytes).unwrap(), m);
        }
    }

    /// A version-0 neochumbyd's decoder refuses a Hello with anything
    /// after the name, and any tag it doesn't know: why a Chumby says its
    /// version only after neochumbyd has said its own (crate::version).
    /// Hello is as version 0 sends it, and so is all a new Chumby sends
    /// before then.
    #[test]
    fn hello_is_as_version_0_sends_it() {
        let hello = b"\x00\x00\x06lounge".to_vec();
        assert_eq!(Msg::decode(&hello).unwrap(), Msg::Hello { name: "lounge".into() });
        assert_eq!(Msg::Hello { name: "lounge".into() }.encode(), hello);
        // This decoder, as the version-0 one did, refuses bytes after the name.
        assert!(Msg::decode(&[hello, vec![0, 1]].concat()).is_err());
    }

    /// What a new neochumbyd sends a version-0 Chumby is only version-0
    /// messages, `Config` with one pair more: decoded by version 0's
    /// format, which takes any pairs.
    #[test]
    fn config_with_the_version_is_version_0s_format() {
        let s = Settings { start: Some("wx".into()), ..Default::default() };
        let m = version::config(&s);
        assert_eq!(m.since(), Some(0));
        assert_eq!(m.encode(), b"\x09\x00\x02\x00\x05start\x00\x02wx\x00\x0dproto.version\x00\x011".to_vec());
    }

    #[test]
    fn version_1() {
        assert_eq!(Msg::Version(1).since(), Some(1));
        assert_eq!(Msg::Version(1).encode(), [40, 0, 1]);
        assert_eq!(Msg::decode(&[40, 0, 1]).unwrap(), Msg::Version(1));
        assert!(Msg::decode(&[40, 0]).is_err());
        assert!(Msg::decode(&[40, 0, 1, 0]).is_err());
    }

    /// A tag not known is kept, whole, for the receiver to skip; a known
    /// tag's bad body is still an error.
    #[test]
    fn unknown_messages() {
        for tag in [19, 31, 41, 99, 255] {
            let m = Msg::decode(&[tag, 1, 2]).unwrap();
            assert_eq!(m, Msg::Unknown { tag, body: vec![1, 2] });
            assert_eq!(m.since(), None);
            assert_eq!(m.encode(), [tag, 1, 2]);
        }
        assert!(Msg::decode(&[1, 0]).is_err());
    }

    #[test]
    fn frames() {
        let mut b = Vec::new();
        frame::write(&mut b, b"one").unwrap();
        frame::write(&mut b, b"").unwrap();
        let mut r = &b[..];
        assert_eq!(frame::read(&mut r).unwrap(), Some(b"one".to_vec()));
        assert_eq!(frame::read(&mut r).unwrap(), Some(Vec::new()));
        assert_eq!(frame::read(&mut r).unwrap(), None);
        assert!(frame::write(&mut b, &vec![0; frame::MAX_FRAME + 1]).is_err());
    }
}
