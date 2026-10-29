//! TLS 1.3 (rustls, ring): one suite, TLS_CHACHA20_POLY1305_SHA256 with
//! X25519, and both sides authenticated by Ed25519 raw public keys (RFC
//! 7250). The handshake proves each side holds the private half of the key
//! it presents; whether that key is one we are paired with is the caller's
//! question ([`Session::peer`]). Resumption is off: a resumed session would
//! not present the client's key again. Messages are a u32 big-endian length
//! and a [`Msg`].

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use ring::rand::SystemRandom;
use ring::signature::Ed25519KeyPair;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{AlwaysResolvesClientRawPublicKeys, Resumption};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls13_signature_with_raw_key};
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, SubjectPublicKeyInfoDer, UnixTime,
};
use rustls::server::AlwaysResolvesServerRawPublicKeys;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::sign::CertifiedKey;
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, DistinguishedName, ServerConfig, ServerConnection,
    SignatureScheme, StreamOwned,
};

use crate::{MAX_MSG, Msg, PublicKey, hex, invalid, six_digits, unhex};

const PAIRING_LABEL: &[u8] = b"EXPORTER-neochumby-pairing";

/// An Ed25519 SubjectPublicKeyInfo: this prefix, then the 32-byte key.
const ED25519_SPKI: [u8; 12] = [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];

fn provider() -> Arc<CryptoProvider> {
    let mut p = rustls::crypto::ring::default_provider();
    p.cipher_suites = vec![rustls::crypto::ring::cipher_suite::TLS13_CHACHA20_POLY1305_SHA256];
    p.kx_groups = vec![rustls::crypto::ring::kx_group::X25519];
    Arc::new(p)
}

/// The key in an Ed25519 SubjectPublicKeyInfo, and nothing else.
fn ed25519_key(spki: &[u8]) -> Result<PublicKey, rustls::Error> {
    match spki.strip_prefix(&ED25519_SPKI) {
        Some(k) if k.len() == 32 => Ok(k.try_into().expect("32 bytes")),
        _ => Err(rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)),
    }
}

/// Our static key: an Ed25519 key pair.
pub struct Identity {
    pkcs8: Vec<u8>,
    public: PublicKey,
    key: Arc<CertifiedKey>,
}

impl Identity {
    pub fn generate() -> io::Result<Identity> {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| invalid("no randomness"))?;
        Identity::from_pkcs8(pkcs8.as_ref().to_vec())
    }

    /// From what [`Identity::secret`] wrote.
    pub fn from_secret(s: &str) -> io::Result<Identity> {
        Identity::from_pkcs8(unhex(s)?)
    }

    /// The private key, for keeping (hex PKCS#8).
    pub fn secret(&self) -> String {
        hex(&self.pkcs8)
    }

    pub fn public(&self) -> PublicKey {
        self.public
    }

    fn from_pkcs8(pkcs8: Vec<u8>) -> io::Result<Identity> {
        let der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8.clone()));
        let signer = provider().key_provider.load_private_key(der).map_err(invalid)?;
        let spki = signer.public_key().ok_or_else(|| invalid("no public key"))?;
        let public = ed25519_key(spki.as_ref()).map_err(invalid)?;
        let key = Arc::new(CertifiedKey::new(vec![CertificateDer::from(spki.to_vec())], signer));
        Ok(Identity { pkcs8, public, key })
    }
}

/// Accepts any Ed25519 key whose holder signs the handshake: who it
/// belongs to is checked against the pairings afterwards.
#[derive(Debug)]
struct AnyEd25519Key(WebPkiSupportedAlgorithms);

impl AnyEd25519Key {
    fn new() -> Arc<AnyEd25519Key> {
        Arc::new(AnyEd25519Key(provider().signature_verification_algorithms))
    }

    fn verify(
        &self,
        msg: &[u8],
        spki: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        ed25519_key(spki.as_ref())?;
        verify_tls13_signature_with_raw_key(msg, &SubjectPublicKeyInfoDer::from(spki.as_ref()), dss, &self.0)
    }
}

impl ServerCertVerifier for AnyEd25519Key {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        ed25519_key(end_entity.as_ref()).map(|_| ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not offered".into()))
    }

    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        spki: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify(msg, spki, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

impl ClientCertVerifier for AnyEd25519Key {
    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        ed25519_key(end_entity.as_ref()).map(|_| ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not offered".into()))
    }

    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        spki: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify(msg, spki, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

enum Stream {
    Client(StreamOwned<ClientConnection, TcpStream>),
    Server(StreamOwned<ServerConnection, TcpStream>),
}

/// An open, authenticated connection.
pub struct Session {
    stream: Stream,
    peer: PublicKey,
    code: String,
    /// Received and not yet a whole message.
    rbuf: Vec<u8>,
    /// The protocol version agreed with the peer (crate::version).
    version: u16,
}

impl Session {
    /// The Chumby's side.
    pub fn connect(tcp: TcpStream, me: &Identity) -> io::Result<Session> {
        let mut config = ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(invalid)?
            .dangerous()
            .with_custom_certificate_verifier(AnyEd25519Key::new())
            .with_client_cert_resolver(Arc::new(AlwaysResolvesClientRawPublicKeys::new(me.key.clone())));
        config.resumption = Resumption::disabled();
        let name = ServerName::try_from("neochumbyd").expect("a valid name");
        let conn = ClientConnection::new(Arc::new(config), name).map_err(invalid)?;
        let mut s = StreamOwned::new(conn, tcp);
        while s.conn.is_handshaking() {
            s.conn.complete_io(&mut s.sock)?;
        }
        let (peer, code) = finish(&s.conn)?;
        Ok(Session { stream: Stream::Client(s), peer, code, rbuf: Vec::new(), version: 0 })
    }

    /// neochumbyd's side.
    pub fn accept(tcp: TcpStream, me: &Identity) -> io::Result<Session> {
        let mut config = ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(invalid)?
            .with_client_cert_verifier(AnyEd25519Key::new())
            .with_cert_resolver(Arc::new(AlwaysResolvesServerRawPublicKeys::new(me.key.clone())));
        config.send_tls13_tickets = 0;
        let conn = ServerConnection::new(Arc::new(config)).map_err(invalid)?;
        let mut s = StreamOwned::new(conn, tcp);
        while s.conn.is_handshaking() {
            s.conn.complete_io(&mut s.sock)?;
        }
        let (peer, code) = finish(&s.conn)?;
        Ok(Session { stream: Stream::Server(s), peer, code, rbuf: Vec::new(), version: 0 })
    }

    /// The other side's public key, proven in the handshake.
    pub fn peer(&self) -> &PublicKey {
        &self.peer
    }

    /// Six digits both sides derive from this handshake: equal on both
    /// screens only if no one sits between them.
    pub fn code(&self) -> String {
        self.code.clone()
    }

    pub fn tcp(&self) -> &TcpStream {
        match &self.stream {
            Stream::Client(s) => &s.sock,
            Stream::Server(s) => &s.sock,
        }
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
        let mut f = Vec::with_capacity(4 + body.len());
        f.extend_from_slice(&(body.len() as u32).to_be_bytes());
        f.extend_from_slice(body);
        match &mut self.stream {
            Stream::Client(s) => s.write_all(&f).and_then(|_| s.flush()),
            Stream::Server(s) => s.write_all(&f).and_then(|_| s.flush()),
        }
    }

    pub fn recv(&mut self) -> io::Result<Msg> {
        self.tcp().set_read_timeout(None)?;
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
        self.tcp().set_read_timeout(Some(t))?;
        match self.fill() {
            Ok(()) => self.take(),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// A whole message off the front of what has come, if there is one.
    fn take(&mut self) -> io::Result<Option<Msg>> {
        let Some(len) = self.rbuf.first_chunk::<4>() else { return Ok(None) };
        let n = u32::from_be_bytes(*len) as usize;
        if n > MAX_MSG {
            return Err(invalid("message too long"));
        }
        if self.rbuf.len() < 4 + n {
            return Ok(None);
        }
        let m = Msg::decode(&self.rbuf[4..4 + n]);
        self.rbuf.drain(..4 + n);
        m.map(Some)
    }

    /// Reads what there is (at least something) onto `rbuf`.
    fn fill(&mut self) -> io::Result<()> {
        let mut chunk = [0u8; 16384];
        let r: &mut dyn Read = match &mut self.stream {
            Stream::Client(s) => s,
            Stream::Server(s) => s,
        };
        match r.read(&mut chunk)? {
            0 => Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                self.rbuf.extend_from_slice(&chunk[..n]);
                Ok(())
            }
        }
    }
}

fn finish<D>(conn: &rustls::ConnectionCommon<D>) -> io::Result<(PublicKey, String)> {
    let spki = conn.peer_certificates().and_then(|c| c.first()).ok_or_else(|| invalid("no peer key"))?;
    let peer = ed25519_key(spki.as_ref()).map_err(invalid)?;
    let code = conn.export_keying_material([0u8; 4], PAIRING_LABEL, None).map_err(invalid)?;
    Ok((peer, six_digits(code)))
}
