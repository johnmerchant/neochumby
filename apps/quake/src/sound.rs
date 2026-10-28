//! Quake's sound: the engine (snd_dma.c) mixes into a ring as if a sound
//! card played it, and csrc/glue.c's SNDDMA_* functions call these to play
//! it on a neochumby audio stream instead.

use std::ffi::{c_int, c_short, c_uint};

use neochumby::audio::Stream;

use crate::state;

/// The master volume while Quake plays; the game's own volume is applied in
/// the mix.
const VOLUME: u32 = 80;

#[unsafe(no_mangle)]
pub extern "C" fn chum_snd_init(channels: c_int, rate: c_int) -> c_int {
    state().sound = Some(Stream::new(channels as u32, rate as u32, VOLUME));
    rate
}

/// Frames played, which the engine counts as its sound time: the low bits
/// are enough, as it calls this many times a pass of its ring.
#[unsafe(no_mangle)]
pub extern "C" fn chum_snd_played() -> c_uint {
    state().sound.as_mut().map_or(0, |s| s.played() as c_uint)
}

/// Writes the frames the engine has mixed up to `painted` (its paintedtime)
/// since the last call.
///
/// # Safety
/// `ring` points to `samples` 16-bit stereo samples, `samples` a power of
/// two.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chum_snd_submit(ring: *const c_short, samples: c_int, painted: c_int) {
    let Some(s) = state().sound.as_mut() else { return };
    let ring = unsafe { std::slice::from_raw_parts(ring, samples as usize) };
    let frames = ring.len() / 2;
    let count = (painted as u32).wrapping_sub(s.written() as u32) as i32;
    // Nothing new, or more than the engine mixes ahead (its time is reset
    // after a day or so of play, which leaves the sound off): not the ring.
    if count <= 0 || count as usize > frames / 2 {
        return;
    }
    let start = s.written() as usize % frames;
    let end = start + count as usize;
    s.write(&ring[start * 2..end.min(frames) * 2]);
    if end > frames {
        s.write(&ring[..(end - frames) * 2]);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn chum_snd_shutdown() {
    state().sound = None;
}
