//! An app's service, reached from in front: its own exec, or a frontend,
//! another app the owner approved to show it (an AirPlay screen of
//! someone else's, say). The service listens with [`listen`]; the other
//! end connects with [`connect`], naming the app. Each gets a
//! `UnixStream`, and what goes over it is theirs (AirPlay's is
//! `neochumby_airplay_proto::frontend`).
//!
//! The socket is a Unix one in the abstract namespace, which every jail
//! reaches (it has no file), named after the app: `neochumby-service-<app>`.
//! **The session makes it** ([`bind`]), for each app with a service, as it
//! starts and before it runs any app, and hands it to the service as
//! `NEOCHUMBY_LISTENER` (a descriptor) each time it starts it; so no app
//! can take another's name first. Both ends check who is at the other by
//! its uid, which the kernel vouches for:
//!
//! - **the service** takes only its own uid (its app's exec runs as the
//!   same user) and the uids the session lists in `NEOCHUMBY_CLIENTS`: its
//!   approved frontends'. Anyone else is turned away, and logged.
//! - **a frontend, or the app's exec,** takes only a socket the session
//!   made (root's, for a jailed app) or one of its own user: never another
//!   app's.
//!
//! The owner approves a frontend with the Chumby's settings
//! (`frontend_<app>=<frontend>`, the session's frontend.rs), and its
//! manifest says which app it's for (`frontend = "<app>"`). The approval
//! applies from the next session start; the session then tells the
//! frontend which app it is for (`NEOCHUMBY_FRONTEND`).
//!
//! Where jails are off (the emulator, a session not run as root) every app
//! is the same user, and nothing here can tell them apart.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd, RawFd};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};

use neochumby_sys::{fd, net, process};

/// The uids, comma-separated, of the frontends the owner approved for the
/// app a service belongs to.
pub const CLIENTS_ENV: &str = "NEOCHUMBY_CLIENTS";
/// For an approved frontend: the app it's for.
pub const FRONTEND_ENV: &str = "NEOCHUMBY_FRONTEND";
/// The listening socket the session made for a service, a descriptor.
pub const LISTENER_ENV: &str = "NEOCHUMBY_LISTENER";

/// The socket's name for `app`.
fn name(app: &str) -> io::Result<SocketAddr> {
    SocketAddr::from_abstract_name(format!("neochumby-service-{app}"))
}

/// For the session: `app`'s socket, bound and listening, before any app
/// runs. Fails with [`io::ErrorKind::AddrInUse`] if something holds the
/// name already.
pub fn bind(app: &str) -> io::Result<UnixListener> {
    UnixListener::bind_addr(&name(app)?)
}

/// A service listening for its exec and its frontends.
pub struct Listener {
    sock: UnixListener,
    app: String,
    /// The uids it takes besides its own.
    clients: Vec<u32>,
}

/// Listens as `app`'s service (its directory in the tree), on the socket
/// the session handed it, without waiting: [`Listener::accept`] returns at
/// once. Run by hand, without the session, it binds the name itself, and
/// fails with [`io::ErrorKind::AddrInUse`] while something else holds it.
pub fn listen(app: &str) -> io::Result<Listener> {
    let given = std::env::var(LISTENER_ENV).ok().and_then(|v| v.parse::<RawFd>().ok()).and_then(fd::handed_over);
    let clients = std::env::var(CLIENTS_ENV).map(|v| parse_uids(&v)).unwrap_or_default();
    listen_on(app, given, clients)
}

/// [`listen`], on `given` if there is one.
fn listen_on(app: &str, given: Option<OwnedFd>, clients: Vec<u32>) -> io::Result<Listener> {
    let sock = match given {
        Some(fd) => UnixListener::from(fd),
        None => bind(app)?,
    };
    sock.set_nonblocking(true)?;
    Ok(Listener { sock, app: app.into(), clients })
}

fn parse_uids(v: &str) -> Vec<u32> {
    v.split(',').filter_map(|u| u.trim().parse().ok()).collect()
}

impl Listener {
    /// The next end that has connected and may, if one has; others are
    /// closed, and logged. Its stream waits, as std's do.
    pub fn accept(&self) -> io::Result<Option<UnixStream>> {
        loop {
            let s = match self.sock.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            match net::peer_uid(s.as_fd()) {
                Ok(uid) if uid == process::uid() || self.clients.contains(&uid) => {
                    s.set_nonblocking(false)?;
                    return Ok(Some(s));
                }
                Ok(uid) => eprintln!(
                    "neochumby: {}'s service: turned away uid {uid}, which is neither this app's nor an approved frontend's",
                    self.app
                ),
                Err(e) => eprintln!("neochumby: {}'s service: {e}", self.app),
            }
        }
    }
}

impl AsFd for Listener {
    /// For waiting on it with others (neochumby_sys::fd::readable).
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.sock.as_fd()
    }
}

/// The app this one is an approved frontend for, if the session said so.
pub fn frontend_of() -> Option<String> {
    std::env::var(FRONTEND_ENV).ok().filter(|a| neochumby_proto::valid_name(a))
}

/// Connects to `app`'s service: this app's own, or the one it's an
/// approved frontend for. Fails with [`io::ErrorKind::PermissionDenied`]
/// if what answers isn't a socket the session made, nor this user's, or
/// with [`io::ErrorKind::ConnectionRefused`] while the service isn't
/// listening.
pub fn connect(app: &str) -> io::Result<UnixStream> {
    let s = UnixStream::connect_addr(&name(app)?)?;
    // Whoever made the listening socket: the session (root), or, run by
    // hand or with jails off, this user.
    let theirs = net::peer_uid(s.as_fd())?;
    if theirs != 0 && theirs != process::uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{app}'s socket is another app's (uid {theirs}), not the session's"),
        ));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    #[test]
    fn its_own_end_connects() {
        let app = format!("test-{}", std::process::id());
        let l = listen_on(&app, None, Vec::new()).unwrap();
        assert!(l.accept().unwrap().is_none(), "nothing yet");
        let mut c = connect(&app).unwrap();
        let mut s = l.accept().unwrap().expect("connected");
        c.write_all(b"hi").unwrap();
        let mut b = [0u8; 2];
        s.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"hi");
        drop(l);
        assert!(connect(&app).is_err(), "gone with its listener");
    }

    /// The session binds the name before any app runs: another app can't
    /// take it then, and the service listens on what it was handed.
    #[test]
    fn once_the_session_has_the_name_no_one_else_can_take_it() {
        let app = format!("taken-{}", std::process::id());
        let session = bind(&app).unwrap();
        // Another app, binding the name as its own.
        assert_eq!(bind(&app).err().map(|e| e.kind()), Some(io::ErrorKind::AddrInUse));
        assert_eq!(listen_on(&app, None, Vec::new()).err().map(|e| e.kind()), Some(io::ErrorKind::AddrInUse));
        // The service, on the session's socket.
        let l = listen_on(&app, Some(OwnedFd::from(session)), Vec::new()).unwrap();
        let _c = connect(&app).unwrap();
        assert!(l.accept().unwrap().is_some());
    }

    #[test]
    fn uids_from_the_session() {
        assert_eq!(parse_uids("20001,20004"), [20001, 20004]);
        assert_eq!(parse_uids(""), [] as [u32; 0]);
        assert_eq!(parse_uids("x, 7"), [7]);
    }
}
