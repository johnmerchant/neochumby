//! The engine's VID_* functions (vid.h), after id's vid_null.c: Quake's
//! 8-bit picture, in a buffer of ours, drawn on the Chumby's screen through
//! its palette.

use std::ffi::{c_int, c_uchar, c_void};

use neochumby::fb;

use crate::engine::{self, Sys_Error, vid, vrect_t};
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
    // SAFETY: palette is 768 readable bytes, as the caller promises: the
    // engine passes host_basepal (gfx/palette.lmp) or view.c's own pal[768],
    // neither written while this runs.
    state().palette.set_rgb(unsafe { std::slice::from_raw_parts(palette, 768) });
}

/// # Safety
/// As VID_SetPalette.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn VID_ShiftPalette(palette: *const c_uchar) {
    // SAFETY: as the caller promises, the same.
    unsafe { VID_SetPalette(palette) };
}

/// # Safety
/// As VID_SetPalette; the engine's host_colormap is loaded.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn VID_Init(palette: *const c_uchar) {
    let v = &raw mut vid;
    // SAFETY: vid is the engine's viddef_t, defined in engine.rs; the
    // engine reads it only after VID_Init returns, on this thread, and Rust
    // takes no reference to it, only raw pointers, so these writes alias
    // nothing. host_colormap is gfx/colormap.lmp, which host.c loads just
    // before (or Sys_Errors): the file it last loaded, so com_filesize is
    // its length, checked to cover the int read past 2048 ints as id's
    // drivers read it. The buffers are leaked, so live for the process, at
    // the sizes the engine is told: WIDTH x HEIGHT pixels and z values, and
    // the surface cache D_SurfaceCacheForRes asks for, in u64s, aligned for
    // the surfcache_t at its start (d_surf.c).
    unsafe {
        if engine::com_filesize < 2048 * 4 + 4 {
            Sys_Error(c"gfx/colormap.lmp is too short".as_ptr());
        }
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
        engine::D_InitCaches(leak((size.max(0) as usize).div_ceil(8), 0u64).cast::<c_void>(), size);
    }
    match fb::Fb::open() {
        Ok(f) => state().fb = Some(f),
        Err(e) => eprintln!("neochumby-quake: /dev/fb0: {e}"),
    }
    // SAFETY: as the caller promises: 768 bytes, host_basepal.
    unsafe { VID_SetPalette(palette) };
}

#[unsafe(no_mangle)]
pub extern "C" fn VID_Shutdown() {}

#[unsafe(no_mangle)]
pub extern "C" fn VID_Update(_rects: *mut vrect_t) {
    let s = state();
    let Some(f) = s.fb.as_mut() else { return };
    let v = &raw const vid;
    // SAFETY: a read of vid's fields through a raw pointer, on the engine's
    // thread, between its writes (it calls VID_Update once a frame is drawn).
    let (w, h, r) = unsafe { ((*v).width as usize, (*v).height as usize, (*v).rowbytes as usize) };
    // SAFETY: VID_Init set buffer to a leaked WIDTH * HEIGHT bytes, rowbytes
    // WIDTH and height HEIGHT, and nothing changes them, so r * h bytes are
    // there; the engine doesn't draw into it until this returns.
    let src = unsafe { std::slice::from_raw_parts((*v).buffer, r * h) };
    f.blit_indexed(&s.palette, src, w, h, r);
}

#[unsafe(no_mangle)]
pub extern "C" fn D_BeginDirectRect(_x: c_int, _y: c_int, _pbitmap: *mut u8, _width: c_int, _height: c_int) {}

#[unsafe(no_mangle)]
pub extern "C" fn D_EndDirectRect(_x: c_int, _y: c_int, _width: c_int, _height: c_int) {}
