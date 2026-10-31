//! The wasm runner's host API, as a wasm app imports it (module
//! `neochumby`), behind safe functions: what neochumby-guest offers app
//! authors is built on these. Only for `wasm32`: the runner
//! (neochumby-wasm) provides the imports, and checks every pointer and
//! length it is given against the module's own memory before it reads or
//! writes a byte, so a wrong one traps the module and never reaches the
//! host's memory.
//!
//! The names and signatures are the runner's (neochumby_wasm::host), and
//! must stay as they are.

#[link(wasm_import_module = "neochumby")]
unsafe extern "C" {
    fn fb_size() -> u32;
    fn fb_fill(x: u32, y: u32, w: u32, h: u32, rgb565: u32);
    fn fb_blit(x: u32, y: u32, w: u32, h: u32, px: *const u16, stride: u32);
    fn fb_text(x: u32, y: u32, s: *const u8, len: u32, scale: u32, rgb565: u32);
    fn touch_poll(out: *mut i32) -> i32;
    fn accel(out: *mut i32) -> i32;
    fn now_ms() -> i64;
    fn utc_offset_s() -> i32;
    fn sleep_ms(ms: u32);
    fn log(s: *const u8, len: u32);
    fn request(s: *const u8, len: u32) -> i32;
}

/// The screen's width and height, packed: width in the high 16 bits.
pub fn fb_size_packed() -> u32 {
    // SAFETY: takes and returns plain integers; no memory of ours is
    // touched.
    unsafe { fb_size() }
}

/// Fills a rectangle with an RGB565 colour (the host clips it).
pub fn fill(x: u32, y: u32, w: u32, h: u32, rgb565: u16) {
    // SAFETY: plain integers only.
    unsafe { fb_fill(x, y, w, h, rgb565.into()) }
}

/// Copies `h` rows of `w` pixels, row `r` at `px[r * stride ..]`, to
/// (x, y). Does nothing unless `px` holds every row.
pub fn blit(x: u32, y: u32, w: u32, h: u32, px: &[u16], stride: u32) {
    if w == 0 || h == 0 || stride < w {
        return;
    }
    let need = (stride as usize).saturating_mul(h as usize - 1).saturating_add(w as usize);
    if px.len() < need {
        return;
    }
    // SAFETY: `px` is a live slice of at least `need` pixels, which is all
    // the host reads for these w, h and stride; it reads only, and checks
    // the range against the module's memory itself.
    unsafe { fb_blit(x, y, w, h, px.as_ptr(), stride) }
}

/// Draws `s` in the host's 8x8 font, each pixel `scale` square.
pub fn text(x: u32, y: u32, s: &str, scale: u32, rgb565: u16) {
    // SAFETY: the host reads exactly `s.len()` bytes from `s`, a live
    // string; it reads only, and checks them against the module's memory.
    unsafe { fb_text(x, y, s.as_ptr(), s.len() as u32, scale, rgb565.into()) }
}

/// The next touch, if there is one: `out` is [down (1) or up (0), x, y].
pub fn touch(out: &mut [i32; 3]) -> bool {
    // SAFETY: the host writes at most three i32s (12 bytes) to `out`, a
    // live, exclusively borrowed array of three.
    unsafe { touch_poll(out.as_mut_ptr()) == 1 }
}

/// The accelerometer's three axes into `out`, if it has a reading.
pub fn accel3(out: &mut [i32; 3]) -> bool {
    // SAFETY: the host writes at most three i32s (12 bytes) to `out`, a
    // live, exclusively borrowed array of three.
    unsafe { accel(out.as_mut_ptr()) == 1 }
}

/// Milliseconds since the app started, never going back.
pub fn now() -> u64 {
    // SAFETY: plain integers only.
    unsafe { now_ms() as u64 }
}

/// Local time's offset from UTC, in seconds.
pub fn utc_offset() -> i32 {
    // SAFETY: plain integers only.
    unsafe { utc_offset_s() }
}

/// Sleeps about `ms` milliseconds.
pub fn sleep(ms: u32) {
    // SAFETY: plain integers only.
    unsafe { sleep_ms(ms) }
}

/// A line in the session's log.
pub fn log_line(s: &str) {
    // SAFETY: the host reads exactly `s.len()` bytes from `s`, a live
    // string, and checks them against the module's memory.
    unsafe { log(s.as_ptr(), s.len() as u32) }
}

/// A request of the session (`open APP`): whether it was taken.
pub fn ask(s: &str) -> bool {
    // SAFETY: the host reads exactly `s.len()` bytes from `s`, a live
    // string, and checks them against the module's memory.
    unsafe { request(s.as_ptr(), s.len() as u32) == 0 }
}
