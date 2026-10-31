//! doomgeneric's platform layer (the DG_* functions it calls), on the
//! Chumby's screen, touchscreen and squeeze switch.
//!
//! Controls: the screen is a 3x3 grid of keys, labelled in the letterbox
//! bars above and below Doom's 320x200 picture; squeezing the case fires.
//!
//!   ESC    FWD   USE
//!   LEFT   FIRE  RIGHT
//!   ENTER  BACK  Y

use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_int};
use std::sync::Mutex;
use std::time::Instant;

use neochumby::fb::{self, Fb, rgb};
use neochumby::input::{Accel, Squeeze, Touch, Touchscreen};

const KEY_RIGHTARROW: u8 = 0xae;
const KEY_LEFTARROW: u8 = 0xac;
const KEY_UPARROW: u8 = 0xad;
const KEY_DOWNARROW: u8 = 0xaf;
const KEY_USE: u8 = 0xa2;
const KEY_FIRE: u8 = 0xa3;
const KEY_ESCAPE: u8 = 27;
const KEY_ENTER: u8 = 13;

const GRID: [[u8; 3]; 3] =
    [[KEY_ESCAPE, KEY_UPARROW, KEY_USE], [KEY_LEFTARROW, KEY_FIRE, KEY_RIGHTARROW], [KEY_ENTER, KEY_DOWNARROW, b'y']];

/// Doom draws 200 lines; the rest of the 240 are bars, top and bottom.
const BAR: usize = (fb::H - 200) / 2;

// build.rs builds doomgeneric for DOOMGENERIC_RESX x DOOMGENERIC_RESY =
// 320x240, so DG_ScreenBuffer holds fb::W * fb::H pixels: the unsafe blocks
// below rely on it.
const _: () = assert!(fb::W == 320 && fb::H == 240);

unsafe extern "C" {
    static mut DG_ScreenBuffer: *mut u32;
    fn doomgeneric_Create(argc: c_int, argv: *mut *mut c_char);
    fn doomgeneric_Tick();
}

struct State {
    fb: Fb,
    touch: Touchscreen,
    squeeze: Squeeze,
    accel: Option<(Accel, [i32; 3])>,
    keys: VecDeque<(bool, u8)>,
    held: Option<u8>,
    squeezed: bool,
    tilt: [Option<u8>; 2],
    start: Instant,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(STATE.lock().unwrap().as_mut().expect("DG_Init not called"))
}

pub fn run(args: &[String]) -> ! {
    let fb = Fb::open().expect("/dev/fb0");
    // Tilt steering is opt-in until the axes are known on the device: the
    // neutral pose is whatever it rests in when Doom starts.
    let accel = std::env::var_os("NEOCHUMBY_TILT").and_then(|_| {
        let mut a = Accel::open();
        let rest = a.read()?;
        Some((a, rest))
    });
    *STATE.lock().unwrap() = Some(State {
        fb,
        touch: Touchscreen::open(),
        squeeze: Squeeze::open(),
        accel,
        keys: VecDeque::new(),
        held: None,
        squeezed: false,
        tilt: [None, None],
        start: Instant::now(),
    });

    let mut argv: Vec<CString> =
        std::iter::once("doom".to_string()).chain(args.iter().cloned()).map(|a| CString::new(a).unwrap()).collect();
    let mut ptrs: Vec<*mut c_char> = argv.iter_mut().map(|a| a.as_ptr() as *mut c_char).collect();
    ptrs.push(std::ptr::null_mut());
    // SAFETY: doomgeneric_Create takes main()'s argc and argv: argc
    // NUL-terminated strings, then a null. It keeps them (myargv, which
    // M_CheckParm reads for the life of the game), and argv and ptrs live
    // as long: this function never returns. The engine is called from this
    // thread only, once created and then tick by tick, as doomgeneric.h
    // asks, and STATE is set before DG_Init, which Create calls, needs it.
    unsafe {
        doomgeneric_Create(argv.len() as c_int, ptrs.as_mut_ptr());
        // Doom quits by calling exit(): this loop does not end otherwise.
        loop {
            doomgeneric_Tick();
        }
    }
}

/// doomgeneric's frame: 320x240 XRGB8888, malloc'd by doomgeneric_Create.
fn screen_buffer() -> *mut u32 {
    // SAFETY: a read of the pointer's value, not a reference to the static.
    // It is written once, by doomgeneric_Create, on this thread (the only
    // one that calls into the engine or is called back from it).
    let buf = unsafe { DG_ScreenBuffer };
    // doomgeneric doesn't check its malloc.
    assert!(!buf.is_null(), "neochumby-doom: no memory for the screen");
    buf
}

fn draw_bars(fb: &mut Fb) {
    let bg = rgb(24, 0, 0);
    let fg = rgb(255, 200, 120);
    fb.rect(0, 0, fb::W, BAR, bg);
    fb.rect(0, fb::H - BAR, fb::W, BAR, bg);
    let label = |fb: &mut Fb, y: usize, names: [&str; 3]| {
        for (i, n) in names.iter().enumerate() {
            let x = i * fb::W / 3 + (fb::W / 3 - Fb::text_width(n, 1)) / 2;
            fb.text(x, y, n, 1, fg);
        }
    };
    label(fb, (BAR - 8) / 2, ["ESC", "FWD", "USE"]);
    label(fb, fb::H - BAR + (BAR - 8) / 2, ["ENTER", "BACK", "Y"]);
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_Init() {
    with(|s| {
        // doomgeneric mallocs the buffer and leaves the bars unwritten.
        // SAFETY: doomgeneric_Create calls DG_Init right after it mallocs
        // the buffer, DOOMGENERIC_RESX * DOOMGENERIC_RESY * 4 bytes: fb::W *
        // fb::H u32s (asserted above), suitably aligned, not null
        // (screen_buffer). Zeroes are valid u32s, and nothing else
        // touches the buffer during the call.
        unsafe { std::ptr::write_bytes(screen_buffer(), 0, fb::W * fb::H) };
        draw_bars(&mut s.fb);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_DrawFrame() {
    with(|s| {
        // SAFETY: the buffer is fb::W * fb::H u32s (DG_Init), initialised
        // (zeroed there, then drawn by the engine), and is never freed. The
        // engine calls DG_DrawFrame from I_FinishUpdate once it has copied
        // the frame in, and doesn't write it again until this returns: the
        // shared slice is the only access while it lives.
        let src = unsafe { std::slice::from_raw_parts(screen_buffer(), fb::W * fb::H) };
        let dst = s.fb.pixels();
        // XRGB8888 to RGB565, Doom's 200 lines only: the bars are ours.
        for i in BAR * fb::W..(fb::H - BAR) * fb::W {
            let p = src[i];
            dst[i] = (((p >> 8) & 0xf800) | ((p >> 5) & 0x07e0) | ((p >> 3) & 0x001f)) as u16;
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_SleepMs(ms: u32) {
    neochumby::sleep_ms(ms);
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_GetTicksMs() -> u32 {
    with(|s| s.start.elapsed().as_millis() as u32)
}

fn press(s: &mut State, key: Option<u8>) {
    if s.held == key {
        return;
    }
    if let Some(k) = s.held {
        s.keys.push_back((false, k));
    }
    if let Some(k) = key {
        s.keys.push_back((true, k));
    }
    s.held = key;
}

fn tilt(s: &mut State) {
    // Past roughly 15 degrees (a quarter g), a direction is held.
    const DEAD: i32 = 200;
    let Some((accel, rest)) = s.accel.as_mut() else { return };
    let Some(now) = accel.read() else { return };
    let axis = |d: i32, neg: u8, pos: u8| match d {
        d if d < -DEAD => Some(neg),
        d if d > DEAD => Some(pos),
        _ => None,
    };
    let want =
        [axis(now[0] - rest[0], KEY_LEFTARROW, KEY_RIGHTARROW), axis(now[1] - rest[1], KEY_UPARROW, KEY_DOWNARROW)];
    for (held, want) in s.tilt.iter_mut().zip(want) {
        if *held != want {
            if let Some(k) = *held {
                s.keys.push_back((false, k));
            }
            if let Some(k) = want {
                s.keys.push_back((true, k));
            }
            *held = want;
        }
    }
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
        s.keys.push_back((squeezed, KEY_FIRE));
        s.squeezed = squeezed;
    }
    tilt(s);
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_GetKey(pressed: *mut c_int, key: *mut u8) -> c_int {
    with(|s| {
        if s.keys.is_empty() {
            refresh(s);
        }
        match s.keys.pop_front() {
            Some((p, k)) => {
                // SAFETY: the engine's one caller, I_GetEvent (i_input.c),
                // passes pointers to its own locals, an int and an unsigned
                // char: valid, aligned and writable for the call.
                unsafe {
                    *pressed = p as c_int;
                    *key = k;
                }
                1
            }
            None => 0,
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn DG_SetWindowTitle(_title: *const c_char) {}
