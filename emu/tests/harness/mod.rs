//! The emulator test harness: boots an emulated Chumby Classic through a
//! [`backend::Backend`], and gives the scenarios its console (waits on
//! what it prints, a shell), its touchscreen and its screen.

pub mod backend;
pub mod chumby_emu;
pub mod images;
pub mod qemu;
pub mod screen;

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub use backend::Result;
use backend::{Backend, Machine, Media};
pub use screen::Screen;

/// How much longer than their defaults to wait, for slow hosts:
/// NEOCHUMBY_EMU_TIMEOUT_SCALE (a number, default 1).
fn scale() -> f64 {
    std::env::var("NEOCHUMBY_EMU_TIMEOUT_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0)
}

pub fn secs(n: u64) -> Duration {
    Duration::from_secs_f64(n as f64 * scale())
}

/// The serial console's output so far, filled by a thread, and its input.
struct Console {
    out: Arc<(Mutex<Vec<u8>>, Condvar)>,
    input: Box<dyn Write + Send>,
}

impl Console {
    fn new(mut from: Box<dyn Read + Send>, input: Box<dyn Write + Send>, mut log: File) -> Console {
        let out = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let shared = out.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let n = match from.read(&mut buf) {
                    Ok(0) | Err(_) => 0,
                    Ok(n) => n,
                };
                let _ = log.write_all(&buf[..n]);
                let (m, cv) = &*shared;
                if n == 0 {
                    // The emulator went; wake the waiters to see that.
                    m.lock().unwrap().extend_from_slice(b"\n[harness] console closed\n");
                    cv.notify_all();
                    return;
                }
                m.lock().unwrap().extend_from_slice(&buf[..n]);
                cv.notify_all();
            }
        });
        Console { out, input }
    }

    fn len(&self) -> usize {
        self.out.0.lock().unwrap().len()
    }

    /// Waits until `pat` appears at or after byte `from`; the offset just
    /// past it.
    fn wait_for(&self, pat: &str, from: usize, timeout: Duration) -> Result<usize> {
        let deadline = Instant::now() + timeout;
        let (m, cv) = &*self.out;
        let mut out = m.lock().unwrap();
        loop {
            let tail = &out[from.min(out.len())..];
            if let Some(i) = tail.windows(pat.len()).position(|w| w == pat.as_bytes()) {
                return Ok(from + i + pat.len());
            }
            if tail.ends_with(b"[harness] console closed\n") {
                return Err(format!("the emulator stopped before the console showed {pat:?}"));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!("the console didn't show {pat:?} within {timeout:?}"));
            }
            out = cv.wait_timeout(out, deadline - now).unwrap().0;
        }
    }

    fn text(&self, from: usize, to: usize) -> String {
        let out = self.out.0.lock().unwrap();
        String::from_utf8_lossy(&out[from.min(out.len())..to.min(out.len())]).into_owned()
    }

    fn send(&mut self, line: &str) -> Result<()> {
        self.input.write_all(format!("{line}\n").as_bytes()).map_err(|e| format!("console: {e}"))?;
        self.input.flush().map_err(|e| format!("console: {e}"))
    }
}

/// A booted, emulated Chumby.
pub struct Chumby {
    machine: Box<dyn Machine>,
    console: Console,
    /// Where waits on the console start from: past what earlier waits
    /// matched, so each step looks only at what came after the last.
    mark: usize,
    work: PathBuf,
    shots: usize,
    commands: usize,
}

impl Chumby {
    pub fn boot(backend: &dyn Backend, media: &Media, work: &Path) -> Result<Chumby> {
        let b = backend.boot(media, work)?;
        let log = File::create(work.join("console.log")).map_err(|e| e.to_string())?;
        Ok(Chumby {
            machine: b.machine,
            console: Console::new(b.console_out, b.console_in, log),
            mark: 0,
            work: work.to_path_buf(),
            shots: 0,
            commands: 0,
        })
    }

    /// Waits for `pat` on the console, after the last thing waited for.
    pub fn expect(&mut self, pat: &str, timeout: Duration) -> Result<()> {
        self.mark = self.console.wait_for(pat, self.mark, timeout)?;
        Ok(())
    }

    /// Activates the stock console shell (BusyBox's askfirst, once the
    /// stock boot has finished) and waits for its prompt.
    pub fn open_shell(&mut self, timeout: Duration) -> Result<()> {
        self.expect("Please press Enter to activate this console.", timeout)?;
        self.console.send("")?;
        self.expect("Built-in shell", secs(30))?;
        Ok(())
    }

    /// Runs a command in the console shell; its output. The end marker is
    /// computed by the shell, so the echo of the command line itself
    /// never matches it.
    pub fn sh(&mut self, cmd: &str, timeout: Duration) -> Result<String> {
        self.commands += 1;
        let n = self.commands;
        let start = self.console.len();
        self.console.send(&format!("{cmd}; echo \"@@end-$(({n} + 1000))@@\""))?;
        let end = self.console.wait_for(&format!("@@end-{}@@", n + 1000), start, timeout)?;
        self.mark = self.mark.max(end);
        let text = self.console.text(start, end);
        // Drop the echoed command line and the marker.
        let body = text.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
        Ok(body.rsplit_once("@@end-").map(|(b, _)| b).unwrap_or(body).trim().to_string())
    }

    pub fn tap(&mut self, x: u32, y: u32) -> Result<()> {
        self.machine.tap(x, y, Duration::from_millis(150))
    }

    pub fn hold(&mut self, x: u32, y: u32, hold: Duration) -> Result<()> {
        self.machine.tap(x, y, hold)
    }

    #[allow(dead_code)]
    pub fn press(&mut self, x: u32, y: u32) -> Result<()> {
        self.machine.press(x, y)
    }

    #[allow(dead_code)]
    pub fn release(&mut self) -> Result<()> {
        self.machine.release()
    }

    /// The screen now, also kept in the work directory.
    pub fn screen(&mut self) -> Result<Screen> {
        self.shots += 1;
        let path = self.work.join(format!("screen-{:03}.ppm", self.shots));
        self.machine.screendump(&path)?;
        Screen::read(&path)
    }

    /// Waits until the screen shows `what` (`check` passes); the last
    /// screen is saved as `WORK/<name>.ppm` either way.
    pub fn wait_screen(
        &mut self,
        name: &str,
        timeout: Duration,
        check: impl Fn(&Screen) -> std::result::Result<(), String>,
    ) -> Result<Screen> {
        let deadline = Instant::now() + timeout;
        loop {
            let s = self.screen()?;
            let r = check(&s);
            if r.is_ok() || Instant::now() >= deadline {
                s.write_ppm(&self.work.join(format!("{name}.ppm")))?;
                return r.map(|()| s).map_err(|e| format!("{name}: {e} (after {timeout:?})"));
            }
            std::thread::sleep(Duration::from_millis(1500));
        }
    }

    pub fn stop(mut self) -> Result<()> {
        self.machine.stop()
    }
}
