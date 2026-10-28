//! neochumby-quake: Quake on the Chumby Classic. The engine
//! (third_party/quake, C) calls the chum_* functions here, through
//! csrc/glue.c, for the screen, time and input.
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

mod span;

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_int, c_uchar};
use std::io::Write;
use std::time::Instant;

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
}

/// The engine is single-threaded and calls in only from its own thread.
struct Global(UnsafeCell<Option<State>>);
unsafe impl Sync for Global {}
static STATE: Global = Global(UnsafeCell::new(None));

fn state() -> &'static mut State {
    unsafe { (*STATE.0.get()).as_mut().expect("neochumby-quake: state before main") }
}

unsafe extern "C" {
    fn chum_quake_main(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

#[unsafe(no_mangle)]
pub extern "C" fn chum_fb_init() {
    let s = state();
    match fb::Fb::open() {
        Ok(f) => s.fb = Some(f),
        Err(e) => eprintln!("neochumby-quake: /dev/fb0: {e}"),
    }
}

/// # Safety
/// `pal` points to 256 RGB triples (768 bytes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chum_fb_palette(pal: *const c_uchar) {
    state().palette.set_rgb(unsafe { std::slice::from_raw_parts(pal, 768) });
}

/// # Safety
/// `buf` points to `height` lines of `rowbytes` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chum_fb_present(buf: *const c_uchar, width: c_int, height: c_int, rowbytes: c_int) {
    let s = state();
    if let Some(f) = s.fb.as_mut() {
        let (w, h, r) = (width as usize, height as usize, rowbytes as usize);
        let src = unsafe { std::slice::from_raw_parts(buf, r * h) };
        f.blit_indexed(&s.palette, src, w, h, r);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn chum_time() -> f64 {
    state().start.elapsed().as_secs_f64()
}

/// # Safety
/// `msg` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chum_log(msg: *const c_char) {
    let s = unsafe { CStr::from_ptr(msg) };
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(s.to_bytes());
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

/// # Safety
/// `key` and `down` point to writable ints.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chum_poll_key(key: *mut c_int, down: *mut c_int) -> c_int {
    let s = state();
    if s.keys.is_empty() {
        refresh(s);
    }
    match s.keys.pop_front() {
        Some((k, d)) => {
            unsafe {
                *key = k;
                *down = d as c_int;
            }
            1
        }
        None => 0,
    }
}

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
        });
    }
    let args: Vec<CString> = std::env::args().map(|a| CString::new(a).expect("argument with a NUL")).collect();
    let mut argv: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    // Quake leaves by exit() (Sys_Quit, Sys_Error).
    unsafe { chum_quake_main(args.len() as c_int, argv.as_mut_ptr()) };
}
