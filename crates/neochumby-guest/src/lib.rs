//! neochumby-guest: the host API of neochumby's wasm runner
//! (neochumby-wasm), for an app built for `wasm32-wasip1`, as safe
//! functions. Rust's std works as it is beside it (stdout is the app's
//! terminal in text mode; time, sleeping and random numbers through WASI);
//! this is what std has no word for: the screen, touches, the
//! accelerometer and the session.
//!
//! Each needs its word in the app's `access`, as the runner checks before
//! the app runs: an app that calls [`fb`] without `fb` is refused, not
//! trapped later.
//!
//! On anything but wasm32 the functions do nothing and report nothing, so
//! an app's own code can be built and tested on the host.
//!
//! It reaches the host only through neochumby-sys, whose module
//! neochumby_sys::guest holds the imports, behind safe functions.

#![forbid(unsafe_code)]

// The imports themselves are in neochumby-sys.
#[cfg(target_arch = "wasm32")]
use neochumby_sys::guest as sys;

/// The screen: 320x240, RGB565, drawn by the host in whole operations.
/// Needs `fb`.
pub mod fb {
    /// RGB565 from 8-bit channels.
    pub const fn rgb(r: u8, g: u8, b: u8) -> u16 {
        ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
    }

    /// Its width and height in pixels; (0, 0) off wasm.
    pub fn size() -> (u32, u32) {
        #[cfg(target_arch = "wasm32")]
        {
            let s = super::sys::fb_size_packed();
            (s >> 16, s & 0xffff)
        }
        #[cfg(not(target_arch = "wasm32"))]
        (0, 0)
    }

    /// Fills a rectangle (clipped to the screen).
    pub fn fill(x: u32, y: u32, w: u32, h: u32, c: u16) {
        #[cfg(target_arch = "wasm32")]
        super::sys::fill(x, y, w, h, c);
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (x, y, w, h, c);
    }

    /// Copies `h` rows of `w` pixels, row `r` at `px[r * stride ..]`, to
    /// (x, y), clipped to the screen. Does nothing if `px` is too short.
    pub fn blit(x: u32, y: u32, w: u32, h: u32, px: &[u16], stride: u32) {
        if w == 0 || h == 0 || stride < w || px.len() < (stride as usize) * (h as usize - 1) + w as usize {
            return;
        }
        #[cfg(target_arch = "wasm32")]
        super::sys::blit(x, y, w, h, px, stride);
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (x, y);
    }

    /// Draws `s` in the library's 8x8 font, each pixel `scale` pixels
    /// square (1 to 16).
    pub fn text(x: u32, y: u32, s: &str, scale: u32, c: u16) {
        #[cfg(target_arch = "wasm32")]
        super::sys::text(x, y, s, scale, c);
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (x, y, s, scale, c);
    }

    /// The width `s` takes at `scale`.
    pub fn text_width(s: &str, scale: u32) -> u32 {
        s.chars().count() as u32 * 8 * scale
    }
}

/// A touch: down (or moved) at (x, y), in screen pixels, or up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    Down(i32, i32),
    Up,
}

/// The next touch since the last, if any; never on anything but wasm.
/// Needs `touch`.
pub fn touch() -> Option<Touch> {
    #[cfg(target_arch = "wasm32")]
    {
        let mut t = [0i32; 3];
        if !sys::touch(&mut t) {
            return None;
        }
        Some(if t[0] == 1 { Touch::Down(t[1], t[2]) } else { Touch::Up })
    }
    #[cfg(not(target_arch = "wasm32"))]
    None
}

/// The accelerometer's three axes, if it has a reading. Needs `accel`.
pub fn accel() -> Option<[i32; 3]> {
    #[cfg(target_arch = "wasm32")]
    {
        let mut a = [0i32; 3];
        sys::accel3(&mut a).then_some(a)
    }
    #[cfg(not(target_arch = "wasm32"))]
    None
}

/// Milliseconds since the app started, never going back.
pub fn now_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    return sys::now();
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_millis() as u64
    }
}

/// Local time's offset from UTC, in seconds, as the Chumby's zone had it
/// when the app started (0 off wasm). The wall clock itself is std's
/// `SystemTime`.
pub fn utc_offset_s() -> i32 {
    #[cfg(target_arch = "wasm32")]
    return sys::utc_offset();
    #[cfg(not(target_arch = "wasm32"))]
    0
}

/// Sleeps about `ms` milliseconds.
pub fn sleep_ms(ms: u32) {
    #[cfg(target_arch = "wasm32")]
    sys::sleep(ms);
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::sleep(std::time::Duration::from_millis(ms.into()));
}

/// A line in the session's log, as this app's.
pub fn log(s: &str) {
    #[cfg(target_arch = "wasm32")]
    sys::log_line(s);
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("{s}");
}

/// Asks the session to open `app` next; the app should then exit. False if
/// there's no session to ask.
pub fn open(app: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    return sys::ask(&format!("open {app}"));
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = app;
        false
    }
}
