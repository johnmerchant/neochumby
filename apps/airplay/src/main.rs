//! AirPlay's Chumby half (the companion, companion.rs, is neochumbyd's).
//!
//! AirPlay on the Chumby's speaker, as a background service: it opens this
//! app's companion on the paired neochumbyd, which runs an AirPlay receiver
//! named after this Chumby while it is connected and sends what it plays
//! here as PCM ([0] + PCM; [1] when it stops), which goes to /dev/dsp. The
//! speaker is held only while something plays, so apps can have it
//! otherwise; if something else has it, what arrives meanwhile is dropped,
//! and the connection kept.
//!
//! Connects to the paired neochumbyd (/psp/neochumby.conf) and checks its
//! key against the one pinned there; reconnects, backing off, whenever the
//! connection goes.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::process::Command;
use std::time::{Duration, Instant};

use neochumby::companion;
use neochumby::conf;
use neochumby_proto::{Identity, Msg, Session};

// The companion's frames.
const AUDIO: u8 = 0;
const END: u8 = 1;

// sys/soundcard.h
const SNDCTL_DSP_SPEED: libc::Ioctl = 0xc004_5002u32 as libc::Ioctl;
const SNDCTL_DSP_SETFMT: libc::Ioctl = 0xc004_5005u32 as libc::Ioctl;
const SNDCTL_DSP_CHANNELS: libc::Ioctl = 0xc004_5006u32 as libc::Ioctl;
const AFMT_S16_LE: libc::c_int = 0x10;

/// The speaker amplifier's mute: the boot leaves it on, and whatever plays
/// turns it off while it does.
const SPEAKER_MUTE: &str = "/proc/sys/sense1/spkmute";

/// The stock firmware's master volume (0-100): with no argument it prints
/// the volume, with --master_volume N sets it.
const SET_VOLUME: &str = "/usr/bin/chumby_set_volume";

/// The master volume while AirPlay plays. The phone's volume is applied to
/// the audio itself (by shairport-sync), so this only has to be loud enough
/// for the phone's maximum to be the speaker's.
const PLAY_VOLUME: u32 = 90;

fn mute_speaker(mute: bool) {
    if let Err(e) = std::fs::write(SPEAKER_MUTE, if mute { "1" } else { "0" }) {
        eprintln!("neochumby-airplay: {SPEAKER_MUTE}: {e}");
    }
}

fn volume() -> Option<u32> {
    let out = Command::new(SET_VOLUME).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn set_volume(v: u32) {
    if let Err(e) = Command::new(SET_VOLUME).arg("--master_volume").arg(v.to_string()).output() {
        eprintln!("neochumby-airplay: {SET_VOLUME}: {e}");
    }
}

/// The speaker, set for 16-bit stereo at 44.1 kHz, unmuted and at
/// [`PLAY_VOLUME`] while open; dropping it puts both back.
struct Speaker {
    dsp: File,
    volume_before: Option<u32>,
}

impl Speaker {
    fn open() -> io::Result<Speaker> {
        let dsp = OpenOptions::new().write(true).open("/dev/dsp")?;
        for (req, value) in [(SNDCTL_DSP_SETFMT, AFMT_S16_LE), (SNDCTL_DSP_CHANNELS, 2), (SNDCTL_DSP_SPEED, 44100)] {
            let mut v = value;
            if unsafe { libc::ioctl(dsp.as_raw_fd(), req, &mut v) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let volume_before = volume();
        set_volume(PLAY_VOLUME);
        mute_speaker(false);
        Ok(Speaker { dsp, volume_before })
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        mute_speaker(true);
        if let Some(v) = self.volume_before {
            set_volume(v);
        }
    }
}

fn play(mut s: Session) -> io::Result<()> {
    let mut speaker: Option<Speaker> = None;
    // While the speaker is someone else's: when to try it again.
    let mut busy_until: Option<Instant> = None;
    loop {
        let frame = match s.recv()? {
            Msg::Data(f) => f,
            Msg::Closed => return Err(io::Error::other("the companion has gone")),
            _ => continue,
        };
        match frame.split_first() {
            Some((&AUDIO, pcm)) => {
                if speaker.is_none() && busy_until.is_none_or(|t| Instant::now() >= t) {
                    match Speaker::open() {
                        Ok(sp) => {
                            speaker = Some(sp);
                            busy_until = None;
                        }
                        Err(e) => {
                            if busy_until.is_none() {
                                eprintln!("neochumby-airplay: /dev/dsp: {e}; dropping audio until it is free");
                            }
                            busy_until = Some(Instant::now() + Duration::from_secs(2));
                        }
                    }
                }
                if let Some(sp) = speaker.as_mut()
                    && let Err(e) = sp.dsp.write_all(pcm)
                {
                    eprintln!("neochumby-airplay: /dev/dsp: {e}");
                    speaker = None;
                }
            }
            Some((&END, _)) => {
                speaker = None;
                busy_until = None;
            }
            _ => {}
        }
    }
}

fn main() {
    neochumby::debug::install("neochumby-airplay");
    let Some(p) = conf::paired() else {
        eprintln!("neochumby-airplay: not paired");
        std::process::exit(1);
    };
    let me = match Identity::from_secret(&p.secret) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("neochumby-airplay: {}: {e}", conf::KEY);
            std::process::exit(1);
        }
    };
    let mut backoff = 2;
    loop {
        let started = Instant::now();
        let r = companion::open(neochumby::companion!(), &p, &me).and_then(play);
        if let Err(e) = r {
            eprintln!("neochumby-airplay: {e}");
        }
        // A connection that lasted was a good one: start backing off again.
        if started.elapsed() > Duration::from_secs(60) {
            backoff = 2;
        }
        neochumby::sleep_ms(backoff * 1000);
        backoff = (backoff * 2).min(60);
    }
}
