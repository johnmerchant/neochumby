//! Where a Chumby's neochumbyd is, as its conf names it (`server=`): a host
//! on its network, or its serial line (`serial`, or `serial:/dev/ttyS1` to
//! name the device), which an emulated Chumby has in place of a network.
//! Over the line, several sessions share one stream ([`crate::mux`]), and
//! each is the same TLS session as over TCP.

use std::fmt;

/// What `server=` holds for the serial line.
pub const SERIAL: &str = "serial";

/// The Chumby's second UART, which carries the line.
pub const SERIAL_DEVICES: [&str; 1] = ["/dev/ttymxc1"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Server {
    /// A host name or address, reached over TCP at the conf's `api_port`.
    Net(String),
    /// The serial line: on this device, or None for the second UART
    /// ([`SERIAL_DEVICES`]).
    Serial(Option<String>),
}

impl Server {
    /// `server=`'s value: `serial`, `serial:/dev/NAME`, or a host. None
    /// for a device outside /dev, or one named with anything but letters,
    /// digits and `/._-`.
    pub fn parse(s: &str) -> Option<Server> {
        if s == SERIAL {
            return Some(Server::Serial(None));
        }
        match s.strip_prefix(SERIAL).and_then(|r| r.strip_prefix(':')) {
            Some(dev) => {
                let ok = dev.strip_prefix("/dev/").is_some_and(|n| {
                    !n.is_empty()
                        && !n.split('/').any(|c| c.is_empty() || c == "." || c == "..")
                        && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
                });
                ok.then(|| Server::Serial(Some(dev.to_string())))
            }
            None if !s.is_empty() => Some(Server::Net(s.to_string())),
            None => None,
        }
    }

    pub fn is_serial(&self) -> bool {
        matches!(self, Server::Serial(_))
    }
}

impl fmt::Display for Server {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Server::Net(host) => f.write_str(host),
            Server::Serial(None) => f.write_str(SERIAL),
            Server::Serial(Some(dev)) => write!(f, "{SERIAL}:{dev}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(Server::parse("serial"), Some(Server::Serial(None)));
        assert_eq!(Server::parse("serial:/dev/ttyS1"), Some(Server::Serial(Some("/dev/ttyS1".into()))));
        assert_eq!(Server::parse("192.0.2.7"), Some(Server::Net("192.0.2.7".into())));
        assert_eq!(Server::parse("nas.local"), Some(Server::Net("nas.local".into())));
        for bad in ["", "serial:", "serial:ttyS1", "serial:/dev/", "serial:/dev/../mem", "serial:/dev/a b"] {
            assert_eq!(Server::parse(bad), None, "{bad}");
        }
        for s in ["serial", "serial:/dev/ttymxc1", "192.0.2.7"] {
            assert_eq!(Server::parse(s).unwrap().to_string(), s);
        }
    }
}
