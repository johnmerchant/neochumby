//! The Chumbys' connections (neochumby-proto), one thread each.
//!
//! A paired Chumby says Hello and is welcomed, is sent its settings and
//! the protocol's version (neochumby_proto::version), then may open an app's
//! companion (companion.rs), make this its developer link (dev.rs), or ask
//! for sync rounds (sync.rs). An unknown one may ask to pair: its request
//! waits in pending/ until approved or refused (`neochumbyd approve|refuse
//! CODE`), or ten minutes pass.
//!
//! Until a connection is known to be a paired Chumby's, it is held to a
//! deadline for its first words, the handshake among them, and to a share
//! of the connections: so that anyone who can reach the port can't hold
//! every thread. A few of that share are kept for addresses a paired
//! Chumby has connected from, so that the rest filling up doesn't shut
//! out the Chumbys; and a paired Chumby, which may not be well behaved
//! either, has a share of its own.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use neochumby_proto::{Identity, Msg, Unverified, valid_name, version};

use crate::state::{PENDING_TTL, State};
use crate::{companion, dev, sync};

pub struct Api {
    pub state: State,
    pub identity: Identity,
    pub nfs_port: u16,
    /// The app tree, for the apps' companions.
    pub tree: PathBuf,
    /// neochumbyd's state directory, where companions keep theirs.
    pub state_dir: PathBuf,
    /// The tree's hashes, for sync rounds.
    pub hashes: neochumby_sync::serve::Hashes,
    /// The Chumbys' developer links.
    pub links: dev::Links,
    /// How apps' programs are run: sandboxed, or not at all.
    pub confine: crate::confine::Confine,
}

/// The most connections at once; and of them, the most not yet known to
/// be a paired Chumby's, overall and from one address. A Chumby keeps a
/// few (its settings, developer link, sync, an app's companion).
const MAX_CONNECTIONS: usize = 256;
const MAX_UNKNOWN: usize = 32;
const MAX_UNKNOWN_PER_ADDR: usize = 8;
/// Unknown connections beyond MAX_UNKNOWN, taken only from an address a
/// paired Chumby has connected from.
const RESERVED: usize = 8;
/// The most addresses remembered as a paired Chumby's.
const MAX_PAIRED_ADDRS: usize = 1024;
/// The most connections one paired Chumby holds at once.
const MAX_PER_CHUMBY: usize = 32;
/// From a connection's start to the end of its first words: the handshake
/// and Hello, then the message after Config; or a request to pair.
#[cfg(not(test))]
const FIRST_WORDS: Duration = Duration::from_secs(30);
#[cfg(test)]
const FIRST_WORDS: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Limits(Mutex<Counts>);

#[derive(Default)]
struct Counts {
    all: usize,
    unknown: usize,
    by_addr: HashMap<IpAddr, usize>,
    /// Addresses paired Chumbys have connected from.
    paired_addrs: HashSet<IpAddr>,
    /// Each paired Chumby's connections, by name.
    by_chumby: HashMap<String, usize>,
}

/// One connection's place in the [`Limits`], given back when it is dropped.
struct Slot {
    limits: Arc<Limits>,
    ip: IpAddr,
    unknown: bool,
    /// The paired Chumby it is known to be.
    chumby: Option<String>,
}

impl Limits {
    fn admit(self: &Arc<Self>, ip: IpAddr) -> Option<Slot> {
        let mut c = self.0.lock().unwrap();
        let from = c.by_addr.get(&ip).copied().unwrap_or(0);
        let share = if c.paired_addrs.contains(&ip) { MAX_UNKNOWN + RESERVED } else { MAX_UNKNOWN };
        if c.all >= MAX_CONNECTIONS || c.unknown >= share || from >= MAX_UNKNOWN_PER_ADDR {
            return None;
        }
        c.all += 1;
        c.unknown += 1;
        c.by_addr.insert(ip, from + 1);
        Some(Slot { limits: self.clone(), ip, unknown: true, chumby: None })
    }
}

impl Counts {
    fn known(&mut self, ip: IpAddr) {
        self.unknown -= 1;
        if let Some(n) = self.by_addr.get_mut(&ip) {
            *n -= 1;
            if *n == 0 {
                self.by_addr.remove(&ip);
            }
        }
    }
}

impl Slot {
    /// The connection is paired Chumby `name`'s: it no longer counts as
    /// unknown, but as one of `name`'s, of which it may hold only so many
    /// (false if it already does).
    fn known(&mut self, name: &str) -> bool {
        let mut c = self.limits.0.lock().unwrap();
        if std::mem::take(&mut self.unknown) {
            c.known(self.ip);
            if c.paired_addrs.len() >= MAX_PAIRED_ADDRS {
                c.paired_addrs.clear();
            }
            c.paired_addrs.insert(self.ip);
        }
        let n = c.by_chumby.entry(name.to_string()).or_default();
        if *n >= MAX_PER_CHUMBY {
            return false;
        }
        *n += 1;
        self.chumby = Some(name.to_string());
        true
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut c = self.limits.0.lock().unwrap();
        c.all -= 1;
        if self.unknown {
            c.known(self.ip);
        }
        if let Some(name) = &self.chumby
            && let Some(n) = c.by_chumby.get_mut(name)
        {
            *n -= 1;
            if *n == 0 {
                c.by_chumby.remove(name);
            }
        }
    }
}

pub fn serve(listener: TcpListener, api: Arc<Api>) {
    let limits = Arc::new(Limits::default());
    for tcp in listener.incoming().flatten() {
        let Ok(addr) = tcp.peer_addr() else { continue };
        let Some(mut slot) = limits.admit(addr.ip()) else {
            eprintln!("neochumbyd: {addr}: too many connections; dropped");
            continue;
        };
        let api = api.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle(&api, tcp, &mut slot) {
                eprintln!("neochumbyd: {addr}: {e}");
            }
        });
    }
}

/// TCP keepalive, so a Chumby that went without a word (rebooted, unplugged)
/// is noticed within about 25 s even when nothing is being sent, and what
/// was running for it stops.
fn keepalive(tcp: &TcpStream) {
    // The timings apply on Linux; elsewhere, the system's.
    neochumby_sys::net::keepalive(tcp, 10, 5, 3);
}

fn handle(api: &Api, tcp: TcpStream, slot: &mut Slot) -> io::Result<()> {
    let addr = tcp.peer_addr()?;
    keepalive(&tcp);
    // The first words by their deadline, the whole handshake within it
    // however slowly it is sent, or give up on it; and a write the Chumby
    // is not taking for this long means it is gone.
    let deadline = Instant::now() + FIRST_WORDS;
    tcp.set_read_timeout(Some(FIRST_WORDS))?;
    tcp.set_write_timeout(Some(FIRST_WORDS))?;
    let mut u = Unverified::accept_by(tcp, &api.identity, deadline)?;
    match u.recv_by(deadline)? {
        Msg::Hello { .. } => {
            let Some(name) = api.state.paired(u.peer()) else {
                return u.send(&Msg::Unpaired);
            };
            let mut s = u.verify(|k| api.state.paired(k).as_ref() == Some(&name))?;
            if !slot.known(&name) {
                return Err(io::Error::other(format!("{name} has {MAX_PER_CHUMBY} connections already")));
            }
            s.send(&Msg::Welcome)?;
            s.send(&version::config(&api.state.settings(&name)))?;
            match version::after_config(&mut s, deadline) {
                Ok(Msg::Open { app }) => companion::run(&api.tree, &api.state_dir, &api.confine, &name, &app, s),
                Ok(m @ Msg::SyncHave { .. }) => sync::serve(api, &name, s, m),
                Ok(Msg::Dev) => dev::run(api, &name, s),
                // A Chumby that came only for its settings hangs up.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
                r => r.map(|_| ()),
            }
        }
        Msg::Pair { name } if valid_name(&name) => {
            let code = u.code();
            let id = match api.state.request(&code, &name, u.peer(), addr) {
                Ok(id) => id,
                Err(e) => {
                    let _ = u.send(&Msg::Refused);
                    return Err(e);
                }
            };
            eprintln!(
                "neochumbyd: {name} at {} asks to pair, showing {code}: if its screen shows the same, run `neochumbyd approve CODE` with the code from its screen",
                addr.ip()
            );
            let deadline = Instant::now() + PENDING_TTL;
            loop {
                std::thread::sleep(Duration::from_secs(1));
                match api.state.decision(&id, u.peer()) {
                    Some(true) => {
                        eprintln!("neochumbyd: {name} paired");
                        return u.send(&Msg::Paired { nfs_port: api.nfs_port, path: "/".into() });
                    }
                    Some(false) => return u.send(&Msg::Refused),
                    None if Instant::now() > deadline => {
                        api.state.withdraw(&id)?;
                        return u.send(&Msg::Refused);
                    }
                    None => {}
                }
            }
        }
        _ => u.send(&Msg::Refused),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neochumby_proto::Session;

    #[test]
    fn limits() {
        let limits = Arc::new(Limits::default());
        let (a, b): (IpAddr, IpAddr) = ("192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap());
        let mut from_a: Vec<Slot> = (0..MAX_UNKNOWN_PER_ADDR).map(|_| limits.admit(a).unwrap()).collect();
        assert!(limits.admit(a).is_none(), "too many unknown from one address");
        let from_b = limits.admit(b).unwrap();
        // Known, it makes room for another from its address.
        assert!(from_a[0].known("den"));
        let more = limits.admit(a).unwrap();
        drop((from_a, from_b, more));
        let c = limits.0.lock().unwrap();
        assert_eq!((c.all, c.unknown, c.by_addr.len()), (0, 0, 0));
    }

    /// A Chumby asks to pair over a real connection, and is approved by
    /// its code.
    #[test]
    fn pairing() {
        let d = std::env::temp_dir().join(format!("neochumbyd-api-pairing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let state = State::open(d.clone()).unwrap();
        let identity = state.identity().unwrap();
        let server_key = identity.public();
        let api = Arc::new(Api {
            state,
            identity,
            nfs_port: 2049,
            tree: d.join("tree"),
            state_dir: d.clone(),
            hashes: Default::default(),
            links: Default::default(),
            confine: crate::confine::Confine::Refused("not needed here".into()),
        });
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn({
            let api = api.clone();
            move || serve(l, api)
        });

        let chumby = Identity::generate().unwrap();
        // Unknown, its Hello is answered Unpaired, and nothing else.
        let mut u = Unverified::connect(TcpStream::connect(addr).unwrap(), &chumby).unwrap();
        u.send(&Msg::Hello { name: "den".into() }).unwrap();
        assert_eq!(u.recv().unwrap(), Msg::Unpaired);

        let mut u = Unverified::connect(TcpStream::connect(addr).unwrap(), &chumby).unwrap();
        u.send(&Msg::Pair { name: "den".into() }).unwrap();
        let code = u.code();
        while api.state.pending().unwrap().is_empty() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(api.state.pending().unwrap()[0].code, code);
        api.state.approve(&code, false).unwrap();
        assert_eq!(u.recv().unwrap(), Msg::Paired { nfs_port: 2049, path: "/".into() });
        assert_eq!(u.peer(), &server_key);

        // Paired, it is welcomed.
        let mut s = Session::connect(TcpStream::connect(addr).unwrap(), &chumby, &server_key).unwrap();
        s.send(&Msg::Hello { name: "den".into() }).unwrap();
        assert_eq!(s.recv().unwrap(), Msg::Welcome);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A neochumbyd on a port of its own, in a fresh state directory.
    fn server(test: &str) -> (Arc<Api>, std::net::SocketAddr, PathBuf) {
        let d = std::env::temp_dir().join(format!("neochumbyd-api-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let state = State::open(d.clone()).unwrap();
        let identity = state.identity().unwrap();
        let api = Arc::new(Api {
            state,
            identity,
            nfs_port: 2049,
            tree: d.join("tree"),
            state_dir: d.clone(),
            hashes: Default::default(),
            links: Default::default(),
            confine: crate::confine::Confine::Refused("not needed here".into()),
        });
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn({
            let api = api.clone();
            move || serve(l, api)
        });
        (api, addr, d)
    }

    /// Waits for the server to hang up, at most `limit`: how long it took.
    fn hung_up(mut tcp: &TcpStream, limit: Duration) -> Duration {
        use std::io::Read;
        let t = Instant::now();
        tcp.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let mut b = [0u8; 4096];
        while t.elapsed() < limit {
            match tcp.read(&mut b) {
                Ok(0) => return t.elapsed(),
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(_) => return t.elapsed(),
                Ok(_) => {}
            }
        }
        panic!("still connected after {limit:?}");
    }

    /// A client that completes the handshake and then says nothing is
    /// dropped by FIRST_WORDS, not held for ever (#70, finding 2).
    #[test]
    fn silent_after_the_handshake_is_dropped() {
        let (_api, addr, d) = server("silent");
        let u = Unverified::connect(TcpStream::connect(addr).unwrap(), &Identity::generate().unwrap()).unwrap();
        let took = hung_up(u.tcp(), FIRST_WORDS + Duration::from_secs(10));
        assert!(took <= FIRST_WORDS + Duration::from_secs(2), "{took:?}");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A paired Chumby that takes its settings and then says nothing is
    /// dropped by the same deadline (the second message of #70's finding 2).
    #[test]
    fn silent_after_config_is_dropped() {
        let (api, addr, d) = server("silent-config");
        let chumby = Identity::generate().unwrap();
        let code = "0100-0000-0000-0000-0000-0000-0000-0000";
        api.state.request(code, "den", &chumby.public(), "192.0.2.7:1".parse().unwrap()).unwrap();
        api.state.approve(code, false).unwrap();
        let mut s = Session::connect(TcpStream::connect(addr).unwrap(), &chumby, &api.identity.public()).unwrap();
        version::hello(&mut s, "den".into(), Duration::from_secs(5)).unwrap();
        let took = hung_up(s.tcp(), FIRST_WORDS + Duration::from_secs(10));
        assert!(took <= FIRST_WORDS + Duration::from_secs(2), "{took:?}");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A handshake sent a byte at a time, each well within a read's
    /// timeout, is still held to FIRST_WORDS as a whole.
    #[test]
    fn dribbled_handshake_is_dropped() {
        use std::io::Write;
        let (_api, addr, d) = server("dribble");
        let mut tcp = TcpStream::connect(addr).unwrap();
        // A handshake record of 512 bytes, then its body a byte at a time.
        tcp.write_all(&[0x16, 0x03, 0x01, 0x02, 0x00, 0x01]).unwrap();
        let t = Instant::now();
        let limit = FIRST_WORDS + Duration::from_secs(10);
        let mut gone = false;
        while t.elapsed() < limit && !gone {
            std::thread::sleep(Duration::from_millis(200));
            gone = tcp.write_all(&[0]).is_err() || hung_up_within(&tcp, Duration::from_millis(50));
        }
        assert!(gone, "still connected after {limit:?}");
        assert!(t.elapsed() <= FIRST_WORDS + Duration::from_secs(2), "{:?}", t.elapsed());
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Whether the server has hung up, waiting at most `t` to see.
    fn hung_up_within(mut tcp: &TcpStream, t: Duration) -> bool {
        use std::io::Read;
        tcp.set_read_timeout(Some(t)).unwrap();
        let mut b = [0u8; 64];
        match tcp.read(&mut b) {
            Ok(0) => true,
            Ok(_) => false,
            Err(e) => !matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut),
        }
    }

    /// With the unknown share full, an address a paired Chumby connected
    /// from still gets in; and a Chumby holds only its own share.
    #[test]
    fn reserved_and_per_chumby() {
        let limits = Arc::new(Limits::default());
        let chumby: IpAddr = "192.0.2.9".parse().unwrap();
        let mut s = limits.admit(chumby).unwrap();
        assert!(s.known("den"));
        let others: Vec<Slot> = (0..MAX_UNKNOWN)
            .map(|i| limits.admit(IpAddr::from([198, 51, 100, (i / MAX_UNKNOWN_PER_ADDR) as u8])).unwrap())
            .collect();
        assert!(limits.admit("203.0.113.1".parse().unwrap()).is_none(), "the unknown share is full");
        let mut again = limits.admit(chumby).expect("a paired Chumby's address has slots kept for it");
        assert!(again.known("den"));
        let mut den: Vec<Slot> = vec![s, again];
        while den.len() < MAX_PER_CHUMBY {
            let mut s = limits.admit(chumby).unwrap();
            assert!(s.known("den"));
            den.push(s);
        }
        let mut one_more = limits.admit(chumby).unwrap();
        assert!(!one_more.known("den"), "den holds its share");
        drop((others, den, one_more));
        let c = limits.0.lock().unwrap();
        assert_eq!((c.all, c.unknown, c.by_addr.len(), c.by_chumby.len()), (0, 0, 0, 0));
    }
}
