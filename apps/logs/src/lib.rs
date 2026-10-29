//! neochumby-logs: logs on the Chumby's screen, scrolled by touch.
//!
//! The Chumby's own come first: the session's (/tmp/neochumby.log, which
//! the boot hook and every app write to), the kernel's (dmesg), and any
//! other log in /tmp or /var/log. With a paired neochumbyd, the host's
//! follow, as its companion (companion.rs) is configured to serve them
//! ([`config`]). Each is followed while it is shown, as `tail -f` would.
//!
//! The viewer and the companion talk in [`Logs`]'s messages
//! (neochumby_proto::channel): [`FromViewer`] and [`FromCompanion`].

pub mod config;
pub mod tail;
pub mod text;
pub mod view;

#[cfg(target_arch = "arm")]
pub mod device;

use neochumby_proto::channel::Channel;
use serde::{Deserialize, Serialize};

/// The viewer, to the companion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromViewer {
    /// Show the host's log `i`, by its place in `Sources`, and stop the
    /// last.
    Show(u8),
}

/// The companion, to the viewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromCompanion {
    /// The host's logs, by name, as it starts.
    Sources(Vec<String>),
    /// Log `i`'s text starts here: clear the screen.
    Start(u8),
    /// More of it, as it was read: not always whole lines, or UTF-8.
    Text(Vec<u8>),
}

/// The logs app's channel: what its halves send each other.
pub struct Logs;

impl Channel for Logs {
    type ToCompanion = FromViewer;
    type ToApp = FromCompanion;
}

/// How much of a log is read as it is opened: its end, this many bytes.
pub const TAIL: u64 = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use neochumby_proto::channel::{Incoming, OVERHEAD, Outgoing, encode};
    use neochumby_proto::frame::MAX_FRAME;

    /// The viewer's end, as neochumby::companion::Typed reads it: the
    /// companion's messages, the other way round.
    struct Viewer;
    impl Channel for Viewer {
        type ToCompanion = FromCompanion;
        type ToApp = FromViewer;
    }

    #[test]
    fn the_companion_and_the_viewer_agree() {
        let said = [
            FromCompanion::Sources(vec!["syslog".into(), "setup".into()]),
            FromCompanion::Start(1),
            FromCompanion::Text(b"one\ntw".to_vec()),
            FromCompanion::Text(vec![0xff; MAX_FRAME - OVERHEAD]),
        ];
        let mut wire = Vec::new();
        let mut out = Outgoing::<Logs, _>::new(&mut wire);
        for m in &said {
            out.send(m).unwrap();
        }
        let mut viewer = Incoming::<Viewer, _>::new(&wire[..]);
        for m in said {
            assert_eq!(viewer.recv().unwrap(), Some(m));
        }
        assert_eq!(viewer.recv().unwrap(), None);
        assert_eq!(encode(&FromViewer::Show(3)).unwrap(), [0, 3]);
    }
}
