//! Chumbys on a serial line: an emulated Chumby has no network, and its
//! emulator connects the Chumby's second UART to the Unix socket
//! `--listen-unix PATH` instead (qemu-chumby's `--link`). Each connection
//! there is one Chumby's line (neochumby_proto::mux), and each channel the
//! Chumby opens on it is handled as a TCP connection is (api.rs): the same
//! handshake, the same checks of its key, and the same
//! share of the connections, the line counting as one address.
//!
//! The socket reaches what the API port does, so it is made only by the
//! one asking for it, where they say: anyone who can connect to it can ask
//! to pair, as anyone on the network can.

use std::io;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Arc;

use neochumby_proto::mux::Line;

use crate::api::{self, Api, Limits, Origin};

/// Listens at `path`, taking the place of a socket a daemon before this one
/// left there (but nothing else).
pub fn listen(path: &Path) -> io::Result<UnixListener> {
    if let Ok(m) = std::fs::symlink_metadata(path)
        && m.file_type().is_socket()
    {
        std::fs::remove_file(path)?;
    }
    UnixListener::bind(path)
}

/// Takes lines until the listener fails, and serves each in a thread.
pub fn serve(listener: UnixListener, api: Arc<Api>, limits: Arc<Limits>) {
    for (n, sock) in (1u64..).zip(listener.incoming()) {
        let sock = match sock {
            Ok(s) => s,
            Err(e) => {
                eprintln!("neochumbyd: a serial line: {e}");
                continue;
            }
        };
        let line = match sock.try_clone() {
            Ok(r) => Line::server(r, sock, move |e| eprintln!("neochumbyd: serial line {n}: {e}")),
            Err(e) => {
                eprintln!("neochumbyd: serial line {n}: {e}");
                continue;
            }
        };
        eprintln!("neochumbyd: serial line {n} connected");
        let (api, limits) = (api.clone(), limits.clone());
        std::thread::spawn(move || {
            let origin = Origin::Line(n);
            while let Some(chan) = line.accept() {
                let Some(mut slot) = limits.admit(origin.clone()) else {
                    eprintln!("neochumbyd: serial line {n}: too many connections; dropped");
                    continue;
                };
                let api = api.clone();
                std::thread::spawn(move || {
                    if let Err(e) = api::handle(&api, chan, &mut slot) {
                        eprintln!("neochumbyd: serial line {n}: {e}");
                    }
                });
            }
            eprintln!("neochumbyd: serial line {n}: {}", line.gone().unwrap_or_default());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;
    use neochumby_proto::{Identity, Msg, Session};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    /// A registered Chumby on a serial line is welcomed, as over TCP.
    #[test]
    fn a_chumby_connects_over_a_line() {
        let d = std::env::temp_dir().join(format!("neochumbyd-line-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let state = State::open(d.clone()).unwrap();
        let identity = state.identity().unwrap();
        let server_key = identity.public();
        let api = Arc::new(Api {
            state,
            identity,
            tree: d.join("tree"),
            state_dir: d.clone(),
            hashes: Default::default(),
            links: Default::default(),
            confine: crate::confine::Confine::Refused("not needed here".into()),
        });
        let path = d.join("line.sock");
        let l = listen(&path).unwrap();
        std::thread::spawn({
            let api = api.clone();
            move || serve(l, api, Default::default())
        });
        let sock = UnixStream::connect(&path).unwrap();
        let line = Line::chumby(sock.try_clone().unwrap(), sock);
        let wait = Duration::from_secs(10);

        let chumby = Identity::generate().unwrap();
        api.state.add("den", &chumby.public(), false).unwrap();
        let mut s = Session::connect(line.open(wait).unwrap(), &chumby, &server_key).unwrap();
        s.send(&Msg::Hello { name: "den".into() }).unwrap();
        assert_eq!(s.recv().unwrap(), Msg::Welcome);
        // Another key, not registered, is told so.
        let mut u = Session::connect(line.open(wait).unwrap(), &Identity::generate().unwrap(), &server_key).unwrap();
        u.send(&Msg::Hello { name: "den".into() }).unwrap();
        assert_eq!(u.recv().unwrap(), Msg::Unpaired);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
