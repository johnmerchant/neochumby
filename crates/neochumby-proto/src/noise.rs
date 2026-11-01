//! Noise_XX_25519_ChaChaPoly_BLAKE2s (snow, pure Rust), in place of TLS
//! with the `noise` feature. XX exchanges both static X25519 keys,
//! encrypted, in the handshake; whether the other's is one we are paired
//! with is the caller's question ([`Session::peer`]). Each message, of the
//! handshake and after, is a u16 big-endian length and a Noise message.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::{MAX_MSG, Msg, PublicKey, Stream, hex, invalid, unhex};

const PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const PROLOGUE: &[u8] = b"neochumby 1";
const NOISE_MAX: usize = 65535;

fn builder() -> io::Result<snow::Builder<'static>> {
    Ok(snow::Builder::new(PARAMS.parse().map_err(invalid)?))
}

fn key(b: &[u8]) -> io::Result<PublicKey> {
    b.try_into().map_err(|_| invalid("not a 32-byte key"))
}

/// Our static key: an X25519 key pair.
pub struct Identity {
    private: [u8; 32],
    public: PublicKey,
}

impl Identity {
    pub fn generate() -> io::Result<Identity> {
        let kp = builder()?.generate_keypair().map_err(invalid)?;
        Ok(Identity { private: key(&kp.private)?, public: key(&kp.public)? })
    }

    /// From what [`Identity::secret`] wrote.
    pub fn from_secret(s: &str) -> io::Result<Identity> {
        let b = unhex(s)?;
        if b.len() != 64 {
            return Err(invalid("not a key pair"));
        }
        Ok(Identity { private: key(&b[..32])?, public: key(&b[32..])? })
    }

    /// The key pair, for keeping (hex, private then public).
    pub fn secret(&self) -> String {
        hex(&[self.private, self.public].concat())
    }

    pub fn public(&self) -> PublicKey {
        self.public
    }

    /// None: an X25519 key can't sign, so a Noise build's neochumbyd
    /// serves no signed manifest (neochumby_sync::signed).
    pub fn sign(&self, _msg: &[u8]) -> Option<[u8; 64]> {
        None
    }
}

/// An open, authenticated connection.
pub struct Session {
    tcp: Box<dyn Stream>,
    noise: snow::TransportState,
    peer: PublicKey,
    buf: Vec<u8>,
    /// Received and not yet a whole Noise message.
    rbuf: Vec<u8>,
    /// The protocol version agreed with the peer (crate::version).
    version: u16,
}

impl Session {
    /// The Chumby's side: opens the handshake. Its peer's key is not yet
    /// checked.
    pub(crate) fn client(tcp: Box<dyn Stream>, me: &Identity) -> io::Result<Session> {
        Session::handshake(tcp, me, true, None)
    }

    /// neochumbyd's side, its peer's key not yet checked; the handshake by
    /// `deadline`, if one is given.
    pub(crate) fn server(tcp: Box<dyn Stream>, me: &Identity, deadline: Option<Instant>) -> io::Result<Session> {
        Session::handshake(tcp, me, false, deadline)
    }

    /// XX, by `deadline` if one is given; the socket's read timeout is put
    /// back as it was.
    fn handshake(
        mut tcp: Box<dyn Stream>,
        me: &Identity,
        initiator: bool,
        deadline: Option<Instant>,
    ) -> io::Result<Session> {
        let was = tcp.read_timeout()?;
        let mut io = crate::ByDeadline { tcp: &mut *tcp, deadline };
        let b = builder()?.local_private_key(&me.private).map_err(invalid)?.prologue(PROLOGUE).map_err(invalid)?;
        let mut hs = if initiator { b.build_initiator() } else { b.build_responder() }.map_err(invalid)?;
        let mut buf = vec![0u8; NOISE_MAX];
        let mut scratch = vec![0u8; NOISE_MAX];
        // XX: -> e; <- e, ee, s, es; -> s, se.
        let mut ours = initiator;
        while !hs.is_handshake_finished() {
            if ours {
                let n = hs.write_message(&[], &mut buf).map_err(invalid)?;
                send_frame(&mut io, &buf[..n])?;
            } else {
                let n = recv_frame(&mut io, &mut buf)?;
                hs.read_message(&buf[..n], &mut scratch).map_err(invalid)?;
            }
            ours = !ours;
        }
        tcp.set_read_timeout(was)?;
        let peer = key(hs.get_remote_static().unwrap_or_default())?;
        let noise = hs.into_transport_mode().map_err(invalid)?;
        Ok(Session { tcp, noise, peer, buf, rbuf: Vec::new(), version: 0 })
    }

    /// The other side's public key, proven in the handshake.
    pub fn peer(&self) -> &PublicKey {
        &self.peer
    }

    /// What the session runs over.
    pub fn stream(&self) -> &dyn Stream {
        self.tcp.as_ref()
    }

    /// The protocol version agreed with the peer: 0 until
    /// [`crate::version`] has agreed on another.
    pub fn version(&self) -> u16 {
        self.version
    }

    pub(crate) fn set_version(&mut self, v: u16) {
        self.version = v;
    }

    /// Sends `msg`, if the agreed version has it (else
    /// [`io::ErrorKind::Unsupported`]).
    pub fn send(&mut self, msg: &Msg) -> io::Result<()> {
        crate::version::sendable(msg, self.version)?;
        self.send_body(&msg.encode())
    }

    /// Sends an encoded message as it is.
    pub(crate) fn send_body(&mut self, body: &[u8]) -> io::Result<()> {
        if body.len() > MAX_MSG {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "message too long"));
        }
        let n = self.noise.write_message(body, &mut self.buf).map_err(invalid)?;
        send_frame(&mut self.tcp, &self.buf[..n])
    }

    pub fn recv(&mut self) -> io::Result<Msg> {
        self.tcp.set_read_timeout(None)?;
        loop {
            if let Some(m) = self.take()? {
                return Ok(m);
            }
            self.fill()?;
        }
    }

    /// A message if one comes within `t`. Waiting out the time loses
    /// nothing: what came of a message meanwhile is kept for the next call.
    pub fn recv_timeout(&mut self, t: Duration) -> io::Result<Option<Msg>> {
        if let Some(m) = self.take()? {
            return Ok(Some(m));
        }
        self.tcp.set_read_timeout(Some(t))?;
        match self.fill() {
            Ok(()) => self.take(),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// A whole message off the front of what has come, if there is one.
    fn take(&mut self) -> io::Result<Option<Msg>> {
        let Some(len) = self.rbuf.first_chunk::<2>() else { return Ok(None) };
        let n = u16::from_be_bytes(*len) as usize;
        if self.rbuf.len() < 2 + n {
            return Ok(None);
        }
        let r = self.noise.read_message(&self.rbuf[2..2 + n], &mut self.buf);
        self.rbuf.drain(..2 + n);
        let m = r.map_err(invalid)?;
        Msg::decode(&self.buf[..m]).map(Some)
    }

    /// Reads what there is (at least something) onto `rbuf`.
    fn fill(&mut self) -> io::Result<()> {
        let mut chunk = [0u8; 16384];
        match self.tcp.read(&mut chunk)? {
            0 => Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                self.rbuf.extend_from_slice(&chunk[..n]);
                Ok(())
            }
        }
    }
}

fn send_frame(s: &mut impl Write, b: &[u8]) -> io::Result<()> {
    let mut f = Vec::with_capacity(2 + b.len());
    f.extend_from_slice(&(b.len() as u16).to_be_bytes());
    f.extend_from_slice(b);
    s.write_all(&f)
}

fn recv_frame(s: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut len = [0u8; 2];
    s.read_exact(&mut len)?;
    let n = u16::from_be_bytes(len) as usize;
    s.read_exact(&mut buf[..n])?;
    Ok(n)
}
