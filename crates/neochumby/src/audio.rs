//! The Chumby Classic's speaker: /dev/dsp (OSS), the amplifier's mute
//! (/proc/sys/sense1/spkmute; the boot leaves it on) and the stock
//! firmware's master volume (0-100, through /usr/bin/chumby_set_volume).
//!
//! [`Unmuted`] is the volume and the mute alone, for a process that has
//! another program play (the stock btplay, which opens /dev/dsp itself);
//! [`Speaker`] is that plus /dev/dsp, for one that writes PCM itself.
//!
//! # The handover
//!
//! /dev/dsp can be open in only one process at a time, so the newest player
//! takes the speaker: whoever holds it is asked to yield, stops its player
//! and drops its guard, and may start again once the speaker is free.
//! Processes agree on this through two files in /tmp/neochumby:
//! - `speaker`: the holder's pid. Creating an [`Unmuted`] or a [`Speaker`]
//!   writes it, failing if another live process's pid is there; dropping
//!   the guard removes it. A dead process's claim is ignored and replaced.
//! - `speaker.yield`: the pid of the process asking for the speaker. A
//!   newcomer that finds the speaker held writes it, and its guard's
//!   creation fails with [`io::ErrorKind::WouldBlock`]; it tries again
//!   soon, which renews the request, and taking the speaker removes it. A
//!   request counts while its process is alive and for five seconds after
//!   it was last written. While one is pending, a second newcomer waits
//!   (also `WouldBlock`) and asks the first once that one holds the speaker.
//!
//! The holder polls its guard's `yield_requested` as it plays and, when it
//! is true, stops its player (and waits for it to exit, if it has another
//! program play) before dropping the guard. A process that yielded doesn't
//! ask for the speaker back: it waits until [`free`] says no one else holds
//! it or has asked for it. A newcomer that gives up calls [`withdraw`].
//!
//! A [`Speaker`] is written as an [`io::Write`] of interleaved 16-bit
//! little-endian PCM, rather than through cpal, the crate most Rust audio
//! (rodio, kira, Bevy's audio) is built on. cpal isn't an option yet:
//! - On Linux it depends on the `alsa` crate unconditionally, even for a
//!   custom host, and that links libasound, which the Chumby's static musl
//!   binaries don't have.
//! - It has no OSS host. One could be written with its `custom` feature,
//!   but its own hosts run streams on threads synchronised with std's
//!   `Mutex`, whose futex flags the stock 2.6.16 kernel lacks, so it would
//!   need a futex-free design.
//! - rodio, the usual way to play through it, mixes in floating point,
//!   which the FPU-less ARM926 can only emulate.
//!
//! If cpal makes `alsa` optional, an OSS custom host would let those crates
//! play here.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::process::Command;

use neochumby_sys::oss::{self, AFMT_S16_LE, Setting};
use neochumby_sys::speaker::{Handover, Take};

use crate::debug;

const DSP: &str = "/dev/dsp";
const SPEAKER_MUTE: &str = "/proc/sys/sense1/spkmute";
/// With no argument it prints the master volume; with --master_volume N it
/// sets it.
const SET_VOLUME: &str = "/usr/bin/chumby_set_volume";

/// Mutes or unmutes the speaker amplifier. Whatever plays unmutes it while
/// it does, and mutes it again after.
pub fn mute(mute: bool) -> io::Result<()> {
    std::fs::write(SPEAKER_MUTE, if mute { "1" } else { "0" })
}

/// The master volume (0-100), or None if it can't be read.
pub fn volume() -> Option<u32> {
    let out = Command::new(SET_VOLUME).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Sets the master volume (0-100).
pub fn set_volume(v: u32) -> io::Result<()> {
    Command::new(SET_VOLUME).arg("--master_volume").arg(v.to_string()).output().map(|_| ())
}

/// Whether the speaker is free for this process: no other live process
/// holds it or has asked for it. A process that yielded the speaker waits
/// for this before taking it again.
pub fn free() -> bool {
    Handover::new().free()
}

/// Withdraws this process's request for the speaker, if it has one: for a
/// newcomer that no longer wants it.
pub fn withdraw() {
    Handover::new().withdraw();
}

/// The speaker unmuted and at a volume while held; dropping it mutes the
/// speaker and puts the volume back. Only a drop restores them, so a process
/// killed while holding it leaves the speaker unmuted and at its volume.
pub struct Unmuted {
    // Dropped first: the volume is put back before the speaker is given up.
    _volume: Volume,
    claim: Claim,
}

impl Unmuted {
    /// Takes the speaker, then sets the master volume to `volume` and
    /// unmutes, remembering the volume to put back. If another process
    /// holds the speaker, asks it to yield and fails with
    /// [`io::ErrorKind::WouldBlock`], changing nothing: try again soon.
    pub fn at(volume: u32) -> io::Result<Unmuted> {
        let claim = Claim::take()?;
        Ok(Unmuted { _volume: Volume::at(volume), claim })
    }

    /// Whether another process has asked for the speaker: if so, stop
    /// playing and drop this.
    pub fn yield_requested(&self) -> bool {
        self.claim.yield_requested()
    }
}

/// The speaker, held: /dev/dsp set for 16-bit little-endian PCM, and the
/// speaker unmuted and at a volume while open, as [`Unmuted`]. Dropping it
/// restores the speaker, closes /dev/dsp, then gives the speaker up.
pub struct Speaker {
    // In this order: the speaker is muted before /dev/dsp closes, and
    // /dev/dsp is closed before the next player is let take it.
    _volume: Volume,
    dsp: File,
    claim: Claim,
}

impl Speaker {
    /// Takes the speaker, opens /dev/dsp for `channels` channels at `rate`
    /// Hz, then sets the master volume to `volume` and unmutes. If another
    /// process holds the speaker, asks it to yield and fails with
    /// [`io::ErrorKind::WouldBlock`]: try again soon. Fails, changing
    /// nothing, if /dev/dsp is busy (with a program outside the handover)
    /// or refuses the format.
    pub fn open(channels: u32, rate: u32, volume: u32) -> io::Result<Speaker> {
        let claim = Claim::take()?;
        let dsp = OpenOptions::new().write(true).open(DSP)?;
        for (setting, value) in
            [(Setting::Format, AFMT_S16_LE), (Setting::Channels, channels as i32), (Setting::Rate, rate as i32)]
        {
            oss::set(&dsp, setting, value)?;
        }
        Ok(Speaker { _volume: Volume::at(volume), dsp, claim })
    }

    /// Whether another process has asked for the speaker: if so, stop
    /// writing and drop this.
    pub fn yield_requested(&self) -> bool {
        self.claim.yield_requested()
    }
}

/// This process's claim on the speaker, given up when dropped.
struct Claim(Handover);

impl Claim {
    fn take() -> io::Result<Claim> {
        let h = Handover::new();
        match h.take()? {
            Take::Taken => Ok(Claim(h)),
            Take::Asked { holder } => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("the speaker is process {holder}'s; asked it to yield"),
            )),
            Take::Waiting { asker } => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("process {asker} has asked for the speaker first"),
            )),
        }
    }

    fn yield_requested(&self) -> bool {
        self.0.yield_requested()
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// The master volume set and the speaker unmuted while held; dropping it
/// mutes the speaker and puts the volume back.
struct Volume {
    before: Option<u32>,
}

impl Volume {
    fn at(volume: u32) -> Volume {
        let before = self::volume();
        report(SET_VOLUME, set_volume(volume));
        report(SPEAKER_MUTE, mute(false));
        Volume { before }
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        report(SPEAKER_MUTE, mute(true));
        if let Some(v) = self.before {
            report(SET_VOLUME, set_volume(v));
        }
    }
}

impl Write for Speaker {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.dsp.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.dsp.flush()
    }
}

// The volume and the mute are best effort: the audio plays either way.
fn report(what: &str, r: io::Result<()>) {
    if let Err(e) = r {
        eprintln!("{}: {what}: {e}", debug::name());
    }
}
