//! Quake's sound: the engine (snd_dma.c) mixes into a ring as if a sound
//! card played it by DMA, and the SNDDMA_* functions here play it on a
//! neochumby audio stream instead: SNDDMA_Submit writes out what has been
//! mixed since the last, and SNDDMA_GetDMAPos says how far the speaker has
//! got. 11025 Hz, the rate of Quake's own sounds, so they load without
//! resampling.

use std::ffi::c_int;

use neochumby::audio::Stream;

use crate::engine::{self, qboolean, shm, sn};
use crate::state;

const RATE: c_int = 11025;
const CHANNELS: c_int = 2;
/// Mono samples, a power of two: three seconds of stereo.
const SAMPLES: usize = 1 << 16;
/// The master volume while Quake plays; the game's own volume is applied in
/// the mix.
const VOLUME: u32 = 80;

/// The engine's ring: samples of 16-bit stereo, kept for the life of the
/// process.
fn ring() -> &'static [i16] {
    unsafe { std::slice::from_raw_parts((*shm).buffer.cast::<i16>(), (*shm).samples as usize) }
}

#[unsafe(no_mangle)]
pub extern "C" fn SNDDMA_Init() -> qboolean {
    let ring: &'static mut [i16] = Box::leak(vec![0; SAMPLES].into_boxed_slice());
    unsafe {
        let d = &raw mut sn;
        (*d).splitbuffer = 0;
        (*d).samplebits = 16;
        (*d).channels = CHANNELS;
        (*d).speed = RATE;
        (*d).samples = SAMPLES as c_int;
        (*d).samplepos = 0;
        (*d).submission_chunk = 1;
        (*d).buffer = ring.as_mut_ptr().cast();
        shm = d;
    }
    state().sound = Some(Stream::new(CHANNELS as u32, RATE as u32, VOLUME));
    1
}

/// Where the speaker has got in the ring, from the frames played: the
/// engine counts its passes of the ring, calling this many times a pass.
#[unsafe(no_mangle)]
pub extern "C" fn SNDDMA_GetDMAPos() -> c_int {
    let played = state().sound.as_mut().map_or(0, |s| s.played());
    unsafe {
        (*shm).samplepos = (played as c_int).wrapping_mul(CHANNELS) & ((*shm).samples - 1);
        (*shm).samplepos
    }
}

/// Writes the frames the engine has mixed (up to its paintedtime) since the
/// last call.
#[unsafe(no_mangle)]
pub extern "C" fn SNDDMA_Submit() {
    let Some(s) = state().sound.as_mut() else { return };
    let ring = ring();
    let frames = ring.len() / 2;
    let painted = unsafe { engine::paintedtime };
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
pub extern "C" fn SNDDMA_Shutdown() {
    state().sound = None;
}
