//! WASM CLOCK: a big digital clock, built as WebAssembly and run by the
//! session's wasm runner (neochumby-wasm), drawing through its host API,
//! until a tap. The first wasm app, and the measure of what wasm costs: it
//! logs its mean draw time every minute, and `neochumby-clockwasm-native`
//! (src/native.rs) draws the same face natively and logs the same.
//!
//! To compare the two on a Chumby, open WASM CLOCK and read its log line;
//! then run the native one on the console as root, with nothing else in
//! front, and read its:
//!
//!   cargo build --release --target armv5te-unknown-linux-musleabi \
//!     -p neochumby-clockwasm --bin neochumby-clockwasm-native

mod face;

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use neochumby_guest::{Touch, fb};

/// The screen, through the host.
struct Host;

impl face::Canvas for Host {
    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, c: u16) {
        fb::fill(x, y, w, h, c);
    }

    fn text(&mut self, x: u32, y: u32, s: &str, scale: u32, c: u16) {
        fb::text(x, y, s, scale, c);
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn main() {
    let offset = i64::from(neochumby_guest::utc_offset_s());
    let mut meter = face::Meter::new(60);
    let mut shown = None;
    loop {
        if matches!(neochumby_guest::touch(), Some(Touch::Down(..))) {
            return;
        }
        let t = now() + offset;
        if shown != Some(t) {
            let start = Instant::now();
            face::draw(&mut Host, t);
            if let Some(line) = meter.add(start.elapsed().as_micros() as u64) {
                neochumby_guest::log(&line);
            }
            shown = Some(t);
        }
        neochumby_guest::sleep_ms(50);
    }
}
