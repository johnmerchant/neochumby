//! The engine's Sys_* functions (sys.h): files, as id's sys_null.c keeps
//! them, time, logging and quitting. Sys_Error and Sys_Printf are
//! csrc/varargs.c's, which format and call `chum_error` and `chum_log`.

use std::ffi::{CStr, c_char, c_int, c_ulong, c_void};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::engine::{self, Sys_Error};
use crate::state;

/// sys_null.c's handle table: 1-9 (0 is never handed out).
const HANDLES: usize = 10;

/// # Safety
/// `p` is a NUL-terminated string that outlives `'a`.
unsafe fn path<'a>(p: *const c_char) -> &'a Path {
    // SAFETY: as the caller promises.
    Path::new(std::ffi::OsStr::from_bytes(unsafe { CStr::from_ptr(p) }.to_bytes()))
}

fn handle(h: c_int) -> &'static mut File {
    match state().files.get_mut(h as usize).and_then(Option::as_mut) {
        Some(f) => f,
        // SAFETY: Sys_Error takes a printf format and its arguments: a
        // NUL-terminated literal, whose one %d is the c_int given. It
        // formats into its own buffer and exits (chum_error), never
        // returning or calling back into the files.
        None => unsafe { Sys_Error(c"bad file handle %d".as_ptr(), h) },
    }
}

fn store(f: File) -> c_int {
    let files = &mut state().files;
    files.resize_with(HANDLES, || None);
    match (1..HANDLES).find(|&i| files[i].is_none()) {
        Some(i) => {
            files[i] = Some(f);
            i as c_int
        }
        // SAFETY: as in handle(): a format with no conversions, and no
        // arguments.
        None => unsafe { Sys_Error(c"out of handles".as_ptr()) },
    }
}

/// # Safety
/// `p` is a NUL-terminated path, and `hndl` points to a writable int.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Sys_FileOpenRead(p: *const c_char, hndl: *mut c_int) -> c_int {
    // SAFETY: p is NUL-terminated, and lives through the call (the caller's
    // promise; common.c passes its own netpath buffer).
    let (h, len) = match File::open(unsafe { path(p) }) {
        Ok(f) => {
            let len = f.metadata().map_or(0, |m| m.len() as c_int);
            (store(f), len)
        }
        Err(_) => (-1, -1),
    };
    // SAFETY: hndl points to a writable int, as the caller promises
    // (COM_FindFile passes its own).
    unsafe { *hndl = h };
    len
}

/// # Safety
/// `p` is a NUL-terminated path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Sys_FileOpenWrite(p: *const c_char) -> c_int {
    // SAFETY: p is NUL-terminated, and lives through the call (the caller's
    // promise).
    match File::create(unsafe { path(p) }) {
        Ok(f) => store(f),
        Err(e) => {
            let msg = std::ffi::CString::new(e.to_string()).unwrap_or_default();
            // SAFETY: a format whose two %s are the two NUL-terminated
            // strings given, p (the caller's) and msg (ours), both live
            // until Sys_Error, which never returns, has formatted them.
            unsafe { Sys_Error(c"Error opening %s: %s".as_ptr(), p, msg.as_ptr()) }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn Sys_FileClose(h: c_int) {
    if let Some(f) = state().files.get_mut(h as usize) {
        *f = None;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn Sys_FileSeek(h: c_int, position: c_int) {
    let _ = handle(h).seek(SeekFrom::Start(position.max(0) as u64));
}

/// As fread: as much as there is, up to `count`.
///
/// # Safety
/// `dest` points to `count` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Sys_FileRead(h: c_int, dest: *mut c_void, count: c_int) -> c_int {
    // SAFETY: dest is count writable bytes, as the caller promises (the
    // engine reads into its own buffers of at least count), and nothing
    // else reads or writes them during the call.
    let buf = unsafe { std::slice::from_raw_parts_mut(dest.cast::<u8>(), count.max(0) as usize) };
    let f = handle(h);
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    n as c_int
}

/// # Safety
/// `data` points to `count` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Sys_FileWrite(h: c_int, data: *const c_void, count: c_int) -> c_int {
    // SAFETY: data is count readable bytes, as the caller promises, not
    // written during the call.
    let buf = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), count.max(0) as usize) };
    match handle(h).write_all(buf) {
        Ok(()) => count,
        Err(_) => 0,
    }
}

/// # Safety
/// `p` is a NUL-terminated path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Sys_FileTime(p: *const c_char) -> c_int {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: p is NUL-terminated, and lives through the call (the caller's
    // promise).
    std::fs::metadata(unsafe { path(p) }).map_or(-1, |m| m.mtime() as c_int)
}

/// # Safety
/// `p` is a NUL-terminated path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Sys_mkdir(p: *const c_char) {
    // SAFETY: p is NUL-terminated, and lives through the call (the caller's
    // promise).
    let _ = std::fs::create_dir(unsafe { path(p) });
}

#[unsafe(no_mangle)]
pub extern "C" fn Sys_MakeCodeWriteable(_start: c_ulong, _length: c_ulong) {}

#[unsafe(no_mangle)]
pub extern "C" fn Sys_Quit() -> ! {
    // SAFETY: the engine calls Sys_Quit (Host_Quit_f) on its own thread,
    // once it is running, and Host_Shutdown is what id's own Sys_Quit calls
    // then; nothing of ours is borrowed across it (state()'s rule), and the
    // process exits right after.
    unsafe { engine::Host_Shutdown() };
    std::process::exit(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn Sys_FloatTime() -> f64 {
    state().start.elapsed().as_secs_f64()
}

#[unsafe(no_mangle)]
pub extern "C" fn Sys_ConsoleInput() -> *mut c_char {
    std::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn Sys_Sleep() {}

#[unsafe(no_mangle)]
pub extern "C" fn Sys_HighFPPrecision() {}

#[unsafe(no_mangle)]
pub extern "C" fn Sys_LowFPPrecision() {}

/// # Safety
/// `msg` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chum_log(msg: *const c_char) {
    // SAFETY: msg is NUL-terminated and lives through the call (Sys_Printf's
    // own buffer, csrc/varargs.c).
    let s = unsafe { CStr::from_ptr(msg) };
    let _ = std::io::stderr().lock().write_all(s.to_bytes());
}

/// Sys_Error's end: the message, then exit(1).
///
/// # Safety
/// `msg` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chum_error(msg: *const c_char) -> ! {
    // SAFETY: msg is NUL-terminated and lives through the call (Sys_Error's
    // own buffer, csrc/varargs.c).
    let s = unsafe { CStr::from_ptr(msg) };
    eprintln!("Sys_Error: {}", s.to_string_lossy());
    std::process::exit(1)
}
