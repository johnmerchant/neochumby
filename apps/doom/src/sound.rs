//! Doom's sound effects: csrc/sound.c (the engine's sound module) hands
//! each one here as 8-bit samples, and they are mixed, in integers, onto a
//! neochumby audio stream a little ahead of what plays.

use std::ffi::c_int;
use std::sync::Mutex;

use neochumby::audio::Stream;

/// Doom's effects are mostly 11025 Hz; others are stepped to it.
const RATE: u32 = 11025;
/// The master volume while Doom plays; the game's own sfx volume is applied
/// in the mix.
const VOLUME: u32 = 80;
/// How far ahead of the speaker to mix: more than a frame at the Chumby's
/// slowest, so the speaker doesn't run dry between frames.
const AHEAD: u64 = RATE as u64 / 6;
/// s_sound's snd_channels is 8 unless the config says otherwise.
const VOICES: usize = 16;

struct Voice {
    samples: &'static [u8],
    /// The next sample, in 16.16.
    pos: u32,
    step: u32,
    left: i32,
    right: i32,
}

impl Voice {
    fn set(&mut self, vol: c_int, sep: c_int) {
        // As Chocolate Doom pans: each side 0-254.
        let (vol, sep) = (vol.clamp(0, 127), sep.clamp(0, 254));
        self.left = (254 - sep) * vol / 127;
        self.right = sep * vol / 127;
    }

    fn done(&self) -> bool {
        (self.pos >> 16) as usize >= self.samples.len()
    }
}

struct Mixer {
    stream: Stream,
    voices: [Option<Voice>; VOICES],
    acc: Vec<[i32; 2]>,
    out: Vec<i16>,
}

static MIXER: Mutex<Option<Mixer>> = Mutex::new(None);

fn with(f: impl FnOnce(&mut Mixer)) {
    if let Some(m) = MIXER.lock().unwrap().as_mut() {
        f(m)
    }
}

fn voice(m: &mut Mixer, channel: c_int) -> Option<&mut Option<Voice>> {
    m.voices.get_mut(usize::try_from(channel).ok()?)
}

impl Mixer {
    fn mix(&mut self, frames: usize) {
        self.acc.clear();
        self.acc.resize(frames, [0, 0]);
        for v in self.voices.iter_mut() {
            let Some(voice) = v else { continue };
            for f in self.acc.iter_mut() {
                let Some(&s) = voice.samples.get((voice.pos >> 16) as usize) else { break };
                let s = s as i32 - 128;
                f[0] += s * voice.left;
                f[1] += s * voice.right;
                voice.pos += voice.step;
            }
            if voice.done() {
                *v = None;
            }
        }
        self.out.clear();
        self.out.extend(self.acc.iter().flatten().map(|&s| s.clamp(i16::MIN as i32, i16::MAX as i32) as i16));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn chum_sound_init() {
    *MIXER.lock().unwrap() = Some(Mixer {
        stream: Stream::new(2, RATE, VOLUME),
        voices: Default::default(),
        acc: Vec::new(),
        out: Vec::new(),
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn chum_sound_shutdown() {
    *MIXER.lock().unwrap() = None;
}

#[unsafe(no_mangle)]
pub extern "C" fn chum_sound_update() {
    with(|m| {
        let want = (m.stream.played() + AHEAD).saturating_sub(m.stream.written());
        if want > 0 {
            m.mix(want as usize);
            m.stream.write(&m.out);
        }
    });
}

/// # Safety
/// `samples` points to `length` bytes that live as long as the process.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chum_sound_start(
    channel: c_int,
    samples: *const u8,
    length: c_int,
    rate: c_int,
    vol: c_int,
    sep: c_int,
) {
    let samples = unsafe { std::slice::from_raw_parts(samples, length.max(0) as usize) };
    let step = ((rate.max(1) as u64) << 16) / RATE as u64;
    with(|m| {
        if let Some(v) = voice(m, channel) {
            let mut new = Voice { samples, pos: 0, step: step as u32, left: 0, right: 0 };
            new.set(vol, sep);
            *v = Some(new);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn chum_sound_params(channel: c_int, vol: c_int, sep: c_int) {
    with(|m| {
        if let Some(Some(v)) = voice(m, channel) {
            v.set(vol, sep);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn chum_sound_stop(channel: c_int) {
    with(|m| {
        if let Some(v) = voice(m, channel) {
            *v = None;
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn chum_sound_playing(channel: c_int) -> c_int {
    let mut playing = false;
    with(|m| playing = matches!(voice(m, channel), Some(Some(_))));
    playing as c_int
}
