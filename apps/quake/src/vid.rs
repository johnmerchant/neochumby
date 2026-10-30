//! The engine's VID_* functions (vid.h), after id's vid_null.c: Quake's
//! 8-bit picture, in a buffer of ours, drawn on the Chumby's screen through
//! its palette.

use std::ffi::{c_int, c_uchar, c_void};

use neochumby::fb;

use crate::engine::{self, vid, vrect_t};
use crate::state;

const WIDTH: usize = 320;
const HEIGHT: usize = 240;

/// The engine's buffers, allocated once and kept for the life of the
/// process.
fn leak<T: Clone>(len: usize, v: T) -> *mut T {
    Box::leak(vec![v; len].into_boxed_slice()).as_mut_ptr()
}

/// # Safety
/// `palette` points to 256 RGB triples (768 bytes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn VID_SetPalette(palette: *const c_uchar) {
    state().palette.set_rgb(unsafe { std::slice::from_raw_parts(palette, 768) });
}

/// # Safety
/// As VID_SetPalette.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn VID_ShiftPalette(palette: *const c_uchar) {
    unsafe { VID_SetPalette(palette) };
}

/// # Safety
/// As VID_SetPalette; the engine's host_colormap is loaded.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn VID_Init(palette: *const c_uchar) {
    let v = &raw mut vid;
    unsafe {
        let buffer = leak(WIDTH * HEIGHT, 0u8);
        (*v).width = WIDTH as u32;
        (*v).height = HEIGHT as u32;
        (*v).conwidth = WIDTH as u32;
        (*v).conheight = HEIGHT as u32;
        (*v).maxwarpwidth = WIDTH as c_int;
        (*v).maxwarpheight = HEIGHT as c_int;
        // Square pixels on a 320x240 panel.
        (*v).aspect = (HEIGHT as f32 / WIDTH as f32) * (320.0 / 240.0);
        (*v).numpages = 1;
        (*v).colormap = engine::host_colormap;
        // The colormap's 2048th int (little-endian) is where the
        // fullbright colours start.
        let at = std::slice::from_raw_parts(engine::host_colormap.add(2048 * 4), 4);
        (*v).fullbright = 256 - i32::from_le_bytes([at[0], at[1], at[2], at[3]]);
        (*v).buffer = buffer;
        (*v).conbuffer = buffer;
        (*v).rowbytes = WIDTH as u32;
        (*v).conrowbytes = WIDTH as c_int;

        engine::d_pzbuffer = leak(WIDTH * HEIGHT, 0i16);
        let size = engine::D_SurfaceCacheForRes(WIDTH as c_int, HEIGHT as c_int);
        engine::D_InitCaches(leak(size.max(0) as usize, 0u8).cast::<c_void>(), size);
    }
    match fb::Fb::open() {
        Ok(f) => state().fb = Some(f),
        Err(e) => eprintln!("neochumby-quake: /dev/fb0: {e}"),
    }
    unsafe { VID_SetPalette(palette) };
}

#[unsafe(no_mangle)]
pub extern "C" fn VID_Shutdown() {}

#[unsafe(no_mangle)]
pub extern "C" fn VID_Update(_rects: *mut vrect_t) {
    let s = state();
    let Some(f) = s.fb.as_mut() else { return };
    let v = &raw const vid;
    let (w, h, r) = unsafe { ((*v).width as usize, (*v).height as usize, (*v).rowbytes as usize) };
    let src = unsafe { std::slice::from_raw_parts((*v).buffer, r * h) };
    f.blit_indexed(&s.palette, src, w, h, r);
}

#[unsafe(no_mangle)]
pub extern "C" fn D_BeginDirectRect(_x: c_int, _y: c_int, _pbitmap: *mut u8, _width: c_int, _height: c_int) {}

#[unsafe(no_mangle)]
pub extern "C" fn D_EndDirectRect(_x: c_int, _y: c_int, _width: c_int, _height: c_int) {}
