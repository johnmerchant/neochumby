//! OSS (/dev/dsp) format settings and buffer space, from sys/soundcard.h.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

/// Signed 16-bit little-endian samples, for [`Setting::Format`].
pub const AFMT_S16_LE: i32 = 0x10;

/// What [`set`] sets; each takes an int.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// SNDCTL_DSP_SETFMT: the sample format, as an AFMT_* value.
    Format,
    /// SNDCTL_DSP_CHANNELS: the number of channels.
    Channels,
    /// SNDCTL_DSP_SPEED: the sample rate, in Hz.
    Rate,
}

impl Setting {
    fn request(self) -> libc::Ioctl {
        match self {
            Setting::Rate => 0xc004_5002u32 as libc::Ioctl,
            Setting::Format => 0xc004_5005u32 as libc::Ioctl,
            Setting::Channels => 0xc004_5006u32 as libc::Ioctl,
        }
    }
}

/// SNDCTL_DSP_GETOSPACE: _IOR('P', 12, audio_buf_info), four ints.
const GETOSPACE: u32 = 0x8010_500c;

/// The driver's playback buffer, in bytes: what is written and not yet
/// played, and what can be written without blocking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Space {
    pub queued: usize,
    pub free: usize,
}

/// `dsp`'s playback buffer (SNDCTL_DSP_GETOSPACE).
pub fn space(dsp: &File) -> io::Result<Space> {
    // audio_buf_info: fragments, fragstotal, fragsize, bytes.
    let mut info: [libc::c_int; 4] = [0; 4];
    // SAFETY: GETOSPACE writes one audio_buf_info, four ints, through the
    // pointer, and `info` is four live c_ints; the descriptor is open while
    // `dsp` is borrowed.
    if unsafe { libc::ioctl(dsp.as_raw_fd(), GETOSPACE as libc::Ioctl, info.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let [_, total, size, free] = info.map(|v| v.max(0) as usize);
    Ok(Space { queued: (total * size).saturating_sub(free), free })
}

/// Sets one of `dsp`'s settings (an ioctl on /dev/dsp). The driver may pick
/// a value near the one asked; what it picked is returned.
pub fn set(dsp: &File, setting: Setting, value: i32) -> io::Result<i32> {
    let mut v: libc::c_int = value;
    // SAFETY: the three requests each read and write one int through the
    // pointer (_IOWR('P', n, int)), and `v` is a live c_int; the descriptor
    // is open while `dsp` is borrowed.
    if unsafe { libc::ioctl(dsp.as_raw_fd(), setting.request(), &mut v) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(v)
}
