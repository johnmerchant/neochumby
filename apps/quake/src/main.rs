//! neochumby-quake: Quake on the Chumby Classic. The engine
//! (third_party/quake, C) is driven from here, and calls back the platform
//! functions a port provides (VID_*, IN_*, SNDDMA_*, Sys_*) for the screen,
//! input, sound, files and time; engine.rs mirrors its side.
//!
//! Controls, as neochumby's Doom: the screen is a 3x3 grid of keys, and
//! squeezing the case fires.
//!
//!   ESC    FWD    JUMP
//!   LEFT   FIRE   RIGHT
//!   ENTER  BACK   Y (next weapon in play: bound in autoexec.cfg)

// FFI to a C engine (GPL), so allowed unsafe code, as neochumby-sys is.
// Each unsafe block says, in its SAFETY comment, what the engine requires of
// it and why that holds. The engine is single-threaded: main() calls into it
// (Host_Init, then Host_Frame), and it calls back the platform functions
// here from that same thread, so nothing below is ever raced.
#![allow(unsafe_code)]

mod engine;
mod net;
mod sound;
mod span;
mod sys;
mod vid;

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_int, c_void};
use std::time::Instant;

use neochumby::audio::Stream;
use neochumby::fb::{self, Palette};
use neochumby::input::{Squeeze, Touch, Touchscreen};

// Quake's key numbers (keys.h).
const K_ENTER: i32 = 13;
const K_ESCAPE: i32 = 27;
const K_SPACE: i32 = 32;
const K_UPARROW: i32 = 128;
const K_DOWNARROW: i32 = 129;
const K_LEFTARROW: i32 = 130;
const K_RIGHTARROW: i32 = 131;
const K_CTRL: i32 = 133;

const GRID: [[i32; 3]; 3] =
    [[K_ESCAPE, K_UPARROW, K_SPACE], [K_LEFTARROW, K_CTRL, K_RIGHTARROW], [K_ENTER, K_DOWNARROW, b'y' as i32]];

struct State {
    fb: Option<fb::Fb>,
    palette: Palette,
    touch: Touchscreen,
    squeeze: Squeeze,
    keys: VecDeque<(i32, bool)>,
    held: Option<i32>,
    squeezed: bool,
    start: Instant,
    sound: Option<Stream>,
    /// The engine's open files, by handle (sys.rs).
    files: Vec<Option<std::fs::File>>,
    /// The answer for this host's own name, once asked (net.rs).
    own_host: Option<Box<net::OwnHost>>,
}

/// The engine is single-threaded and calls in only from its own thread.
struct Global(UnsafeCell<Option<State>>);
// SAFETY: STATE is only touched from the main thread: by main(), and by the
// engine's callbacks, which it makes from the thread that runs Host_Init and
// Host_Frame (main's). No other thread (neochumby's audio stream has its own
// state) ever reaches it, so sharing it is never actually concurrent.
unsafe impl Sync for Global {}
static STATE: Global = Global(UnsafeCell::new(None));

/// The platform layer's state. Each caller uses what this returns briefly,
/// and never across a call into the engine (which may call back in, and
/// call this again): so no two of the references it hands out are live at
/// once.
fn state() -> &'static mut State {
    // SAFETY: STATE is only reached from the main thread (Global's Sync), it
    // is set before the engine is first called (main) and never unset, and
    // by the rule above no other reference into it is live while this one
    // is used.
    unsafe { (*STATE.0.get()).as_mut().expect("neochumby-quake: state before main") }
}

fn press(s: &mut State, key: Option<i32>) {
    if s.held == key {
        return;
    }
    if let Some(k) = s.held {
        s.keys.push_back((k, false));
    }
    if let Some(k) = key {
        s.keys.push_back((k, true));
    }
    s.held = key;
}

fn refresh(s: &mut State) {
    for t in s.touch.poll() {
        let key = match t {
            Touch::Down { x, y, .. } => Some(GRID[(y as usize * 3 / fb::H).min(2)][(x as usize * 3 / fb::W).min(2)]),
            Touch::Up => None,
        };
        press(s, key);
    }
    let squeezed = s.squeeze.squeezed();
    if squeezed != s.squeezed {
        s.keys.push_back((K_CTRL, squeezed));
        s.squeezed = squeezed;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn Sys_SendKeyEvents() {
    let s = state();
    if s.keys.is_empty() {
        refresh(s);
    }
    // state() again for each key: a key may run a command (the menu's quit,
    // say) that calls back in, so none of it is borrowed across Key_Event.
    while let Some((k, d)) = state().keys.pop_front() {
        // SAFETY: Key_Event takes any key number below 256 (keys.c indexes
        // its 256-entry tables with it): ours are keys.h's, all below 256.
        // It is called on the engine's thread, between frames, as its own
        // drivers' Sys_SendKeyEvents do.
        unsafe { engine::Key_Event(k, d as engine::qboolean) };
    }
}

// The engine's input.h: all input comes as key events.
#[unsafe(no_mangle)]
pub extern "C" fn IN_Init() {}

#[unsafe(no_mangle)]
pub extern "C" fn IN_Shutdown() {}

#[unsafe(no_mangle)]
pub extern "C" fn IN_Commands() {}

#[unsafe(no_mangle)]
pub extern "C" fn IN_Move(_cmd: *mut c_void) {}

fn main() {
    neochumby::debug::install("neochumby-quake");
    // SAFETY: before any call into the engine, on the main thread, with no
    // reference into STATE yet taken (state() has never been called).
    unsafe {
        *STATE.0.get() = Some(State {
            fb: None,
            palette: Palette::new(),
            touch: Touchscreen::open(),
            squeeze: Squeeze::open(),
            keys: VecDeque::new(),
            held: None,
            squeezed: false,
            start: Instant::now(),
            sound: None,
            files: Vec::new(),
            own_host: None,
        });
    }
    let args: Vec<CString> = std::env::args().map(|a| CString::new(a).expect("argument with a NUL")).collect();
    let mut argv: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    // SAFETY: COM_InitArgv takes main()'s argc and argv, argc NUL-terminated
    // strings; it copies the pointers (into largv, com_argv) and keeps them,
    // and args and argv live as long, as main never returns. com_argv[p + 1]
    // is read only when p + 1 < com_argc, so inside largv, and Q_atof only
    // reads that string. Host_Init copies quakeparms_t (host_parms =
    // *parms); the strings and the memory it points to are static or
    // leaked, so live for the process. membase is at least memsize bytes,
    // in u64s, so aligned as malloc's are, which the hunk (zone.c) needs.
    // Host_Init, and then Host_Frame below, are called from this thread only.
    unsafe {
        engine::COM_InitArgv(args.len() as c_int, argv.as_mut_ptr());
        // Most of the RAM left; -mem <MB> overrides.
        let mut memsize = 10 * 1024 * 1024;
        let p = engine::COM_CheckParm(c"-mem".as_ptr());
        if p != 0 && p + 1 < engine::com_argc {
            memsize = (engine::Q_atof(*engine::com_argv.add(p as usize + 1)) * 1024.0 * 1024.0) as c_int;
        }
        // In u64s, for malloc's alignment; at least memsize bytes, which the
        // engine takes to be its own.
        let membase = Box::leak(vec![0u64; (memsize.max(0) as usize).div_ceil(8)].into_boxed_slice());
        let parms = Box::leak(Box::new(engine::quakeparms_t {
            basedir: c".".as_ptr(),
            cachedir: std::ptr::null(),
            argc: engine::com_argc,
            argv: engine::com_argv,
            membase: membase.as_mut_ptr().cast(),
            memsize,
        }));
        engine::Host_Init(parms);
    }
    // Quake leaves by exit() (Sys_Quit, Sys_Error).
    let mut old = sys::Sys_FloatTime();
    loop {
        let new = sys::Sys_FloatTime();
        // SAFETY: Host_Init has returned (an engine that failed to start
        // left by Sys_Error), and frames run on this thread, one at a time,
        // as the engine's own sys_*.c drive it.
        unsafe { engine::Host_Frame((new - old) as f32) };
        old = new;
    }
}
