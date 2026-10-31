//! How this Chumby reaches its neochumbyd: over TCP, or, when its conf
//! says `server=serial` (an emulated Chumby, which has no network), over
//! its serial line (neochumby_proto::server). The session owns the line
//! and shares it out (its line.rs): each connection to [`SOCKET`] becomes
//! one on the line, to neochumbyd, which handles it as a TCP connection.
//! What goes over it is the same TLS session either way, checked end to
//! end; the session in between only carries bytes.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use neochumby_proto::{Server, Stream};

/// Where the session takes connections for the line: in a directory only
/// root can enter, outside every app's jail (each has a /tmp of its own).
pub const DIR: &str = "/tmp/neochumby-line";
pub const SOCKET: &str = "/tmp/neochumby-line/socket";

/// A connection to the neochumbyd at `server` (a conf's `server=`),
/// within `timeout`: TCP to its `port`, with keepalive so that a
/// neochumbyd gone without a word is noticed in about a minute; or, for
/// `serial`, through the session's line.
pub fn connect(server: &str, port: u16, timeout: Duration) -> io::Result<Box<dyn Stream>> {
    match Server::parse(server) {
        Some(Server::Serial(_)) => {
            let s = UnixStream::connect(SOCKET)
                .map_err(|e| io::Error::new(e.kind(), format!("the serial line ({SOCKET}): {e}")))?;
            Ok(Box::new(s))
        }
        Some(Server::Net(host)) => {
            let addr = (host.as_str(), port)
                .to_socket_addrs()?
                .next()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))?;
            let tcp = TcpStream::connect_timeout(&addr, timeout)?;
            neochumby_sys::net::keepalive(&tcp, 30, 10, 3);
            Ok(Box::new(tcp))
        }
        None => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("not a server: {server}"))),
    }
}
