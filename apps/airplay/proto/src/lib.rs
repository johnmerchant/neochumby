//! AirPlay on neochumby, as the apps that show it see it: what's playing
//! (the track, its cover art, where it has got to) and the controls
//! (play/pause, next, previous, volume). A frontend, AirPlay's own or
//! another app's, depends on this crate and nothing else of AirPlay's.
//!
//! AirPlay is three programs (apps/airplay):
//!
//! - **the receiver**, AirPlay's companion, which neochumbyd runs on its
//!   host for each Chumby: shairport-sync, announced under the Chumby's
//!   name, with its metadata and remote control;
//! - **the service**, on the Chumby, which keeps the only connection to
//!   the receiver, plays the audio on the speaker, is the Chumby's player
//!   if the owner chose it (`pause`, `resume`), and serves frontends;
//! - **a frontend**, in front: AirPlay's own text-mode screen, or another
//!   app's, which the owner approved.
//!
//! Two channels join them, each a pair of message types:
//!
//! - [`receiver`]: the receiver and the service, over the companion link
//!   (`neochumby::companion`), as a `neochumby_proto::channel::Channel`.
//! - [`frontend`]: the service and its frontends, over the service's socket
//!   (`neochumby::service`), which a frontend opens with
//!   `neochumby::service::connect(neochumby_airplay_proto::APP)`.
//!
//! Each message is one frame (`neochumby_proto::frame`: a u32 big-endian
//! length, then that many bytes), encoded by postcard. Nothing here knows
//! what carries the frames: a socket, a pipe or a test's buffer.
//!
//! # Versions
//!
//! [`VERSION`] is what the service says it speaks, first, in
//! [`frontend::ToFrontend::Hello`]. Messages change only by new variants
//! added at the end of an enum, and a frame that doesn't decode is
//! skipped ([`Reader`]), so an older frontend keeps working with a newer
//! service, without what's new. A message's fields never change: what
//! would change one is a new variant. This crate's own semver follows the
//! same rule.

use std::io::{self, Read, Write};

use neochumby_proto::frame::{self, MAX_FRAME};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use neochumby_proto::channel::{decode, encode};

/// The protocol version of these messages (see the crate's docs).
pub const VERSION: u16 = 1;

/// The app whose service frontends connect to: AirPlay's directory in the
/// tree.
pub const APP: &str = "airplay";

/// The cover art's longest side, in pixels, as the receiver scales it.
pub const COVER_SIDE: u16 = 128;

/// What is playing, as the sender describes it. Any field may be empty.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The sender: "Matt's iPhone".
    pub source: String,
}

impl Track {
    /// Nothing is known of it.
    pub fn is_empty(&self) -> bool {
        self.title.is_empty() && self.artist.is_empty() && self.album.is_empty()
    }
}

/// Cover art, scaled by the receiver to fit [`COVER_SIDE`] square, as the
/// Chumby's ARM926 shouldn't decode JPEGs. 0 by 0 is none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cover {
    pub width: u16,
    pub height: u16,
    /// RGB565, little-endian, a row at a time: `width * height * 2` bytes.
    #[serde(with = "serde_bytes")]
    pub rgb565: Vec<u8>,
}

impl Cover {
    /// There is no picture.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0 || self.rgb565.len() < self.width as usize * self.height as usize * 2
    }

    /// The pixel at (`x`, `y`), RGB565; black outside the picture.
    pub fn pixel(&self, x: u16, y: u16) -> u16 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let i = (y as usize * self.width as usize + x as usize) * 2;
        self.rgb565.get(i..i + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
    }

    /// The pixel at (`x`, `y`) as 8-bit red, green and blue.
    pub fn rgb(&self, x: u16, y: u16) -> [u8; 3] {
        let p = self.pixel(x, y);
        let (r, g, b) = ((p >> 11) & 0x1f, (p >> 5) & 0x3f, p & 0x1f);
        [(r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8]
    }
}

/// Whether the sender is playing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Play {
    #[default]
    Stopped,
    Playing,
    Paused,
}

/// Where the track has got to, when it was sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub play: Play,
    /// The sender takes remote control: [`Control`]s reach it.
    pub controls: bool,
    /// The time gone, when this was sent; it runs on while playing.
    pub elapsed_ms: u32,
    /// The track's length; 0 if not known.
    pub duration_ms: u32,
    /// The sender's volume, 0 to 100, if it has said.
    pub volume: Option<u8>,
}

/// A button: what the sender is asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Control {
    PlayPause,
    Next,
    Previous,
    /// Pause, and play: what the service sends for the session's `pause`
    /// and `resume`, as the Chumby's player. A frontend may send them too.
    Pause,
    Play,
    VolumeUp,
    VolumeDown,
}

/// The receiver (AirPlay's companion) and the service on the Chumby.
pub mod receiver {
    use super::*;

    /// From the receiver.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ToChumby {
        /// Audio, as it comes: S16LE, stereo, 44.1 kHz.
        Audio(#[serde(with = "serde_bytes")] Vec<u8>),
        /// The audio has stopped for now: the speaker can go.
        End,
        /// The receiver's name, as phones list it.
        Name(String),
        Track(Track),
        Cover(Cover),
        State(State),
    }

    /// To the receiver.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ToReceiver {
        Control(Control),
    }

    /// The companion channel: the service's `Link::typed::<Receiver>()`,
    /// the companion's `neochumby_proto::channel::stdio::<Receiver>()`.
    pub struct Receiver;

    impl neochumby_proto::channel::Channel for Receiver {
        type ToCompanion = ToReceiver;
        type ToApp = ToChumby;
    }

    /// The most audio one [`ToChumby::Audio`] carries.
    pub const MAX_AUDIO: usize = MAX_FRAME - neochumby_proto::channel::OVERHEAD;
}

/// The service and its frontends.
pub mod frontend {
    use std::time::Instant;

    use super::*;

    /// From the service. A frontend that connects is told everything
    /// known, in this order (Hello first), then each change as it comes.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ToFrontend {
        /// The [`VERSION`] the service speaks.
        Hello {
            version: u16,
        },
        /// Whether the service has its receiver: while it hasn't (no
        /// neochumbyd, or it's away), nothing can play.
        Link(bool),
        /// The receiver's name, as phones list it.
        Name(String),
        Track(Track),
        Cover(Cover),
        State(State),
    }

    /// To the service.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ToService {
        Control(Control),
    }

    /// All a frontend knows, kept up to date by [`NowPlaying::apply`].
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct NowPlaying {
        /// The service's version, once it has said.
        pub version: Option<u16>,
        pub link: bool,
        pub name: String,
        pub track: Track,
        pub cover: Cover,
        pub state: State,
        /// When `state` came.
        pub at: Option<Instant>,
    }

    impl NowPlaying {
        /// Takes in a message that came at `now`.
        pub fn apply(&mut self, m: ToFrontend, now: Instant) {
            match m {
                ToFrontend::Hello { version } => self.version = Some(version),
                ToFrontend::Link(l) => self.link = l,
                ToFrontend::Name(n) => self.name = n,
                ToFrontend::Track(t) => self.track = t,
                ToFrontend::Cover(c) => self.cover = c,
                ToFrontend::State(s) => {
                    self.state = s;
                    self.at = Some(now);
                }
            }
        }

        /// The time gone at `now`: as the last state said, plus the time
        /// since while playing, up to the track's length.
        pub fn elapsed_ms(&self, now: Instant) -> u32 {
            let mut e = self.state.elapsed_ms as u64;
            if self.state.play == Play::Playing
                && let Some(at) = self.at
            {
                e += now.saturating_duration_since(at).as_millis() as u64;
            }
            if self.state.duration_ms > 0 {
                e = e.min(self.state.duration_ms as u64);
            }
            e.min(u32::MAX as u64) as u32
        }
    }
}

/// One message as a whole frame, its length first: what goes on the
/// wire, for a writer that buffers.
pub fn frame<T: Serialize>(m: &T) -> io::Result<Vec<u8>> {
    let b = encode(m)?;
    let mut f = Vec::with_capacity(4 + b.len());
    f.extend_from_slice(&(b.len() as u32).to_be_bytes());
    f.extend_from_slice(&b);
    Ok(f)
}

/// Writes one message as a frame.
pub fn send<T: Serialize>(w: &mut impl Write, m: &T) -> io::Result<()> {
    frame::write(w, &encode(m)?)
}

/// Messages as they come off a stream, which may hand them over in pieces
/// (and, if it doesn't wait, none at all for now).
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
}

impl Reader {
    /// The next message; None at the end of the stream. A frame that
    /// doesn't decode as a `T` (a newer end's) is skipped. A stream that
    /// doesn't wait fails with [`io::ErrorKind::WouldBlock`] when a whole
    /// frame hasn't come, keeping what has for the next call.
    pub fn next<T: DeserializeOwned>(&mut self, r: &mut impl Read) -> io::Result<Option<T>> {
        loop {
            while let Some(f) = self.take()? {
                if let Ok(m) = decode(&f) {
                    return Ok(Some(m));
                }
            }
            let mut chunk = [0u8; 8192];
            match r.read(&mut chunk) {
                Ok(0) => return Ok(None),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// A whole frame, if one has come.
    fn take(&mut self) -> io::Result<Option<Vec<u8>>> {
        let Some(len) = self.buf.first_chunk::<4>() else { return Ok(None) };
        let n = u32::from_be_bytes(*len) as usize;
        if n > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too long"));
        }
        if self.buf.len() < 4 + n {
            return Ok(None);
        }
        let f = self.buf[4..4 + n].to_vec();
        self.buf.drain(..4 + n);
        Ok(Some(f))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::frontend::{NowPlaying, ToFrontend, ToService};
    use super::receiver::{MAX_AUDIO, ToChumby};
    use super::*;

    /// Hands over at most five bytes a read, then says it would block.
    struct Trickle<'a>(&'a [u8], bool);

    impl Read for Trickle<'_> {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.1 = !self.1;
            if self.1 && !self.0.is_empty() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let n = b.len().min(5).min(self.0.len());
            b[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn messages_come_whole_however_the_stream_cuts_them() {
        let track = Track { title: "Song".into(), artist: "Band".into(), ..Track::default() };
        let mut wire = Vec::new();
        send(&mut wire, &ToFrontend::Hello { version: VERSION }).unwrap();
        send(&mut wire, &ToFrontend::Track(track.clone())).unwrap();
        let mut r = Trickle(&wire, false);
        let mut reader = Reader::default();
        let mut got = Vec::new();
        loop {
            match reader.next::<ToFrontend>(&mut r) {
                Ok(Some(m)) => got.push(m),
                Ok(None) => break,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(got, [ToFrontend::Hello { version: VERSION }, ToFrontend::Track(track)]);
    }

    #[test]
    fn what_doesnt_decode_is_skipped() {
        let mut wire = Vec::new();
        // A newer service's message: a variant this build doesn't have.
        frame::write(&mut wire, &[200, 1]).unwrap();
        send(&mut wire, &ToService::Control(Control::Next)).unwrap();
        let mut reader = Reader::default();
        assert_eq!(reader.next::<ToService>(&mut &wire[..]).unwrap(), Some(ToService::Control(Control::Next)));
        assert_eq!(reader.next::<ToService>(&mut &wire[..0]).unwrap(), None);
    }

    /// The wire format is what other repositories build against: these
    /// bytes may only change with a new [`VERSION`].
    #[test]
    fn the_wire_is_stable() {
        assert_eq!(encode(&ToFrontend::Hello { version: 1 }).unwrap(), [0, 1]);
        assert_eq!(encode(&ToService::Control(Control::PlayPause)).unwrap(), [0, 0]);
        assert_eq!(encode(&ToService::Control(Control::VolumeDown)).unwrap(), [0, 6]);
        let s = State { play: Play::Paused, controls: true, elapsed_ms: 300, duration_ms: 0, volume: Some(50) };
        assert_eq!(encode(&ToFrontend::State(s)).unwrap(), [5, 2, 1, 0xac, 0x02, 0, 1, 50]);
        assert_eq!(encode(&ToChumby::Audio(vec![1, 2])).unwrap(), [0, 2, 1, 2]);
        assert_eq!(encode(&ToChumby::End).unwrap(), [1]);
    }

    #[test]
    fn the_largest_cover_and_audio_fit_a_frame() {
        let side = COVER_SIDE as usize;
        let c = Cover { width: COVER_SIDE, height: COVER_SIDE, rgb565: vec![0xff; side * side * 2] };
        assert!(frame(&ToFrontend::Cover(c.clone())).is_ok());
        assert!(encode(&ToChumby::Cover(c)).is_ok());
        assert!(encode(&ToChumby::Audio(vec![0; MAX_AUDIO])).is_ok());
    }

    #[test]
    fn cover_pixels() {
        // Red, then white, in a 2 by 1 picture.
        let c = Cover { width: 2, height: 1, rgb565: vec![0x00, 0xf8, 0xff, 0xff] };
        assert_eq!(c.rgb(0, 0), [255, 0, 0]);
        assert_eq!(c.rgb(1, 0), [255, 255, 255]);
        assert_eq!(c.pixel(2, 0), 0);
        assert!(!c.is_empty());
        assert!(Cover { width: 2, height: 2, rgb565: vec![0; 4] }.is_empty());
    }

    #[test]
    fn the_time_runs_on_while_playing() {
        let t0 = Instant::now();
        let mut np = NowPlaying::default();
        let s = State { play: Play::Playing, elapsed_ms: 1000, duration_ms: 5000, ..State::default() };
        np.apply(ToFrontend::State(s), t0);
        assert_eq!(np.elapsed_ms(t0 + Duration::from_secs(2)), 3000);
        assert_eq!(np.elapsed_ms(t0 + Duration::from_secs(60)), 5000, "not past the end");
        np.apply(ToFrontend::State(State { play: Play::Paused, ..s }), t0);
        assert_eq!(np.elapsed_ms(t0 + Duration::from_secs(2)), 1000, "paused");
    }
}
