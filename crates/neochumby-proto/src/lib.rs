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
//! A paired Chumby's session also keeps a connection of its own, the
//! developer link ([`dev`]): it sends [`Msg::Dev`], streams its log as
//! [`Msg::Log`], and answers [`Msg::Restart`] and [`Msg::Shot`], so that
//! working on a Chumby needs no shell on it.

use std::fmt::Write as _;
use std::io;

pub mod dev;
mod msg;
pub use msg::Msg;

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
            Msg::Dev,
            Msg::Log(vec![]),
            Msg::Log(vec!["neochumby: opening home".into(), String::new(), "ünïcode".into()]),
            Msg::Restart,
            Msg::Shot,
            Msg::Screen { width: 320, height: 240 },
            Msg::Screen { width: 0, height: 0 },
        ] {
            assert_eq!(Msg::decode(&m.encode()).unwrap(), m);
        }
        assert!(Msg::decode(&[200]).is_err());
        assert!(Msg::decode(&[0, 9, 0]).is_err());
        assert!(Msg::decode(&[9, 0, 1]).is_err());
        // Short, long, and a count with too few lines.
        assert!(Msg::decode(&[14, 1, 64, 0]).is_err());
        assert!(Msg::decode(&[12, 0]).is_err());
        assert!(Msg::decode(&[11, 0, 2, 0, 1, b'x']).is_err());
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
