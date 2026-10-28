//! The Quake engine's side: its structs, mirrored as #[repr(C)] (id's
//! WinQuake is a pinned submodule), and the functions and globals of its
//! that the platform layer uses. The globals it expects a platform to
//! define are defined here.

#![allow(non_upper_case_globals, non_camel_case_types)]

use std::ffi::{c_char, c_float, c_int, c_uint, c_ushort, c_void};

/// common.h's qboolean, an enum the size of an int.
pub type qboolean = c_int;

/// vid.h's vrect_t.
#[repr(C)]
pub struct vrect_t {
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
    pub pnext: *mut vrect_t,
}

/// vid.h's viddef_t.
#[repr(C)]
pub struct viddef_t {
    pub buffer: *mut u8,
    pub colormap: *mut u8,
    pub colormap16: *mut c_ushort,
    pub fullbright: c_int,
    pub rowbytes: c_uint,
    pub width: c_uint,
    pub height: c_uint,
    pub aspect: c_float,
    pub numpages: c_int,
    pub recalc_refdef: c_int,
    pub conbuffer: *mut u8,
    pub conrowbytes: c_int,
    pub conwidth: c_uint,
    pub conheight: c_uint,
    pub maxwarpwidth: c_int,
    pub maxwarpheight: c_int,
    pub direct: *mut u8,
}

/// quakedef.h's quakeparms_t.
#[repr(C)]
pub struct quakeparms_t {
    pub basedir: *const c_char,
    pub cachedir: *const c_char,
    pub argc: c_int,
    pub argv: *mut *mut c_char,
    pub membase: *mut c_void,
    pub memsize: c_int,
}

/// sound.h's dma_t.
#[repr(C)]
pub struct dma_t {
    pub gamealive: qboolean,
    pub soundalive: qboolean,
    pub splitbuffer: qboolean,
    pub channels: c_int,
    pub samples: c_int,
    pub submission_chunk: c_int,
    pub samplepos: c_int,
    pub samplebits: c_int,
    pub speed: c_int,
    pub buffer: *mut u8,
}

// The globals a platform defines (vid_*.c's and sys_*.c's).
#[unsafe(no_mangle)]
pub static mut vid: viddef_t = viddef_t {
    buffer: std::ptr::null_mut(),
    colormap: std::ptr::null_mut(),
    colormap16: std::ptr::null_mut(),
    fullbright: 0,
    rowbytes: 0,
    width: 0,
    height: 0,
    aspect: 0.0,
    numpages: 0,
    recalc_refdef: 0,
    conbuffer: std::ptr::null_mut(),
    conrowbytes: 0,
    conwidth: 0,
    conheight: 0,
    maxwarpwidth: 0,
    maxwarpheight: 0,
    direct: std::ptr::null_mut(),
};
#[unsafe(no_mangle)]
pub static mut d_8to16table: [c_ushort; 256] = [0; 256];
#[unsafe(no_mangle)]
pub static mut d_8to24table: [c_uint; 256] = [0; 256];
/// The Chumby is never the dedicated server.
#[unsafe(no_mangle)]
pub static mut isDedicated: qboolean = 0;

unsafe extern "C" {
    pub static mut host_colormap: *mut u8;
    pub static mut d_pzbuffer: *mut i16;
    pub static mut com_argc: c_int;
    pub static mut com_argv: *mut *mut c_char;
    pub static mut shm: *mut dma_t;
    pub static mut sn: dma_t;
    pub static mut paintedtime: c_int;

    pub fn COM_InitArgv(argc: c_int, argv: *mut *mut c_char);
    pub fn COM_CheckParm(parm: *const c_char) -> c_int;
    pub fn Q_atof(s: *const c_char) -> c_float;
    pub fn Host_Init(parms: *mut quakeparms_t);
    pub fn Host_Frame(time: c_float);
    pub fn Host_Shutdown();
    pub fn Key_Event(key: c_int, down: qboolean);
    pub fn D_SurfaceCacheForRes(width: c_int, height: c_int) -> c_int;
    pub fn D_InitCaches(buffer: *mut c_void, size: c_int);
    /// Variadic, in csrc/varargs.c; it doesn't return.
    pub fn Sys_Error(error: *const c_char, ...) -> !;
}
