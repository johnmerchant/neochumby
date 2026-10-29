//! flurry-bench: Flurry (flurry.rs) as fast as it will go, or at a set
//! frame rate, for measuring it. On the Chumby it draws to /dev/fb0; on any
//! other machine into memory, and can write the frames out.
//!
//! ```text
//! flurry-bench [--secs S] [--fps N] [--back] [--dump PREFIX] [WORD...]
//! ```
//!
//! - `--secs`: how long, default 20.
//! - `--fps`: hold to N frames a second (the classic engine's is 25);
//!   default 0, as fast as it can.
//! - `--back`: draw into a buffer and copy the rows to the screen, as the
//!   engine does, rather than straight onto the screen.
//! - `--dump PREFIX`: write each frame as PREFIX_NNNN.raw (RGB565), with
//!   the frames timed at `--fps` (default 25) whatever the real time.
//! - WORDs: as the manifest's `flurry` line takes them, e.g. `classic
//!   streams=3 puffs=600 seed=1`.
//!
//! Each second it prints the frame rate and where the time went; at the end,
//! the totals and the CPU it used (from /proc).

use std::time::Instant;

use neochumby_classic::flurry::{Options, Scene};

const N: usize = 320 * 240;

/// This process's CPU time and the whole machine's busy and total time, in
/// clock ticks (Linux's /proc); zeros elsewhere.
fn cpu() -> (u64, u64, u64) {
    let own = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| {
            // The fields after the command's closing parenthesis, from the
            // state on; utime and stime are the 12th and 13th of them.
            let f: Vec<&str> = s[s.rfind(')')? + 2..].split(' ').collect();
            Some(f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?)
        })
        .unwrap_or(0);
    let (busy, total) = std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|s| {
            let f: Vec<u64> = s.lines().next()?.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
            let total: u64 = f.iter().sum();
            Some((total - f.get(3)? - f.get(4).unwrap_or(&0), total))
        })
        .unwrap_or((0, 0));
    (own, busy, total)
}

struct Screen {
    #[cfg(target_arch = "arm")]
    fb: neochumby::fb::Fb,
    #[cfg(not(target_arch = "arm"))]
    fb: Vec<u16>,
}

impl Screen {
    fn open() -> Screen {
        #[cfg(target_arch = "arm")]
        let fb = neochumby::fb::Fb::open().unwrap_or_else(|e| {
            eprintln!("flurry-bench: /dev/fb0: {e}");
            std::process::exit(1)
        });
        #[cfg(not(target_arch = "arm"))]
        let fb = vec![0u16; N];
        Screen { fb }
    }

    fn pixels(&mut self) -> &mut [u16] {
        #[cfg(target_arch = "arm")]
        return self.fb.pixels();
        #[cfg(not(target_arch = "arm"))]
        return &mut self.fb;
    }
}

fn main() {
    let mut secs = 20i64;
    let mut fps = 0i64;
    let mut via_back = false;
    let mut dump: Option<String> = None;
    let mut words = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--secs" => secs = args.next().and_then(|v| v.parse().ok()).unwrap_or(secs),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).unwrap_or(fps),
            "--back" => via_back = true,
            "--dump" => dump = args.next(),
            _ => words.push(a),
        }
    }
    let Some(opt) = Options::parse(words.iter().map(String::as_str)) else {
        eprintln!("flurry-bench: options not understood: {}", words.join(" "));
        std::process::exit(2);
    };
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u32);
    let mut scene = Scene::new(&opt, seed);
    let mut screen = Screen::open();
    screen.pixels().fill(0);
    let mut back = vec![0u16; N];
    println!(
        "flurry-bench: {} for {secs} s, fps {fps}, {}",
        words.join(" "),
        if via_back { "via a back buffer" } else { "straight to the screen" }
    );

    let clock = Instant::now();
    let us = || clock.elapsed().as_micros() as i64;
    let (cpu0, busy0, total0) = cpu();
    let mut frames = 0i64;
    let (mut sec_frames, mut sec_start) = (0i64, 0i64);
    let mut sum = [0i64; 4];
    let mut tot = [0i64; 4];
    let (mut puffs, mut quads, mut pixels) = (0u64, 0u64, 0u64);
    let step = if fps > 0 { 1_000_000 / fps } else { 0 };
    // As the engine paces itself: to a schedule, so a late wake-up is made
    // up by the next frame's shorter sleep (the Chumby sleeps in 10 ms ticks).
    let mut next = 0i64;
    let dump_step = 1000 / if fps > 0 { fps } else { 25 };
    loop {
        let t0 = us();
        if t0 >= secs * 1_000_000 {
            break;
        }
        // A dump's frames are timed as if at the frame rate, so they play
        // back at the right speed.
        let t_ms = if dump.is_some() { frames * dump_step } else { t0 / 1000 };
        let rows = if via_back {
            let r = scene.frame(t_ms, &mut back, frames == 0, us);
            let b0 = us();
            if let Some((y0, y1)) = r {
                let fb = screen.pixels();
                fb[y0 * 320..(y1 + 1) * 320].copy_from_slice(&back[y0 * 320..(y1 + 1) * 320]);
            }
            sum[3] += us() - b0;
            r
        } else {
            scene.frame(t_ms, screen.pixels(), frames == 0, us)
        };
        let _ = rows;
        let c = scene.cost;
        sum[0] += c.model_us;
        sum[1] += c.draw_us;
        sum[2] += c.pass_us;
        puffs += c.puffs as u64;
        quads += c.quads as u64;
        pixels += c.pixels as u64;
        if let Some(p) = &dump {
            let bytes: Vec<u8> = screen.pixels().iter().flat_map(|v| v.to_le_bytes()).collect();
            let _ = std::fs::write(format!("{p}_{frames:04}.raw"), bytes);
        }
        frames += 1;
        sec_frames += 1;
        let now = us();
        if now - sec_start >= 1_000_000 {
            let f = sec_frames.max(1);
            println!(
                "{} fps  model {} ms  draw {}  pass {}  blit {}  puffs {:4}  quads {:4}  px {:6}",
                dec(sec_frames * 10_000_000 / (now - sec_start), 10),
                dec(sum[0] / f, 1000),
                dec(sum[1] / f, 1000),
                dec(sum[2] / f, 1000),
                dec(sum[3] / f, 1000),
                c.puffs,
                c.quads,
                c.pixels,
            );
            for (t, s) in tot.iter_mut().zip(&mut sum) {
                *t += *s;
                *s = 0;
            }
            sec_frames = 0;
            sec_start = now;
        }
        if step > 0 {
            next += step;
            let wait = next - us();
            if wait > 0 {
                std::thread::sleep(std::time::Duration::from_micros(wait as u64));
            } else {
                next = us();
            }
        }
    }
    for (t, s) in tot.iter_mut().zip(&sum) {
        *t += *s;
    }
    let wall = us().max(1);
    let (cpu1, busy1, total1) = cpu();
    let f = frames.max(1);
    println!(
        "total: {frames} frames in {} s = {} fps; per frame: model {} ms, draw {}, pass {}, blit {}; puffs {}, quads {}, pixels {}",
        dec(wall / 1000, 1000),
        dec(frames * 10_000_000 / wall, 10),
        dec(tot[0] / f, 1000),
        dec(tot[1] / f, 1000),
        dec(tot[2] / f, 1000),
        dec(tot[3] / f, 1000),
        puffs / f as u64,
        quads / f as u64,
        pixels / f as u64,
    );
    if total1 > total0 {
        // Clock ticks are 100 a second on the Chumby.
        println!(
            "cpu: this process {}% of the wall time; the whole machine {}% busy",
            (cpu1 - cpu0) as i64 * 1_000_000 / wall,
            (busy1 - busy0) * 100 / (total1 - total0),
        );
    }
}

/// n / unit with one or two decimals, in integers (no floats here: the
/// Chumby has no FPU, and this keeps soft float out of the binary).
fn dec(n: i64, unit: i64) -> String {
    let places = if unit >= 100 { 2 } else { 1 };
    let scale = if places == 2 { 100 } else { 10 };
    let v = n * scale / unit;
    format!("{}.{:0w$}", v / scale, (v % scale).abs(), w = places)
}
