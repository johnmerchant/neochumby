//! Typed messages between an app and its companion.
//!
//! An app with a companion defines, in its library crate, a type for each
//! direction and a [`Channel`] that pairs them. Its halves then share them:
//! the Chumby's sends and receives through
//! `neochumby::companion::Link::typed`, the companion through [`stdio`].
//! Each end sends only its own direction's type and gets only the other's,
//! so halves that disagree on what a message is don't compile.
//!
//! ```
//! use neochumby_proto::channel::Channel;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! pub enum Ask { Weather { city: String } }
//! #[derive(Serialize, Deserialize)]
//! pub enum Answer { Weather { celsius: i16 } }
//!
//! pub struct Wx;
//! impl Channel for Wx {
//!     type ToCompanion = Ask;
//!     type ToApp = Answer;
//! }
//! ```
//!
//! The companion can't send what only the app does, nor the app what
//! only the companion does:
//!
//! ```compile_fail
//! # use neochumby_proto::channel::Channel;
//! # use serde::{Deserialize, Serialize};
//! # #[derive(Serialize, Deserialize)]
//! # pub struct Ask;
//! # #[derive(Serialize, Deserialize)]
//! # pub struct Answer;
//! # pub struct Wx;
//! # impl Channel for Wx { type ToCompanion = Ask; type ToApp = Answer; }
//! let (_, mut to_app) = neochumby_proto::channel::stdio::<Wx>();
//! to_app.send(&Ask)?; // expected `Answer`, found `Ask`
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Each message is one frame (crate::frame), as [postcard] encodes it: a
//! compact format with a written specification, so neither half's build
//! changes what is on the wire. An app and its companion come from the
//! same tree and change together; a frame that doesn't decode as the
//! type expected is an error ([`io::ErrorKind::InvalidData`]).
//!
//! A message must encode to at most [`frame::MAX_FRAME`] bytes. Bulk bytes
//! are best sent as several: a `Vec<u8>` costs at most [`OVERHEAD`] bytes
//! over its length, in an enum variant.
//!
//! [postcard]: https://postcard.jamesmunns.com/

use std::io::{self, Read, Write};
use std::marker::PhantomData;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{frame, invalid};

/// The most an enum variant's tag and a byte string's length add to the
/// bytes it carries (a varint each, the length's three bytes at most for
/// one frame's worth).
pub const OVERHEAD: usize = 8;

/// What an app and its companion send each other.
pub trait Channel {
    /// The Chumby's half, to its companion.
    type ToCompanion: Serialize + DeserializeOwned;
    /// The companion, to the Chumby's half.
    type ToApp: Serialize + DeserializeOwned;
}

/// One message as a frame's bytes.
pub fn encode<T: Serialize>(m: &T) -> io::Result<Vec<u8>> {
    let b = postcard::to_allocvec(m).map_err(invalid)?;
    if b.len() > frame::MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "message too long for a frame"));
    }
    Ok(b)
}

/// One message from a frame's bytes, all of them.
pub fn decode<T: DeserializeOwned>(b: &[u8]) -> io::Result<T> {
    match postcard::take_from_bytes(b) {
        Ok((m, [])) => Ok(m),
        Ok(_) => Err(invalid("trailing bytes in a frame")),
        Err(e) => Err(invalid(format!("a frame that isn't a message: {e}"))),
    }
}

/// The companion's end of `C`: what the app sends, from its stdin.
pub struct Incoming<C, R = io::Stdin> {
    r: R,
    _c: PhantomData<fn() -> C>,
}

/// The companion's end of `C`: what it sends the app, on its stdout.
pub struct Outgoing<C, W = io::Stdout> {
    w: W,
    _c: PhantomData<fn() -> C>,
}

/// The companion's ends of `C`, on its stdin and stdout, each for a
/// thread of its own if need be.
pub fn stdio<C: Channel>() -> (Incoming<C>, Outgoing<C>) {
    (Incoming::new(io::stdin()), Outgoing::new(io::stdout()))
}

impl<C: Channel, R: Read> Incoming<C, R> {
    pub fn new(r: R) -> Self {
        Incoming { r, _c: PhantomData }
    }

    /// The app's next message, or None once it has gone.
    pub fn recv(&mut self) -> io::Result<Option<C::ToCompanion>> {
        frame::read(&mut self.r)?.map(|f| decode(&f)).transpose()
    }
}

impl<C: Channel, W: Write> Outgoing<C, W> {
    pub fn new(w: W) -> Self {
        Outgoing { w, _c: PhantomData }
    }

    /// Sends the app one message.
    pub fn send(&mut self, m: &C::ToApp) -> io::Result<()> {
        frame::write(&mut self.w, &encode(m)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    enum Up {
        Show(u8),
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    enum Down {
        Names(Vec<String>),
        Text(Vec<u8>),
    }

    struct Test;
    impl Channel for Test {
        type ToCompanion = Up;
        type ToApp = Down;
    }

    #[test]
    fn messages_go_as_frames() {
        let mut wire = Vec::new();
        let mut out = Outgoing::<Test, _>::new(&mut wire);
        out.send(&Down::Names(vec!["syslog".into(), "nginx".into()])).unwrap();
        out.send(&Down::Text(b"hello\n".to_vec())).unwrap();
        // postcard: the variant, then the bytes' length and the bytes.
        assert_eq!(&wire[wire.len() - 12..], b"\x00\x00\x00\x08\x01\x06hello\n");
        let mut back: Incoming<TestBack, _> = Incoming::new(&wire[..]);
        assert_eq!(back.recv().unwrap(), Some(Down::Names(vec!["syslog".into(), "nginx".into()])));
        assert_eq!(back.recv().unwrap(), Some(Down::Text(b"hello\n".to_vec())));
        assert_eq!(back.recv().unwrap(), None);
    }

    /// Test the other way round, to read what the companion wrote.
    struct TestBack;
    impl Channel for TestBack {
        type ToCompanion = Down;
        type ToApp = Up;
    }

    #[test]
    fn a_frame_that_isnt_a_message_is_an_error() {
        assert_eq!(decode::<Up>(&[0, 7]).unwrap(), Up::Show(7));
        assert_eq!(decode::<Up>(&[0, 7, 0]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(decode::<Up>(&[9, 7]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(decode::<Up>(&[]).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn the_most_a_frame_holds() {
        let most = frame::MAX_FRAME - OVERHEAD;
        assert!(encode(&Down::Text(vec![0; most])).unwrap().len() <= frame::MAX_FRAME);
        assert!(encode(&Down::Text(vec![0; frame::MAX_FRAME])).is_err());
    }
}
