//! The Chumbys' connections (neochumby-proto), one thread each.
//!
//! A paired Chumby says Hello and is welcomed, then may open an app's
//! companion (companion.rs). An unknown one may ask to pair: its request
//! waits in pending/ until approved or refused (`neochumbyd approve|refuse
//! <code>`), or ten minutes pass.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use neochumby_proto::{Identity, Msg, Session, valid_name};

use crate::companion;
use crate::state::State;

pub struct Api {
    pub state: State,
    pub identity: Identity,
    pub nfs_port: u16,
    /// The app tree, for the apps' companions.
    pub tree: PathBuf,
    /// neochumbyd's state directory, where companions keep theirs.
    pub state_dir: PathBuf,
}

pub fn serve(listener: TcpListener, api: Arc<Api>) {
    for tcp in listener.incoming().flatten() {
        let api = api.clone();
        std::thread::spawn(move || {
            let addr = tcp.peer_addr().map(|a| a.to_string()).unwrap_or_default();
            if let Err(e) = handle(&api, tcp) {
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

fn handle(api: &Api, tcp: TcpStream) -> io::Result<()> {
    let addr = tcp.peer_addr()?;
    keepalive(&tcp);
    // The handshake and the first message, or give up on it; and a write
    // the Chumby is not taking for this long means it is gone.
    tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut s = Session::accept(tcp, &api.identity)?;
    match s.recv()? {
        Msg::Hello { .. } => {
            let Some(name) = api.state.paired(s.peer()) else {
                return s.send(&Msg::Unpaired);
            };
            s.send(&Msg::Welcome)?;
            s.send(&Msg::Config(api.state.config(&name)))?;
            match s.recv() {
                Ok(Msg::Open { app }) => companion::run(&api.tree, &api.state_dir, &name, &app, s),
                // A Chumby that came only for its settings hangs up.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
                r => r.map(|_| ()),
            }
        }
        Msg::Pair { name } if valid_name(&name) => {
            let code = s.code();
            api.state.request(&code, &name, s.peer(), addr)?;
            eprintln!(
                "neochumbyd: {name} at {} asks to pair: if it shows {code}, run `neochumbyd approve {code}`",
                addr.ip()
            );
            let deadline = Instant::now() + Duration::from_secs(600);
            loop {
                std::thread::sleep(Duration::from_secs(1));
                match api.state.decision(&code, s.peer()) {
                    Some(true) => {
                        eprintln!("neochumbyd: {name} paired");
                        return s.send(&Msg::Paired { nfs_port: api.nfs_port, path: "/".into() });
                    }
                    Some(false) => return s.send(&Msg::Refused),
                    None if Instant::now() > deadline => {
                        api.state.refuse(&code)?;
                        return s.send(&Msg::Refused);
                    }
                    None => {}
                }
            }
        }
        _ => s.send(&Msg::Refused),
    }
}
