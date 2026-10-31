//! TerminalTextEffects on the Chumby, by ttfx (Rust, MIT, third_party/ttfx):
//! ChrisBuilds' TerminalTextEffects, ported to Rust by omacom. "neochumby"
//! and the local time and date, drawn by one effect after another, picked
//! at random, until a tap. ttfx's main.rs is compiled in as a module,
//! unchanged, and the binary runs itself for each effect, so that each
//! one's memory goes with it:
//!
//!   neochumby-ttfx                          on the screen
//!   NEOCHUMBY_TTFX=1 neochumby-ttfx [ARG..]  ttfx itself, its text on stdin

use std::io::{BufRead, Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// Its main.rs. Its main is private to the module: run() is the way in.
#[allow(dead_code, unused_imports, clippy::all)]
#[rustfmt::skip]
mod upstream {
    include!("../third_party/ttfx/src/main.rs");

    pub fn run() -> std::process::ExitCode {
        main()
    }
}

/// The environment variable that makes this binary ttfx itself.
const CHILD: &str = "NEOCHUMBY_TTFX";

/// Frames a second: what the ARM926 keeps up with, and smooth enough.
const FPS: &str = "30";

/// How long the text stays once an effect has finished, in milliseconds.
const HOLD_MS: u32 = 6000;

/// The effects, with the options that keep each within about half a
/// minute of text this short. Left out: those whose look is a gradient or
/// a gentle change of brightness (colorshift, crumble, highlight, smoke,
/// spotlights, sweep, thunderstorm), which the Linux console's 16 colours
/// flatten to blocks of one colour.
const EFFECTS: &[&[&str]] = &[
    &["beams"],
    &["binarypath"],
    &["blackhole"],
    &["bouncyballs"],
    &["bubbles"],
    &["burn"],
    &["decrypt"],
    &["errorcorrect"],
    &["expand"],
    &["fireworks"],
    &["laseretch"],
    &["matrix", "--rain-time", "8"],
    &["middleout"],
    &["orbittingvolley"],
    &["overflow"],
    &["pour"],
    &["print"],
    &["rain"],
    &["randomsequence"],
    &["rings", "--spin-disperse-cycles", "2"],
    &["scattered"],
    &["slice"],
    &["slide"],
    &["spray"],
    // Its swarms are a tenth of the text each, one after another: the
    // date in tens of them would take a minute and a half.
    &["swarm", "--swarm-size", "0.34", "--swarm-area-count-range", "1-2"],
    &["synthgrid"],
    &["unstable"],
    &["vhstape"],
    &["waves"],
    &["wipe"],
];

fn main() -> ExitCode {
    neochumby::debug::install("neochumby-ttfx");
    if std::env::var_os(CHILD).is_some() {
        return upstream::run();
    }
    let Some(exe) = neochumby::apps::exe() else {
        eprintln!("neochumby-ttfx: can't find myself");
        return ExitCode::FAILURE;
    };
    // A tap types a newline (its manifest's tap), which ends the line the
    // terminal holds in line mode. Any line ends it, and so does the
    // terminal closing.
    let tapped = Arc::new(AtomicBool::new(false));
    let t = Arc::clone(&tapped);
    std::thread::spawn(move || {
        let _ = std::io::stdin().lock().read_line(&mut String::new());
        t.store(true, Ordering::SeqCst);
    });
    let mut pick = Picker::new();
    loop {
        // The canvas is the terminal less a row: ttfx makes room for it
        // with a newline a row, which on the last would scroll the screen.
        let (cols, rows) = ttfx::engine::terminal::get_terminal_dimensions();
        let (cols, rows) = (cols.to_string(), (rows - 1).max(1).to_string());
        let effect = pick.next();
        eprintln!("neochumby-ttfx: {}", effect.join(" "));
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[H\x1b[2J").and_then(|()| out.flush());
        let mut cmd = Command::new(&exe);
        cmd.env(CHILD, "1")
            .args(["--frame-rate", FPS, "--canvas-width", &cols, "--canvas-height", &rows])
            .args(["--anchor-text", "c", "--no-eol", "--no-restore-cursor"])
            .args(effect)
            .stdin(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("neochumby-ttfx: {e}");
                return ExitCode::FAILURE;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text().as_bytes());
        }
        loop {
            if tapped.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                return ExitCode::SUCCESS;
            }
            match child.try_wait() {
                Ok(Some(s)) if !s.success() => {
                    eprintln!("neochumby-ttfx: ttfx: {s}");
                    break;
                }
                Ok(Some(_)) => break,
                Ok(None) => neochumby::sleep_ms(50),
                Err(e) => {
                    eprintln!("neochumby-ttfx: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        for _ in 0..HOLD_MS / 50 {
            if tapped.load(Ordering::SeqCst) {
                return ExitCode::SUCCESS;
            }
            neochumby::sleep_ms(50);
        }
    }
}

/// The text: "neochumby", then the local time and date, each line centred
/// on the widest.
fn text() -> String {
    const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let t = neochumby_sys::time::local_time_at(now);
    // The zone's offset is the local time of day less UTC's, between -12
    // and +14 hours: the local date is then UTC's at now plus it.
    let local = i64::from(t.hour) * 3600 + i64::from(t.minute) * 60 + i64::from(t.second);
    let mut offset = (local - now.rem_euclid(86400)).rem_euclid(86400);
    if offset > 14 * 3600 {
        offset -= 86400;
    }
    let days = (now + offset).div_euclid(86400);
    let (year, month, day) = civil(days);
    // 1970-01-01 was a Thursday.
    let weekday = DAYS[(days + 4).rem_euclid(7) as usize];
    let lines = [
        "n e o c h u m b y".to_string(),
        String::new(),
        format!("{:02}:{:02}", t.hour, t.minute),
        format!("{weekday} {day} {} {year}", MONTHS[(month - 1) as usize]),
    ];
    let width = lines.iter().map(String::len).max().unwrap_or(0);
    lines.iter().map(|l| format!("{:^width$}\n", l)).collect()
}

/// The (year, month 1-12, day 1-31) of `days` since 1970-01-01, in the
/// proleptic Gregorian calendar (Howard Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// The effects in a shuffled order, every one before any again, and never
/// the same twice running.
struct Picker {
    /// The order's state: xorshift64*, seeded from /dev/urandom.
    state: u64,
    /// What is left of this round, taken from the end.
    left: Vec<usize>,
    /// The one shown last.
    last: Option<usize>,
}

impl Picker {
    /// A new order.
    fn new() -> Picker {
        let mut seed = [0u8; 8];
        let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut seed));
        let clock = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
        Picker { state: (u64::from_le_bytes(seed) ^ clock) | 1, left: Vec::new(), last: None }
    }

    /// The next effect's arguments.
    fn next(&mut self) -> &'static [&'static str] {
        if self.left.is_empty() {
            self.left = (0..EFFECTS.len()).collect();
            for i in (1..self.left.len()).rev() {
                let j = (self.random() % (i as u64 + 1)) as usize;
                self.left.swap(i, j);
            }
            if self.left.last() == self.last.as_ref() {
                self.left.swap(0, EFFECTS.len() - 1);
            }
        }
        let i = self.left.pop().unwrap_or(0);
        self.last = Some(i);
        EFFECTS[i]
    }

    /// A random number (xorshift64*).
    fn random(&mut self) -> u64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}
