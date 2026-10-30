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
//! deadline for its first words, and to a share of the connections: so
//! that anyone who can reach the port can't hold every thread.

use std::collections::HashMap;
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
/// From a connection's start to the end of its first words: the handshake
/// and Hello, then the message after Config; or a request to pair.
const FIRST_WORDS: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Limits(Mutex<Counts>);

#[derive(Default)]
struct Counts {
    all: usize,
    unknown: usize,
    by_addr: HashMap<IpAddr, usize>,
}

/// One connection's place in the [`Limits`], given back when it is dropped.
struct Slot {
    limits: Arc<Limits>,
    ip: IpAddr,
    unknown: bool,
}

impl Limits {
    fn admit(self: &Arc<Self>, ip: IpAddr) -> Option<Slot> {
        let mut c = self.0.lock().unwrap();
        let from = c.by_addr.get(&ip).copied().unwrap_or(0);
        if c.all >= MAX_CONNECTIONS || c.unknown >= MAX_UNKNOWN || from >= MAX_UNKNOWN_PER_ADDR {
            return None;
        }
        c.all += 1;
        c.unknown += 1;
        c.by_addr.insert(ip, from + 1);
        Some(Slot { limits: self.clone(), ip, unknown: true })
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
    /// The connection is a paired Chumby's: it no longer counts as unknown.
    fn known(&mut self) {
        if std::mem::take(&mut self.unknown) {
            self.limits.0.lock().unwrap().known(self.ip);
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut c = self.limits.0.lock().unwrap();
        c.all -= 1;
        if self.unknown {
            c.known(self.ip);
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
    // The first words by their deadline (the handshake's reads each within
    // it too), or give up on it; and a write the Chumby is not taking for
    // this long means it is gone.
    let deadline = Instant::now() + FIRST_WORDS;
    tcp.set_read_timeout(Some(FIRST_WORDS))?;
    tcp.set_write_timeout(Some(FIRST_WORDS))?;
    let mut u = Unverified::accept(tcp, &api.identity)?;
    match u.recv_by(deadline)? {
        Msg::Hello { .. } => {
            let Some(name) = api.state.paired(u.peer()) else {
                return u.send(&Msg::Unpaired);
            };
            let mut s = u.verify(|k| api.state.paired(k).as_ref() == Some(&name))?;
            slot.known();
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
        from_a[0].known();
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
}
