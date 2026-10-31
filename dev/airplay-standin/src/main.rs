//! A stand-in for AirPlay's receiver (apps/airplay/src/companion), for an
//! emulated Chumby, which has no network to reach a neochumbyd. AirPlay's
//! service runs it in the companion's place when it finds it in the app's
//! directory (`standin-companion`), and it speaks the companion's channel
//! on its stdin and stdout (neochumby_airplay_proto::receiver), as
//! shairport-sync and a phone would through the real one:
//!
//! - three made-up tracks, each 30 seconds of a tone (A4, C#5, E5) at
//!   44.1 kHz, the first two with a generated cover, the third with a
//!   title outside ISO 8859-1 and none;
//! - the controls: play/pause, next and previous move through them, as a
//!   phone would; the volume goes up and down in tens; each is logged
//!   (`airplay-standin: control Next`), for a scenario to wait on.
//!
//! The audio is paced by the clock, as a sender's is, so a service that
//! drops it (paused, or without the speaker) isn't flooded. Exits when its
//! stdin closes.

use std::io::{self, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use neochumby_airplay_proto::receiver::{MAX_AUDIO, ToChumby, ToReceiver};
use neochumby_airplay_proto::{COVER_SIDE, Control, Cover, Play, State, Track, decode, send};
use neochumby_proto::frame;
use neochumby_sys::fd;
use neochumby_sys::time::sleep_ms;

const RATE: u32 = 44_100;
/// Each track's length.
const LENGTH_MS: u32 = 30_000;
/// The audio goes in pieces of this long.
const PIECE_MS: u32 = 100;

/// The made-up tracks: title, artist, album, the tone's frequency.
const TRACKS: [(&str, &str, &str, u32); 3] = [
    ("Test Tone in A", "The Emulators", "Sine Waves, Vol. 1", 440),
    ("A Second, Rather Longer Signal Than The First", "The Emulators", "Sine Waves, Vol. 1", 554),
    ("Café 東京 🎵", "Les Échantillons", "Made-Up Recordings", 659),
];

/// A generated cover for track `n`, or none.
fn cover(n: usize) -> Cover {
    let side = COVER_SIDE as usize;
    let rgb565 = |r: usize, g: usize, b: usize| (((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)) as u16;
    let px: Box<dyn Fn(usize, usize) -> u16> = match n {
        // Red to blue across, dark to light down.
        0 => Box::new(|x, y| rgb565(255 - x * 2, y * 2, x * 2)),
        // Green and yellow quarters.
        1 => Box::new(|x, y| if (x < 64) == (y < 64) { rgb565(0, 200, 0) } else { rgb565(255, 220, 0) }),
        _ => return Cover::default(),
    };
    let mut bytes = Vec::with_capacity(side * side * 2);
    for y in 0..side {
        for x in 0..side {
            bytes.extend_from_slice(&px(x, y).to_le_bytes());
        }
    }
    Cover { width: COVER_SIDE, height: COVER_SIDE, rgb565: bytes }
}

struct Standin {
    out: io::Stdout,
    track: usize,
    play: Play,
    volume: u8,
    /// The time gone in the track, at `since`.
    elapsed_ms: u32,
    since: Instant,
    /// Audio frames written of the track so far.
    sent: u64,
}

impl Standin {
    fn elapsed(&self) -> u32 {
        let mut e = self.elapsed_ms;
        if self.play == Play::Playing {
            e += self.since.elapsed().as_millis() as u32;
        }
        e.min(LENGTH_MS)
    }

    fn state(&mut self) -> io::Result<()> {
        let s = State {
            play: self.play,
            controls: true,
            elapsed_ms: self.elapsed(),
            duration_ms: LENGTH_MS,
            volume: Some(self.volume),
        };
        send(&mut self.out, &ToChumby::State(s))
    }

    /// Starts track `n` from the top, playing.
    fn start(&mut self, n: usize) -> io::Result<()> {
        self.track = n % TRACKS.len();
        let (title, artist, album, _) = TRACKS[self.track];
        let t = Track { title: title.into(), artist: artist.into(), album: album.into(), source: "Test iPhone".into() };
        send(&mut self.out, &ToChumby::Track(t))?;
        send(&mut self.out, &ToChumby::Cover(cover(self.track)))?;
        self.play = Play::Playing;
        self.elapsed_ms = 0;
        self.since = Instant::now();
        self.sent = 0;
        eprintln!("airplay-standin: playing {:?}", TRACKS[self.track].0);
        self.state()
    }

    fn set_play(&mut self, play: Play) -> io::Result<()> {
        self.elapsed_ms = self.elapsed();
        self.since = Instant::now();
        self.play = play;
        if play != Play::Playing {
            send(&mut self.out, &ToChumby::End)?;
        }
        self.state()
    }

    fn control(&mut self, c: Control) -> io::Result<()> {
        eprintln!("airplay-standin: control {c:?}");
        match c {
            Control::PlayPause if self.play == Play::Playing => self.set_play(Play::Paused),
            Control::PlayPause | Control::Play => self.set_play(Play::Playing),
            Control::Pause => self.set_play(Play::Paused),
            Control::Next => self.start(self.track + 1),
            Control::Previous => self.start(self.track + TRACKS.len() - 1),
            Control::VolumeUp => {
                self.volume = (self.volume + 10).min(100);
                self.state()
            }
            Control::VolumeDown => {
                self.volume = self.volume.saturating_sub(10);
                self.state()
            }
        }
    }

    /// The audio due by now, as the clock goes.
    fn audio(&mut self) -> io::Result<()> {
        if self.play != Play::Playing {
            return Ok(());
        }
        let due = self.elapsed() as u64 * RATE as u64 / 1000;
        let freq = TRACKS[self.track].3 as u64;
        while self.sent < due {
            let n = (due - self.sent).min(MAX_AUDIO as u64 / 4);
            let mut pcm = Vec::with_capacity(n as usize * 4);
            for i in self.sent..self.sent + n {
                // A triangle wave: integer arithmetic, as the ARM926 has no
                // FPU, and near enough a tone to hear.
                let phase = (i * freq * 4 % (RATE as u64 * 4)) as i64;
                let q = RATE as i64;
                let v = if phase < 2 * q { phase - q } else { 3 * q - phase };
                let s = (v * 8000 / q) as i16;
                pcm.extend_from_slice(&s.to_le_bytes());
                pcm.extend_from_slice(&s.to_le_bytes());
            }
            send(&mut self.out, &ToChumby::Audio(pcm))?;
            self.sent += n;
        }
        if self.elapsed() >= LENGTH_MS {
            return self.start(self.track + 1);
        }
        Ok(())
    }
}

fn run() -> io::Result<()> {
    let mut stdin = io::stdin();
    let mut s = Standin {
        out: io::stdout(),
        track: 0,
        play: Play::Stopped,
        volume: 60,
        elapsed_ms: 0,
        since: Instant::now(),
        sent: 0,
    };
    send(&mut s.out, &ToChumby::Name("Emulated Chumby".into()))?;
    s.state()?;
    // A moment's quiet, as before a phone picks the receiver.
    sleep_ms(2000);
    s.start(0)?;
    let mut last_state = Instant::now();
    loop {
        if fd::readable(&[stdin.as_fd()], PIECE_MS as i32)?[0] {
            match frame::read(&mut stdin)? {
                None => return Ok(()),
                Some(f) => {
                    if let Ok(ToReceiver::Control(c)) = decode(&f) {
                        s.control(c)?;
                    }
                }
            }
        }
        s.audio()?;
        // Where it has got to, now and then, as shairport-sync's `prgr`.
        if last_state.elapsed() > Duration::from_secs(5) {
            s.state()?;
            last_state = Instant::now();
        }
        s.out.flush()?;
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("airplay-standin: {e}");
        std::process::exit(1);
    }
}
