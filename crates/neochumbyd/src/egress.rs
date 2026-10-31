//! Where a wasm companion's GETs may go (wasm.rs): only to a public
//! address. An approved host is a DNS name, and whoever answers for it
//! decides where it points; so a name that resolves to an address on the
//! server's own network, or the server itself, is refused, whatever the
//! owner approved.
//!
//! Refused ([`refused`]): unspecified, loopback, private (RFC 1918 and
//! IPv6's unique local fc00::/7 and old site-local fec0::/10), link-local,
//! CGNAT (100.64.0.0/10), multicast and broadcast, and the ranges reserved
//! for documentation, benchmarking, protocol assignments and the future.
//! An IPv6 address that carries an IPv4 one (mapped, compatible, NAT64's
//! 64:ff9b::/96 and 6to4's 2002::/16) is judged by that IPv4 address.
//!
//! Checked twice:
//! 1. [`Resolver`]: when a name is looked up, if any of its addresses is
//!    refused, the GET is. Each lookup is checked, so a name that changes
//!    its answer (DNS rebinding) is caught at the lookup it changes in.
//! 2. [`Connector`]: the address the socket is connected to, from the
//!    socket itself, before TLS, so a connection that ends up somewhere
//!    refused is closed whatever the lookup said.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver as UreqResolver};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector as UreqConnector, LazyBuffers, NextTimeout, ShutdownHandle, Transport,
};

/// Whether a companion may not reach `ip`.
pub fn refused(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => refused_v4(v4),
        IpAddr::V6(v6) => refused_v6(v6),
    }
}

fn refused_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0                                  // "this network", 0/8
        || (a == 100 && (64..128).contains(&b))    // CGNAT, 100.64/10
        || (a == 192 && b == 0 && c == 0)          // protocol assignments, 192.0.0/24
        || (a == 198 && (18..20).contains(&b))     // benchmarking, 198.18/15
        || a >= 240 // reserved, 240/4
}

fn refused_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // An IPv4 address carried in it: mapped (::ffff:a.b.c.d), compatible
    // (::a.b.c.d), NAT64 (64:ff9b::a.b.c.d) and 6to4 (2002:aabb:ccdd::).
    let v4 = |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
    if let Some(v4) = ip.to_ipv4_mapped() {
        return refused_v4(v4);
    }
    if s[..6] == [0; 6] && !ip.is_unspecified() && !ip.is_loopback() {
        return refused_v4(v4(s[6], s[7]));
    }
    if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return refused_v4(v4(s[6], s[7]));
    }
    if s[0] == 0x2002 {
        return refused_v4(v4(s[1], s[2]));
    }
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00   // unique local, fc00::/7
        || (s[0] & 0xffc0) == 0xfe80   // link-local, fe80::/10
        || (s[0] & 0xffc0) == 0xfec0   // site-local, fec0::/10
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation, 2001:db8::/32
        || (s[0] == 0x2001 && s[1] < 0x0200) // IETF protocol assignments, 2001::/23
        || (s[0] == 0x0100 && s[1..4] == [0, 0, 0]) // discard-only, 100::/64
}

/// What the checks allow: [`refused`]'s rule, except that tests may also
/// reach 127.0.0.1, where their own HTTPS server is.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rule {
    #[cfg(test)]
    pub loopback: bool,
}

impl Rule {
    pub fn allows(&self, ip: IpAddr) -> bool {
        #[cfg(test)]
        if self.loopback && ip == IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return true;
        }
        !refused(ip)
    }
}

/// `addrs`, a name's addresses, if none is refused.
pub fn check_resolved(rule: Rule, addrs: &[SocketAddr]) -> io::Result<()> {
    match addrs.iter().find(|a| !rule.allows(a.ip())) {
        Some(a) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("it resolves to {}, an address a companion may not reach", a.ip()),
        )),
        None => Ok(()),
    }
}

/// Connects to the first of `addrs` that answers, each checked by `before`
/// first, and the socket's peer then checked by `after`: what it is
/// connected to, whatever the addresses said.
pub fn connect_checked(
    addrs: &[SocketAddr],
    timeout: Duration,
    before: impl Fn(IpAddr) -> bool,
    after: impl Fn(IpAddr) -> bool,
) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address to connect to");
    for a in addrs {
        if !before(a.ip()) {
            last = io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is refused", a.ip()));
            continue;
        }
        match TcpStream::connect_timeout(a, timeout) {
            Ok(s) => {
                let peer = s.peer_addr()?;
                if !after(peer.ip()) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("connected to {}, an address a companion may not reach", peer.ip()),
                    ));
                }
                return Ok(s);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// ureq's resolver, its every answer checked.
#[derive(Debug)]
pub struct Resolver<R = DefaultResolver> {
    pub inner: R,
    pub rule: Rule,
}

impl<R: UreqResolver> UreqResolver for Resolver<R> {
    fn resolve(&self, uri: &Uri, config: &Config, timeout: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let addrs = self.inner.resolve(uri, config, timeout)?;
        check_resolved(self.rule, &addrs)?;
        Ok(addrs)
    }
}

/// In place of ureq's TcpConnector: a TCP connection to the resolved
/// addresses, its peer checked ([`connect_checked`]).
#[derive(Debug)]
pub struct Connector {
    pub rule: Rule,
    pub timeout: Duration,
}

impl<In: Transport> UreqConnector<In> for Connector {
    type Out = Tcp;

    fn connect(&self, details: &ConnectionDetails, _chained: Option<In>) -> Result<Option<Tcp>, ureq::Error> {
        let rule = self.rule;
        let budget = self.timeout.min(*details.timeout.after);
        let stream = connect_checked(&details.addrs, budget, |ip| rule.allows(ip), |ip| rule.allows(ip))?;
        stream.set_nodelay(details.config.no_delay())?;
        let stream = Arc::new(stream);
        let weak = Arc::downgrade(&stream) as std::sync::Weak<dyn ShutdownHandle>;
        (details.register_shutdown)(weak)?;
        let config = details.config;
        let buffers = LazyBuffers::new(config.input_buffer_size(), config.output_buffer_size());
        Ok(Some(Tcp { stream, buffers }))
    }
}

/// A checked TCP connection, as ureq's transport.
pub struct Tcp {
    stream: Arc<TcpStream>,
    buffers: LazyBuffers,
}

impl fmt::Debug for Tcp {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Tcp").field("peer", &self.stream.peer_addr().ok()).finish()
    }
}

/// `timeout` as a socket's: None for none; a timeout already spent is an
/// error.
fn socket_timeout(timeout: NextTimeout) -> Result<Option<Duration>, ureq::Error> {
    if timeout.after.is_zero() {
        return Err(ureq::Error::Timeout(timeout.reason));
    }
    Ok(timeout.not_zero().map(|d| *d))
}

fn io_error(e: io::Error, timeout: NextTimeout) -> ureq::Error {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ureq::Error::Timeout(timeout.reason),
        _ => e.into(),
    }
}

impl Transport for Tcp {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        let timeout = timeout.for_write();
        self.stream.set_write_timeout(socket_timeout(timeout)?)?;
        let output = &self.buffers.output()[..amount];
        (&*self.stream).write_all(output).map_err(|e| io_error(e, timeout))
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let timeout = timeout.for_read();
        self.stream.set_read_timeout(socket_timeout(timeout)?)?;
        let started = Instant::now();
        let n = loop {
            match (&*self.stream).read(self.buffers.input_append_buf()) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted && started.elapsed() < *timeout.after => {}
                Err(e) => return Err(io_error(e, timeout)),
            }
        };
        self.buffers.input_appended(n);
        Ok(n > 0)
    }

    fn is_open(&mut self) -> bool {
        // Open if reading would block: nothing unasked for, and not closed.
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let mut b = [0];
        let open = matches!((&*self.stream).read(&mut b), Err(e) if e.kind() == io::ErrorKind::WouldBlock);
        open && self.stream.set_nonblocking(false).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn what_is_refused() {
        for a in [
            "0.0.0.0",
            "0.1.2.3",
            "127.0.0.1",
            "127.255.0.9",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "224.0.0.1",
            "239.255.255.250",
            "255.255.255.255",
            "240.0.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.7",
            "203.0.113.9",
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::192.168.0.1",
            "64:ff9b::a9fe:a9fe",
            "2002:c0a8:0101::1",
            "100::1",
        ] {
            assert!(refused(ip(a)), "{a} should be refused");
        }
        for a in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.215.14",
            "100.63.255.255",
            "100.128.0.1",
            "172.32.0.1",
            "192.0.1.1",
            "198.20.0.1",
            "2606:4700:4700::1111",
            "2a00:1450:4009:80b::200e",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:0808:0808::1",
        ] {
            assert!(!refused(ip(a)), "{a} should be allowed");
        }
    }

    #[test]
    fn a_name_with_any_refused_address_is() {
        let rule = Rule::default();
        let public: SocketAddr = "1.1.1.1:443".parse().unwrap();
        let lan: SocketAddr = "192.168.1.2:443".parse().unwrap();
        assert!(check_resolved(rule, &[public]).is_ok());
        assert!(check_resolved(rule, &[public, lan]).is_err());
        assert!(check_resolved(rule, &[lan]).is_err());
        // Tests' own loopback allowance, which a companion never has.
        let local: SocketAddr = "127.0.0.1:443".parse().unwrap();
        assert!(check_resolved(rule, &[local]).is_err());
        assert!(check_resolved(Rule { loopback: true }, &[local]).is_ok());
    }

    /// A name that answers with a public address, then (DNS rebinding)
    /// with a private one: each lookup is checked, so the second fails.
    #[derive(Debug, Default)]
    struct Rebinding(AtomicUsize);

    impl UreqResolver for Rebinding {
        fn resolve(&self, _: &Uri, _: &Config, _: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
            let mut v = self.empty();
            let a = if self.0.fetch_add(1, Ordering::SeqCst) == 0 { "1.1.1.1:443" } else { "10.0.0.7:443" };
            v.push(a.parse().unwrap());
            Ok(v)
        }
    }

    #[test]
    fn a_rebinding_name_is_caught_at_its_next_lookup() {
        let r = Resolver { inner: Rebinding::default(), rule: Rule::default() };
        let uri: Uri = "https://weather.example.com/".parse().unwrap();
        let config = Config::default();
        assert!(r.resolve(&uri, &config, NextTimeout::default()).is_ok());
        let e = r.resolve(&uri, &config, NextTimeout::default()).unwrap_err();
        assert!(e.to_string().contains("10.0.0.7"), "{e}");
    }

    /// The lookup said yes, but the socket is connected to a refused
    /// address: the connection is refused.
    #[test]
    fn the_connected_address_is_checked() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        let t = Duration::from_secs(2);
        let e = connect_checked(&[a], t, |_| true, refused_not).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("connected to 127.0.0.1"), "{e}");
        // Refused before connecting.
        let e = connect_checked(&[a], t, |ip| !refused(ip), |_| true).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        // Allowed both times.
        assert!(connect_checked(&[a], t, |_| true, |_| true).is_ok());
    }

    fn refused_not(ip: IpAddr) -> bool {
        !refused(ip)
    }
}
