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

// FFI to a C engine (GPL), so allowed unsafe code, as neochumby-sys is. Its
// unsafe blocks follow the engine's contract and carry no SAFETY comments.
#![allow(unsafe_code)]
#![allow(clippy::undocumented_unsafe_blocks)]

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
}

/// The engine is single-threaded and calls in only from its own thread.
struct Global(UnsafeCell<Option<State>>);
unsafe impl Sync for Global {}
static STATE: Global = Global(UnsafeCell::new(None));

fn state() -> &'static mut State {
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
    while let Some((k, d)) = s.keys.pop_front() {
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
        });
    }
    let args: Vec<CString> = std::env::args().map(|a| CString::new(a).expect("argument with a NUL")).collect();
    let mut argv: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    unsafe {
        engine::COM_InitArgv(args.len() as c_int, argv.as_mut_ptr());
        // Most of the RAM left once the stock UI is gone; -mem <MB> overrides.
        let mut memsize = 10 * 1024 * 1024;
        let p = engine::COM_CheckParm(c"-mem".as_ptr());
        if p != 0 && p + 1 < engine::com_argc {
            memsize = (engine::Q_atof(*engine::com_argv.add(p as usize + 1)) * 1024.0 * 1024.0) as c_int;
        }
        // In u64s, for malloc's alignment.
        let membase = Box::leak(vec![0u64; memsize.max(0) as usize / 8].into_boxed_slice());
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
        unsafe { engine::Host_Frame((new - old) as f32) };
        old = new;
    }
}
