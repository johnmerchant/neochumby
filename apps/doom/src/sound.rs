//! Doom's sound module (i_sound.h's sound_module_t), with doomgeneric built
//! for FEATURE_SOUND: the effects' lumps are read here and mixed, in
//! integers, onto a neochumby audio stream a little ahead of what plays.
//! Music has no player yet: DG_music_module takes the songs and plays
//! nothing.

use std::ffi::{c_char, c_float, c_int, c_uint, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

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

// ---- the engine's side (doomtype.h, i_sound.h, w_wad.h, z_zone.h) ----

/// doomtype.h's boolean, an enum the size of an int.
type Boolean = c_uint;

/// i_sound.h's sfxinfo_t.
#[repr(C)]
struct SfxInfo {
    tagname: *mut c_char,
    name: [c_char; 9],
    priority: c_int,
    link: *mut SfxInfo,
    pitch: c_int,
    volume: c_int,
    usefulness: c_int,
    lumpnum: c_int,
    numchannels: c_int,
    driver_data: *mut c_void,
}

/// i_sound.h's sound_module_t.
#[repr(C)]
pub struct SoundModule {
    sound_devices: *const c_int,
    num_sound_devices: c_int,
    init: extern "C" fn(Boolean) -> Boolean,
    shutdown: extern "C" fn(),
    get_sfx_lump_num: extern "C" fn(*mut SfxInfo) -> c_int,
    update: extern "C" fn(),
    update_sound_params: extern "C" fn(c_int, c_int, c_int),
    start_sound: extern "C" fn(*mut SfxInfo, c_int, c_int, c_int) -> c_int,
    stop_sound: extern "C" fn(c_int),
    sound_is_playing: extern "C" fn(c_int) -> Boolean,
    cache_sounds: extern "C" fn(*mut SfxInfo, c_int),
}

/// i_sound.h's music_module_t.
#[repr(C)]
pub struct MusicModule {
    sound_devices: *const c_int,
    num_sound_devices: c_int,
    init: extern "C" fn() -> Boolean,
    shutdown: extern "C" fn(),
    set_music_volume: extern "C" fn(c_int),
    pause_music: extern "C" fn(),
    resume_music: extern "C" fn(),
    register_song: extern "C" fn(*mut c_void, c_int) -> *mut c_void,
    unregister_song: extern "C" fn(*mut c_void),
    play_song: extern "C" fn(*mut c_void, Boolean),
    stop_song: extern "C" fn(),
    music_is_playing: extern "C" fn() -> Boolean,
    poll: Option<extern "C" fn()>,
}

// SAFETY: the modules are immutable statics: function pointers, and a
// pointer to DEVICES, an immutable static too. Neither Rust nor the engine
// (i_sound.c only reads them, through sound_modules[]) writes them, so
// sharing them between threads is sharing read-only data.
unsafe impl Sync for SoundModule {}
// SAFETY: as for SoundModule.
unsafe impl Sync for MusicModule {}

/// z_zone.h's PU_STATIC.
const PU_STATIC: c_int = 1;

unsafe extern "C" {
    fn W_GetNumForName(name: *mut c_char) -> c_int;
    fn W_LumpLength(lump: c_uint) -> c_int;
    fn W_CacheLumpNum(lump: c_int, tag: c_int) -> *mut c_void;
    fn W_ReleaseLumpNum(lump: c_int);
}

/// snddevice_t: the cards Doom's setup offers for effects and music.
static DEVICES: [c_int; 6] = [
    3, // SNDDEVICE_SB
    4, // SNDDEVICE_PAS
    5, // SNDDEVICE_GUS
    6, // SNDDEVICE_WAVEBLASTER
    7, // SNDDEVICE_SOUNDCANVAS
    9, // SNDDEVICE_AWE32
];

#[unsafe(no_mangle)]
pub static DG_sound_module: SoundModule = SoundModule {
    sound_devices: DEVICES.as_ptr(),
    num_sound_devices: DEVICES.len() as c_int,
    init,
    shutdown,
    get_sfx_lump_num,
    update,
    update_sound_params,
    start_sound,
    stop_sound,
    sound_is_playing,
    cache_sounds,
};

#[unsafe(no_mangle)]
pub static DG_music_module: MusicModule = MusicModule {
    sound_devices: DEVICES.as_ptr(),
    num_sound_devices: DEVICES.len() as c_int,
    init: music_init,
    shutdown: nothing,
    set_music_volume: music_volume,
    pause_music: nothing,
    resume_music: nothing,
    register_song: music_register,
    unregister_song: music_unregister,
    play_song: music_play,
    stop_song: nothing,
    music_is_playing: music_playing,
    poll: None,
};

// m_config.c binds these with FEATURE_SOUND; there is no libsamplerate.
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static mut use_libsamplerate: c_int = 0;
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static mut libsamplerate_scale: c_float = 0.65;

// ---- the effects ----

/// An effect's 8-bit unsigned samples, kept for the life of the process
/// (in its sfxinfo_t's driver_data) once first played.
struct Effect {
    rate: u32,
    samples: Box<[u8]>,
}

impl Effect {
    /// As Chocolate Doom reads a DMX lump: format 3, a 16-bit rate and a
    /// 32-bit length, then the samples, of which DMX skips the first and
    /// last 16. None for a lump that isn't one, as DMX plays nothing for it.
    fn parse(lump: &[u8]) -> Option<Effect> {
        let [3, 0, r0, r1, l0, l1, l2, l3, ..] = *lump else { return None };
        let length = u32::from_le_bytes([l0, l1, l2, l3]) as usize;
        if length > lump.len() - 8 || length <= 48 {
            return None;
        }
        Some(Effect { rate: u16::from_le_bytes([r0, r1]) as u32, samples: lump[24..length - 8].into() })
    }

    fn of(sfx: &mut SfxInfo) -> Option<&'static Effect> {
        if !sfx.driver_data.is_null() {
            // SAFETY: driver_data is the engine's for the sound module, and
            // null (sounds.c's SOUND()) until this sets it, below, to an
            // Effect leaked for the life of the process: a valid Effect,
            // never freed, never written again.
            return Some(unsafe { &*sfx.driver_data.cast::<Effect>() });
        }
        // S_StartSound sets lumpnum (I_GetSfxLumpNum) before it starts the
        // sound, so it is a lump of the WAD's.
        let lump = sfx.lumpnum;
        // SAFETY: W_LumpLength takes any lump number: one out of range is
        // an I_Error, which exits, not undefined behaviour.
        let len = unsafe { W_LumpLength(lump as c_uint) }.max(0) as usize;
        // SAFETY: as W_LumpLength; with PU_STATIC, the lump is read in (or
        // found in the mapped WAD) and stays put until released, below.
        // Z_Malloc's failure is an I_Error, so the pointer isn't null.
        let data = unsafe { W_CacheLumpNum(lump, PU_STATIC) }.cast::<u8>();
        // SAFETY: data is the lump's W_LumpLength bytes (w_wad.c), cached
        // PU_STATIC so that the zone can't purge it while the slice lives;
        // the slice is only read, by parse, which copies out the samples
        // before the lump is released.
        let effect = Effect::parse(unsafe { std::slice::from_raw_parts(data, len) });
        // SAFETY: the lump was cached above, and nothing refers to its data
        // any more: Effect owns a copy.
        unsafe { W_ReleaseLumpNum(lump) };
        let effect: &'static mut Effect = Box::leak(Box::new(effect?));
        sfx.driver_data = (effect as *mut Effect).cast();
        Some(effect)
    }
}

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
/// Doom's lumps are named DS*; Heretic's and Hexen's aren't.
static PREFIX: AtomicBool = AtomicBool::new(false);

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

// ---- the module's functions ----

extern "C" fn init(use_sfx_prefix: Boolean) -> Boolean {
    PREFIX.store(use_sfx_prefix != 0, Ordering::Relaxed);
    *MIXER.lock().unwrap() = Some(Mixer {
        stream: Stream::new(2, RATE, VOLUME),
        voices: Default::default(),
        acc: Vec::new(),
        out: Vec::new(),
    });
    1
}

extern "C" fn shutdown() {
    *MIXER.lock().unwrap() = None;
}

extern "C" fn get_sfx_lump_num(sfx: *mut SfxInfo) -> c_int {
    // SAFETY: the engine passes an entry of its S_sfx array (s_sound.c's
    // S_StartSound, through I_GetSfxLumpNum): a valid sfxinfo_t, laid out as
    // SfxInfo (i_sound.h), static, and not touched by the engine while this
    // runs.
    let mut sfx = unsafe { &*sfx };
    if !sfx.link.is_null() {
        // SAFETY: a link is another entry of S_sfx (sounds.c's
        // SOUND_LINK), as valid as the first.
        sfx = unsafe { &*sfx.link };
    }
    // As M_snprintf into a 9-byte buffer: at most 8 characters.
    let mut name: Vec<u8> = if PREFIX.load(Ordering::Relaxed) { b"ds".to_vec() } else { Vec::new() };
    // c_char is u8 on ARM, i8 elsewhere.
    #[allow(clippy::unnecessary_cast)]
    name.extend(sfx.name.iter().take_while(|&&c| c != 0).map(|&c| c as u8));
    name.truncate(8);
    name.push(0);
    // SAFETY: name is NUL-terminated, at most 9 bytes as the engine's own
    // lump names, and lives through the call; W_GetNumForName only reads it
    // (a name not found is an I_Error, which exits).
    unsafe { W_GetNumForName(name.as_mut_ptr().cast()) }
}

extern "C" fn update() {
    with(|m| {
        let want = (m.stream.played() + AHEAD).saturating_sub(m.stream.written());
        if want > 0 {
            m.mix(want as usize);
            m.stream.write(&m.out);
        }
    });
}

extern "C" fn start_sound(sfx: *mut SfxInfo, channel: c_int, vol: c_int, sep: c_int) -> c_int {
    // SAFETY: as in get_sfx_lump_num: an entry of S_sfx, which the engine
    // (single-threaded, in S_StartSound) doesn't touch while this runs, so
    // the one mutable reference is unaliased.
    let Some(effect) = Effect::of(unsafe { &mut *sfx }) else { return -1 };
    let step = ((effect.rate.max(1) as u64) << 16) / RATE as u64;
    let mut started = -1;
    with(|m| {
        if let Some(v) = voice(m, channel) {
            let mut new = Voice { samples: &effect.samples, pos: 0, step: step as u32, left: 0, right: 0 };
            new.set(vol, sep);
            *v = Some(new);
            started = channel;
        }
    });
    started
}

extern "C" fn update_sound_params(channel: c_int, vol: c_int, sep: c_int) {
    with(|m| {
        if let Some(Some(v)) = voice(m, channel) {
            v.set(vol, sep);
        }
    });
}

extern "C" fn stop_sound(channel: c_int) {
    with(|m| {
        if let Some(v) = voice(m, channel) {
            *v = None;
        }
    });
}

extern "C" fn sound_is_playing(channel: c_int) -> Boolean {
    let mut playing = false;
    with(|m| playing = matches!(voice(m, channel), Some(Some(_))));
    playing as Boolean
}

/// Effects are read as they are first played.
extern "C" fn cache_sounds(_: *mut SfxInfo, _: c_int) {}

extern "C" fn music_init() -> Boolean {
    1
}

extern "C" fn music_volume(_: c_int) {}

extern "C" fn music_register(_: *mut c_void, _: c_int) -> *mut c_void {
    std::ptr::null_mut()
}

extern "C" fn music_unregister(_: *mut c_void) {}

extern "C" fn music_play(_: *mut c_void, _: Boolean) {}

extern "C" fn music_playing() -> Boolean {
    0
}

extern "C" fn nothing() {}
