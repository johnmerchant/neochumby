//! WASM CLOCK's face, built natively for the Chumby and drawn on the
//! framebuffer itself, to measure the wasm build against (src/main.rs): it
//! logs its mean draw time every minute, as the wasm build does, until a
//! tap.

#[cfg_attr(not(target_arch = "arm"), allow(dead_code))]
mod face;

#[cfg(target_arch = "arm")]
fn main() {
    use std::time::Instant;

    use neochumby::fb::Fb;
    use neochumby::input::{Touch, Touchscreen};

    /// The framebuffer, drawn on directly.
    struct Native(Fb);

    impl face::Canvas for Native {
        fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, c: u16) {
            self.0.rect(x as usize, y as usize, w as usize, h as usize, c);
        }

        fn text(&mut self, x: u32, y: u32, s: &str, scale: u32, c: u16) {
            self.0.text(x as usize, y as usize, s, scale as usize, c);
        }
    }

    let fb = match Fb::open() {
        Ok(fb) => fb,
        Err(e) => {
            eprintln!("neochumby-clockwasm-native: the screen: {e}");
            std::process::exit(1);
        }
    };
    let mut screen = Native(fb);
    let mut touch = Touchscreen::open();
    let mut meter = face::Meter::new(60);
    let mut shown = None;
    loop {
        if touch.poll().iter().any(|t| matches!(t, Touch::Down { .. })) {
            return;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64);
        let utc = now.unwrap_or(0);
        // Local time, as the runner gives the wasm build its offset.
        let l = neochumby_sys::time::local_time_at(utc);
        let local = i64::from(l.hour) * 3600 + i64::from(l.minute) * 60 + i64::from(l.second);
        let mut offset = local - utc.rem_euclid(86400);
        if offset > 14 * 3600 {
            offset -= 86400;
        } else if offset < -12 * 3600 {
            offset += 86400;
        }
        let t = utc + offset;
        if shown != Some(t) {
            let start = Instant::now();
            face::draw(&mut screen, t);
            if let Some(line) = meter.add(start.elapsed().as_micros() as u64) {
                eprintln!("neochumby-clockwasm-native: {line}");
            }
            shown = Some(t);
        }
        neochumby::sleep_ms(50);
    }
}

/// Elsewhere it has no screen to draw on.
#[cfg(not(target_arch = "arm"))]
fn main() {
    eprintln!("neochumby-clockwasm-native: runs on the Chumby");
}
