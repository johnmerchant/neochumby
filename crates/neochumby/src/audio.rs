//! The Chumby Classic's speaker: /dev/dsp (OSS), the amplifier's mute
//! (/proc/sys/sense1/spkmute; the boot leaves it on) and the stock
//! firmware's master volume (0-100, through /usr/bin/chumby_set_volume).
//!
//! /dev/dsp can be open in only one process at a time: while another has
//! it, opening it fails, and a service should drop its audio until it is
//! free rather than wait for it.
//!
//! [`Unmuted`] is the volume and the mute alone, for a process that has
//! another program play (the stock btplay, which opens /dev/dsp itself);
//! [`Speaker`] is that plus /dev/dsp, for one that writes PCM itself.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::process::Command;

use crate::debug;

// sys/soundcard.h
const SNDCTL_DSP_SPEED: libc::Ioctl = 0xc004_5002u32 as libc::Ioctl;
const SNDCTL_DSP_SETFMT: libc::Ioctl = 0xc004_5005u32 as libc::Ioctl;
const SNDCTL_DSP_CHANNELS: libc::Ioctl = 0xc004_5006u32 as libc::Ioctl;
const AFMT_S16_LE: libc::c_int = 0x10;

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

/// The speaker unmuted and at a volume while held; dropping it mutes the
/// speaker and puts the volume back. Only a drop restores them, so a process
/// killed while holding it leaves the speaker unmuted and at its volume.
pub struct Unmuted {
    volume_before: Option<u32>,
}

impl Unmuted {
    /// Sets the master volume to `volume` and unmutes, remembering the
    /// volume to put back.
    pub fn at(volume: u32) -> Unmuted {
        let volume_before = self::volume();
        report(SET_VOLUME, set_volume(volume));
        report(SPEAKER_MUTE, mute(false));
        Unmuted { volume_before }
    }
}

impl Drop for Unmuted {
    fn drop(&mut self) {
        report(SPEAKER_MUTE, mute(true));
        if let Some(v) = self.volume_before {
            report(SET_VOLUME, set_volume(v));
        }
    }
}

/// The speaker, held: /dev/dsp set for 16-bit little-endian PCM, and
/// [`Unmuted`] while open. Dropping it restores the speaker, then closes
/// /dev/dsp.
pub struct Speaker {
    // Dropped first: the speaker is muted before /dev/dsp closes.
    _unmuted: Unmuted,
    dsp: File,
}

impl Speaker {
    /// Opens /dev/dsp for `channels` channels at `rate` Hz, then sets the
    /// master volume to `volume` and unmutes. Fails, changing nothing, if
    /// /dev/dsp is busy or refuses the format.
    pub fn open(channels: u32, rate: u32, volume: u32) -> io::Result<Speaker> {
        let dsp = OpenOptions::new().write(true).open(DSP)?;
        for (req, value) in [
            (SNDCTL_DSP_SETFMT, AFMT_S16_LE),
            (SNDCTL_DSP_CHANNELS, channels as libc::c_int),
            (SNDCTL_DSP_SPEED, rate as libc::c_int),
        ] {
            let mut v = value;
            if unsafe { libc::ioctl(dsp.as_raw_fd(), req, &mut v) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Speaker { _unmuted: Unmuted::at(volume), dsp })
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
