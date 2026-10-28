//! An app's companion: its server half (the manifest's `companion=`), which
//! the paired neochumbyd runs for this Chumby. [`open`] connects to it;
//! frames then go both ways as `Msg::Data` on the session it returns, and
//! `Msg::Closed` says the companion has gone.
//!
//! An app names its own with [`companion!`](crate::companion!), checked
//! against the app's Cargo.toml as it compiles: an app without a
//! `companion`, or without `neochumbyd = "required"` or `"optional"`, does
//! not compile, as neochumbyd would have nothing to run for it.
//!
//! ```ignore
//! let s = neochumby::companion::open(neochumby::companion!(), &paired, &me)?;
//! ```

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use neochumby_proto::{Identity, Msg, Session, unhex_key};

use crate::conf::{self, Paired};

/// How long connecting to the neochumbyd may take.
const CONNECT: Duration = Duration::from_secs(10);

/// An app's companion, as neochumbyd knows it (apps/<app> in the tree).
/// Made only by [`companion!`](crate::companion!).
#[derive(Clone, Copy, Debug)]
pub struct Companion {
    app: &'static str,
}

impl Companion {
    /// For [`companion!`](crate::companion!), which has checked `app`.
    #[doc(hidden)]
    pub const fn __checked(app: &'static str) -> Companion {
        Companion { app }
    }

    /// The app's name: its directory in the tree.
    pub fn app(&self) -> &'static str {
        self.app
    }
}

/// Connects to the paired neochumbyd, checks its key against the one
/// pinned in `p`, and opens `companion` on it.
pub fn open(companion: Companion, p: &Paired, me: &Identity) -> io::Result<Session> {
    let server_key = unhex_key(&p.server_key)?;
    let addr = (p.server.as_str(), p.api_port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))?;
    let tcp = TcpStream::connect_timeout(&addr, CONNECT)?;
    let mut s = Session::connect(tcp, me)?;
    if s.peer() != &server_key {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "not the paired neochumbyd's key"));
    }
    s.send(&Msg::Hello { name: conf::name() })?;
    match s.recv()? {
        Msg::Welcome => {}
        Msg::Unpaired => {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "neochumbyd no longer pairs with this Chumby"));
        }
        m => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected {m:?}"))),
    }
    s.send(&Msg::Open { app: companion.app.into() })?;
    Ok(s)
}
