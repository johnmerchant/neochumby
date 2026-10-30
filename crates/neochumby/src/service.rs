//! An app's service, reached from in front: its own exec, or a frontend,
//! another app the owner approved to show it (an AirPlay screen of
//! someone else's, say). The service listens with [`listen`]; the other
//! end connects with [`connect`], naming the app. Each gets a
//! `UnixStream`, and what goes over it is theirs (AirPlay's is
//! `neochumby_airplay_proto::frontend`).
//!
//! The socket is a Unix one in the abstract namespace, which every jail
//! reaches (it has no file), named after the app: `neochumby-service-<app>`.
//! Both ends check who is at the other by its uid, which the kernel
//! vouches for:
//!
//! - **the service** takes only its own uid (its app's exec runs as the
//!   same user) and the uids the session lists in `NEOCHUMBY_CLIENTS`: its
//!   approved frontends'. Anyone else is turned away, and logged.
//! - **a frontend** takes only a service running as the uid the session
//!   gives it in `NEOCHUMBY_FRONTEND` (`<app>:<uid>`), and its own app's
//!   exec only its own uid.
//!
//! The owner approves a frontend with the Chumby's settings
//! (`frontend_<app>=<frontend>`, the session's frontend.rs), and its
//! manifest says which app it's for (`frontend = "<app>"`). The approval
//! applies from the next session start.
//!
//! Another app could bind the name first. The uid checks stop it posing as
//! either end, though not from keeping them apart. Where jails are off (the
//! emulator, a session not run as root) every app is the same user, and
//! nothing here can tell them apart.

use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};

use neochumby_sys::{net, process};

/// The uids, comma-separated, of the frontends the owner approved for the
/// app a service belongs to.
pub const CLIENTS_ENV: &str = "NEOCHUMBY_CLIENTS";
/// For an approved frontend: the app it's for, and that app's uid when
/// jailed (`airplay:20001`; `airplay` alone when jails are off).
pub const FRONTEND_ENV: &str = "NEOCHUMBY_FRONTEND";

/// The socket's name for `app`.
fn name(app: &str) -> io::Result<SocketAddr> {
    SocketAddr::from_abstract_name(format!("neochumby-service-{app}"))
}

/// A service listening for its exec and its frontends.
pub struct Listener {
    sock: UnixListener,
    app: String,
    /// The uids it takes besides its own.
    clients: Vec<u32>,
}

/// Listens as `app`'s service (its directory in the tree), without
/// waiting: [`Listener::accept`] returns at once. Fails with
/// [`io::ErrorKind::AddrInUse`] while something else holds the name: a run
/// of the service that hasn't quite gone, which it can try again after.
pub fn listen(app: &str) -> io::Result<Listener> {
    let sock = UnixListener::bind_addr(&name(app)?)?;
    sock.set_nonblocking(true)?;
    let clients = std::env::var(CLIENTS_ENV).map(|v| parse_uids(&v)).unwrap_or_default();
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

/// The app this one is an approved frontend for, if the session said so,
/// and that app's uid when jailed.
pub fn frontend_of() -> Option<(String, Option<u32>)> {
    let v = std::env::var(FRONTEND_ENV).ok()?;
    let (app, uid) = match v.split_once(':') {
        Some((a, u)) => (a, Some(u.parse().ok()?)),
        None => (v.as_str(), None),
    };
    neochumby_proto::valid_name(app).then(|| (app.to_string(), uid))
}

/// Connects to `app`'s service: this app's own, or the one it's an
/// approved frontend for. Fails with [`io::ErrorKind::PermissionDenied`]
/// if what answers isn't that service, or with
/// [`io::ErrorKind::ConnectionRefused`] or [`io::ErrorKind::NotFound`]
/// while it isn't running.
pub fn connect(app: &str) -> io::Result<UnixStream> {
    let s = UnixStream::connect_addr(&name(app)?)?;
    let theirs = net::peer_uid(s.as_fd())?;
    let expected = match frontend_of() {
        Some((a, Some(uid))) if a == app => uid,
        _ => process::uid(),
    };
    if theirs != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{app}'s service isn't the one the session runs, or this app isn't approved as its frontend"),
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
        let l = listen(&app).unwrap();
        assert!(l.accept().unwrap().is_none(), "nothing yet");
        assert_eq!(listen(&app).err().map(|e| e.kind()), Some(io::ErrorKind::AddrInUse));
        let mut c = connect(&app).unwrap();
        let mut s = l.accept().unwrap().expect("connected");
        c.write_all(b"hi").unwrap();
        let mut b = [0u8; 2];
        s.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"hi");
        drop(l);
        assert!(connect(&app).is_err(), "gone with its listener");
    }

    #[test]
    fn uids_from_the_session() {
        assert_eq!(parse_uids("20001,20004"), [20001, 20004]);
        assert_eq!(parse_uids(""), [] as [u32; 0]);
        assert_eq!(parse_uids("x, 7"), [7]);
    }
}
