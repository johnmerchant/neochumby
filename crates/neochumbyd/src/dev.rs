//! The developer link's server side (neochumby_proto::dev): a paired
//! Chumby's log, kept here, and the restarts and screenshots asked of it.
//!
//!   neochumbyd log NAME [-f]       its log; -f follows it
//!   neochumbyd restart NAME        its session starts again, from the tree
//!   neochumbyd shot NAME OUT.png   its screen
//!   neochumbyd tap NAME X Y [--hold MS]       a touch, if it is set dev=on
//!   neochumbyd button NAME press|release|tap  its top button, likewise
//!
//! In the state directory:
//!
//!   logs/<name>, logs/<name>.1     its last [`LOG_LINES`] lines at most:
//!                                  logs/<name> until it has half, then it
//!                                  becomes logs/<name>.1
//!   dev/<name>/restart, shot       asked of it by the commands; the daemon
//!                                  removes each once it has asked the
//!                                  Chumby, so a command that sees its file
//!                                  stay knows the Chumby isn't connected
//!   dev/<name>/shot.png, shot.err  the screenshot, or why there's none
//!   dev/<name>/input               a tap or a button asked of it, one at
//!                                  a time ([`input_line`]), taken as the
//!                                  others are
//!   dev/<name>/input.done, .err    the Chumby did it, or why not
//!
//! Input goes only to a Chumby whose settings say `dev=on`
//! (neochumby_proto::Settings::dev), checked by the command, by the
//! daemon as it takes the request, and by the Chumby, to which the daemon
//! sends its settings again first. Off by default: pairing a Chumby lets
//! its server see what it does, and drive it only once its owner says so.
//!
//! As with pairing, the commands and the daemon share only these files:
//! whoever may write the state directory may use them, and nothing listens
//! anywhere new. Only a paired Chumby has a link, over the session pairing
//! authenticates, and what it sends is bounded: lines are cut to
//! neochumby_proto::dev::MAX_LINE and cleaned of control characters, a
//! Chumby's log takes at most [`LOG_RATE`] bytes a second (more is
//! dropped, and a line says how much) and [`LOG_LINES`] lines on disk, and
//! a screen is accepted only when asked for, at most MAX_SHOT_PIXELS, and
//! within [`SHOT_TIMEOUT`].

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use neochumby_proto::dev::{MAX_HOLD_MS, ShotIn, clean_line};
use neochumby_proto::{Msg, Session, valid_name};

use crate::api::Api;
use crate::png;

/// The most lines kept of each Chumby's log.
pub const LOG_LINES: usize = 2000;
/// Bytes of log a second a Chumby may send, on average...
pub const LOG_RATE: u64 = 32 * 1024;
/// ...and at once.
pub const LOG_BURST: u64 = 256 * 1024;
/// How long a Chumby has to send its screen.
pub const SHOT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a Chumby has to answer a tap (after its hold) or a button.
pub const INPUT_ANSWER: Duration = Duration::from_secs(5);
/// How often the daemon looks for what's asked of a Chumby.
const TICK: Duration = Duration::from_millis(200);

fn invalid(what: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// The links up now, one a Chumby: a Chumby that connects again (it
/// restarted before the old connection was noticed gone) takes over, and
/// the old link ends.
#[derive(Default)]
pub struct Links {
    current: Mutex<HashMap<String, u64>>,
    next: Mutex<u64>,
}

impl Links {
    fn begin(&self, name: &str) -> u64 {
        let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
        *next += 1;
        self.current.lock().unwrap_or_else(|e| e.into_inner()).insert(name.into(), *next);
        *next
    }

    fn current(&self, name: &str, id: u64) -> bool {
        self.current.lock().unwrap_or_else(|e| e.into_inner()).get(name) == Some(&id)
    }
}

/// A Chumby's log on disk: at most `keep` lines, in two files.
pub struct LogStore {
    path: PathBuf,
    file: File,
    lines: usize,
    keep: usize,
}

fn log_path(state_dir: &Path, name: &str) -> PathBuf {
    state_dir.join("logs").join(name)
}

fn old_path(path: &Path) -> PathBuf {
    path.with_extension("1")
}

impl LogStore {
    pub fn open(state_dir: &Path, name: &str, keep: usize) -> io::Result<LogStore> {
        if !valid_name(name) {
            return Err(invalid("not a Chumby's name"));
        }
        let path = log_path(state_dir, name);
        std::fs::create_dir_all(path.parent().expect("in logs/"))?;
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let lines = BufReader::new(File::open(&path)?).split(b'\n').count();
        let mut s = LogStore { path, file, lines, keep: keep.max(2) };
        if s.lines >= s.keep / 2 {
            s.rotate()?;
        }
        Ok(s)
    }

    /// Appends `line`, as neochumby_proto::dev::clean_line leaves it.
    pub fn append(&mut self, line: &str) -> io::Result<()> {
        let mut l = clean_line(line);
        l.push('\n');
        self.file.write_all(l.as_bytes())?;
        self.lines += 1;
        if self.lines >= self.keep / 2 {
            self.rotate()?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        std::fs::rename(&self.path, old_path(&self.path))?;
        self.file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.lines = 0;
        Ok(())
    }
}

/// A Chumby's log as kept, oldest first.
pub fn read_log(state_dir: &Path, name: &str) -> io::Result<Vec<u8>> {
    let path = log_path(state_dir, name);
    let mut out = std::fs::read(old_path(&path)).unwrap_or_default();
    match File::open(&path) {
        Ok(mut f) => f.read_to_end(&mut out).map(|_| out),
        Err(e) if e.kind() == io::ErrorKind::NotFound && !out.is_empty() => Ok(out),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(io::Error::new(e.kind(), "no log yet")),
        Err(e) => Err(e),
    }
}

/// Prints a Chumby's log, then what's added to it, until `stop` says so;
/// across rotations.
pub fn follow(state_dir: &Path, name: &str, out: &mut impl Write, stop: impl Fn() -> bool) -> io::Result<()> {
    let path = log_path(state_dir, name);
    let mut f: Option<File> = None;
    if let Ok(old) = std::fs::read(old_path(&path)) {
        out.write_all(&old)?;
    }
    loop {
        if f.is_none() {
            f = File::open(&path).ok();
        }
        if let Some(file) = f.as_mut() {
            io::copy(file, out)?;
            out.flush()?;
            // Rotated: what was written to the old file is read above; the
            // new one is read from its start.
            let ino = file.metadata()?.ino();
            if std::fs::metadata(&path).is_ok_and(|m| m.ino() != ino) {
                io::copy(file, out)?;
                f = None;
                continue;
            }
        }
        if stop() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Bytes a second, on average, with a burst: a token bucket.
pub struct RateLimit {
    rate: u64,
    burst: u64,
    tokens: u64,
    at: Instant,
    /// Lines refused since the last one let through.
    pub dropped: u64,
}

impl RateLimit {
    pub fn new(rate: u64, burst: u64) -> RateLimit {
        RateLimit { rate, burst, tokens: burst, at: Instant::now(), dropped: 0 }
    }

    /// Whether `n` more bytes may go now.
    pub fn admit(&mut self, n: u64, now: Instant) -> bool {
        let earned = now.saturating_duration_since(self.at).as_millis() as u64 * self.rate / 1000;
        if earned > 0 {
            self.tokens = (self.tokens + earned).min(self.burst);
            self.at = now;
        }
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            self.dropped += 1;
            false
        }
    }
}

fn requests(state_dir: &Path, name: &str) -> PathBuf {
    state_dir.join("dev").join(name)
}

/// Whether `path` was there (and now isn't).
fn take(path: &Path) -> bool {
    std::fs::remove_file(path).is_ok()
}

/// Runs the developer link for paired Chumby `name` until either side
/// goes, the Chumby is no longer paired, or another link of its takes over.
pub fn run(api: &Api, name: &str, mut s: Session) -> io::Result<()> {
    let id = api.links.begin(name);
    let mut log = LogStore::open(&api.state_dir, name, LOG_LINES)?;
    let asks = requests(&api.state_dir, name);
    std::fs::create_dir_all(&asks)?;
    let mut limit = RateLimit::new(LOG_RATE, LOG_BURST);
    // A screenshot asked for: when, and what has come of it.
    let mut shot: Option<(Instant, Option<ShotIn>)> = None;
    // A tap or button sent and not yet answered: by when it must be.
    let mut input: Option<Instant> = None;
    log.append(&format!("-- neochumbyd: {name} connected, protocol version {}", s.version()))?;

    while api.links.current(name, id) && api.state.paired(s.peer()).as_deref() == Some(name) {
        match s.recv_timeout(TICK)? {
            None => {}
            Some(Msg::Log(lines)) => {
                let now = Instant::now();
                for l in lines {
                    if !limit.admit(l.len().min(neochumby_proto::dev::MAX_LINE) as u64 + 1, now) {
                        continue;
                    }
                    if limit.dropped > 0 {
                        let n = std::mem::take(&mut limit.dropped);
                        log.append(&format!("-- neochumbyd: {n} lines dropped: more than {LOG_RATE} bytes a second"))?;
                    }
                    log.append(&l)?;
                }
            }
            Some(Msg::Screen { width, height }) => match shot.take() {
                Some((at, None)) => match ShotIn::new(width, height) {
                    Some(screen) => shot = Some((at, Some(screen))),
                    None => {
                        let why = format!("{name} sent no screen ({width}x{height}); its log may say why");
                        std::fs::write(asks.join("shot.err"), &why)?;
                    }
                },
                _ => return Err(invalid("a screen not asked for")),
            },
            Some(Msg::Data(d)) => {
                let Some((_, Some(screen))) = shot.as_mut() else {
                    return Err(invalid("data not asked for"));
                };
                screen.add(&d).map_err(invalid)?;
                if let Some(px) = screen.done() {
                    let tmp = asks.join("shot.png.new");
                    std::fs::write(&tmp, png::rgb565_le(screen.width, screen.height, px))?;
                    std::fs::rename(tmp, asks.join("shot.png"))?;
                    shot = None;
                }
            }
            Some(Msg::Done { ok, why }) => {
                if input.take().is_none() {
                    return Err(invalid("an answer to nothing asked"));
                }
                if ok {
                    std::fs::write(asks.join("input.done"), "")?;
                } else {
                    std::fs::write(asks.join("input.err"), format!("{name}: {}", clean_line(&why)))?;
                }
            }
            // A newer Chumby's, that this neochumbyd may ignore.
            Some(Msg::Unknown { .. }) => {}
            Some(m) => return Err(invalid(format!("unexpected {m:?}"))),
        }
        if shot.as_ref().is_some_and(|(at, _)| at.elapsed() > SHOT_TIMEOUT) {
            shot = None;
            std::fs::write(asks.join("shot.err"), format!("{name} didn't send its screen in time"))?;
        }
        if input.is_some_and(|by| Instant::now() > by) {
            input = None;
            std::fs::write(asks.join("input.err"), format!("{name} didn't answer (is its session this recent?)"))?;
        }
        if input.is_none()
            && let Ok(line) = std::fs::read_to_string(asks.join("input"))
        {
            let _ = std::fs::remove_file(asks.join("input"));
            let settings = api.state.settings(name);
            match parse_input(&line) {
                Err(e) => std::fs::write(asks.join("input.err"), e)?,
                Ok(_) if !settings.dev => std::fs::write(asks.join("input.err"), input_off(name))?,
                Ok(m) => {
                    log.append(&format!("-- neochumbyd: {}", line.trim()))?;
                    // What the Chumby goes by: its settings as they are now.
                    s.send(&Msg::Config(settings.to_pairs()))?;
                    s.send(&m)?;
                    let hold = if let Msg::Tap { hold_ms, .. } = m { hold_ms } else { 0 };
                    input = Some(Instant::now() + Duration::from_millis(hold.into()) + INPUT_ANSWER);
                }
            }
        }
        if take(&asks.join("restart")) {
            log.append("-- neochumbyd: restart asked")?;
            s.send(&Msg::Restart)?;
        }
        if shot.is_none() && take(&asks.join("shot")) {
            s.send(&Msg::Shot)?;
            shot = Some((Instant::now(), None));
        }
    }
    Ok(())
}

/// Asks `name`'s session to restart, through the daemon: fails if the
/// daemon hasn't passed it on within `wait`, as when the Chumby isn't
/// connected.
pub fn restart(state_dir: &Path, name: &str, wait: Duration) -> io::Result<()> {
    let asks = requests(state_dir, name);
    std::fs::create_dir_all(&asks)?;
    let ask = asks.join("restart");
    std::fs::write(&ask, "")?;
    let deadline = Instant::now() + wait;
    while ask.exists() {
        if Instant::now() > deadline {
            let _ = std::fs::remove_file(&ask);
            return Err(not_connected(name));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// Why a Chumby not set `dev=on` takes no input.
pub fn input_off(name: &str) -> String {
    format!(
        "{name} takes no taps or button presses from here: its owner turns that on with `neochumbyd set {name} dev=on` (and off with `dev=`)"
    )
}

/// A tap or a button as the commands ask for it in dev/<name>/input:
/// `tap X Y HOLD_MS`, `button down`, `button up`.
pub fn input_line(m: &Msg) -> Option<String> {
    match m {
        Msg::Tap { x, y, hold_ms } => Some(format!("tap {x} {y} {hold_ms}\n")),
        Msg::Button { down } => Some(format!("button {}\n", if *down { "down" } else { "up" })),
        _ => None,
    }
}

/// The message an [`input_line`] asks for.
pub fn parse_input(line: &str) -> Result<Msg, String> {
    let w: Vec<&str> = line.split_whitespace().collect();
    let n = |s: &str| s.parse::<u16>().map_err(|_| format!("not a number: {}", clean_line(s)));
    match w[..] {
        ["tap", x, y, hold] => {
            let hold_ms = n(hold)?;
            if hold_ms > MAX_HOLD_MS {
                return Err(format!("a hold is at most {MAX_HOLD_MS} ms"));
            }
            Ok(Msg::Tap { x: n(x)?, y: n(y)?, hold_ms })
        }
        ["button", "down"] => Ok(Msg::Button { down: true }),
        ["button", "up"] => Ok(Msg::Button { down: false }),
        _ => Err(format!("not a tap or a button: {}", clean_line(line.trim()))),
    }
}

/// Asks `name` for a tap or a button (`m`) through the daemon, and waits
/// for the Chumby to say it's done. Fails if the daemon hasn't passed it on
/// within `wait`, the Chumby says why not, or it hasn't answered in time.
/// One at a time: fails if another is waiting.
pub fn input(state_dir: &Path, name: &str, m: &Msg, wait: Duration) -> io::Result<()> {
    let line = input_line(m).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a tap or a button"))?;
    let hold = match m {
        Msg::Tap { hold_ms, .. } => Duration::from_millis((*hold_ms).into()),
        _ => Duration::ZERO,
    };
    let asks = requests(state_dir, name);
    std::fs::create_dir_all(&asks)?;
    let (done, err, ask) = (asks.join("input.done"), asks.join("input.err"), asks.join("input"));
    let _ = std::fs::remove_file(&done);
    let _ = std::fs::remove_file(&err);
    // Written aside and linked into place, so the daemon never reads half
    // of it, and a request already waiting isn't replaced.
    let tmp = asks.join(format!("input.{}", std::process::id()));
    std::fs::write(&tmp, line)?;
    let linked = std::fs::hard_link(&tmp, &ask);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(io::Error::new(e.kind(), format!("another tap or button for {name} is waiting")));
        }
        r => r?,
    }
    let start = Instant::now();
    loop {
        if std::fs::remove_file(&done).is_ok() {
            return Ok(());
        }
        if let Ok(why) = std::fs::read_to_string(&err) {
            let _ = std::fs::remove_file(&err);
            return Err(io::Error::other(why));
        }
        if ask.exists() && start.elapsed() > wait {
            let _ = std::fs::remove_file(&ask);
            return Err(not_connected(name));
        }
        if start.elapsed() > wait + hold + INPUT_ANSWER + Duration::from_secs(1) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("no answer from {name}")));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn not_connected(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        format!("{name} isn't connected (is neochumbyd serving, and is {name}'s session running and this recent?)"),
    )
}

/// `name`'s screen, as a PNG at `out`, through the daemon: [`shot_png`].
pub fn shot(state_dir: &Path, name: &str, out: &Path, wait: Duration) -> io::Result<()> {
    std::fs::write(out, shot_png(state_dir, name, wait)?)
}

/// `name`'s screen, as a PNG, through the daemon. Fails if the daemon
/// hasn't passed the request on within `wait`, or no screen has come
/// within `wait` and [`SHOT_TIMEOUT`].
pub fn shot_png(state_dir: &Path, name: &str, wait: Duration) -> io::Result<Vec<u8>> {
    let asks = requests(state_dir, name);
    std::fs::create_dir_all(&asks)?;
    let (png, err) = (asks.join("shot.png"), asks.join("shot.err"));
    let _ = std::fs::remove_file(&png);
    let _ = std::fs::remove_file(&err);
    let ask = asks.join("shot");
    std::fs::write(&ask, "")?;
    let start = Instant::now();
    loop {
        if let Ok(bytes) = std::fs::read(&png) {
            let _ = std::fs::remove_file(&png);
            return Ok(bytes);
        }
        if let Ok(why) = std::fs::read_to_string(&err) {
            let _ = std::fs::remove_file(&err);
            return Err(io::Error::other(why));
        }
        if ask.exists() && start.elapsed() > wait {
            let _ = std::fs::remove_file(&ask);
            return Err(not_connected(name));
        }
        if start.elapsed() > wait + SHOT_TIMEOUT + Duration::from_secs(1) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("no screen from {name}")));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(test: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("neochumbyd-dev-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn lines(b: &[u8]) -> Vec<String> {
        String::from_utf8_lossy(b).lines().map(str::to_string).collect()
    }

    #[test]
    fn the_log_is_bounded_and_rotated() {
        let d = dir("rotate");
        let mut log = LogStore::open(&d, "den", 10).unwrap();
        for i in 0..23 {
            log.append(&format!("line {i}")).unwrap();
        }
        // Never more than 10 kept: the last 5 rotated out, and 3 since.
        let kept = lines(&read_log(&d, "den").unwrap());
        assert_eq!(kept, (15..23).map(|i| format!("line {i}")).collect::<Vec<_>>());
        drop(log);

        // Opened again, it counts what's there.
        let mut log = LogStore::open(&d, "den", 10).unwrap();
        log.append("again").unwrap();
        log.append("\x1b[2Jcleared?").unwrap();
        log.append(&"x".repeat(5000)).unwrap();
        let kept = lines(&read_log(&d, "den").unwrap());
        assert_eq!(kept.len(), 6);
        assert_eq!(kept[4], "?[2Jcleared?");
        assert!(kept[5].len() <= neochumby_proto::dev::MAX_LINE);
        // Bytes on disk stay bounded, whatever is sent.
        for _ in 0..1000 {
            log.append(&"y".repeat(5000)).unwrap();
        }
        assert!(read_log(&d, "den").unwrap().len() <= 10 * (neochumby_proto::dev::MAX_LINE + 1));

        assert!(LogStore::open(&d, "../den", 10).is_err());
        assert!(read_log(&d, "nobody").is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn following_survives_rotation() {
        let d = dir("follow");
        let mut log = LogStore::open(&d, "den", 4).unwrap();
        log.append("a").unwrap();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let d2 = d.clone();
        let flag = done.clone();
        let t = std::thread::spawn(move || {
            let mut out = Vec::new();
            follow(&d2, "den", &mut out, || flag.load(std::sync::atomic::Ordering::SeqCst)).unwrap();
            out
        });
        for l in ["b", "c", "d", "e"] {
            std::thread::sleep(Duration::from_millis(300));
            log.append(l).unwrap();
        }
        std::thread::sleep(Duration::from_millis(600));
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(lines(&t.join().unwrap()), ["a", "b", "c", "d", "e"]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_rate_is_limited() {
        let mut r = RateLimit::new(1000, 3000);
        let t0 = Instant::now();
        assert!(r.admit(3000, t0));
        assert!(!r.admit(1, t0));
        assert!(!r.admit(1, t0));
        assert_eq!(r.dropped, 2);
        // Half a second earns 500 bytes.
        assert!(r.admit(500, t0 + Duration::from_millis(500)));
        assert!(!r.admit(10, t0 + Duration::from_millis(500)));
        // Never more than the burst saved up.
        assert!(!r.admit(3001, t0 + Duration::from_secs(100)));
        assert!(r.admit(3000, t0 + Duration::from_secs(100)));
    }

    #[test]
    fn asking_an_absent_chumby_fails_and_leaves_nothing() {
        let d = dir("absent");
        let e = restart(&d, "den", Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotConnected);
        assert!(!d.join("dev/den/restart").exists());
        let e = shot(&d, "den", &d.join("out.png"), Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotConnected);
        assert!(!d.join("dev/den/shot").exists());
        let e = input(&d, "den", &Msg::Button { down: true }, Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotConnected);
        assert!(!d.join("dev/den/input").exists());
        // One at a time.
        std::fs::write(d.join("dev/den/input"), "button up\n").unwrap();
        let e = input(&d, "den", &Msg::Button { down: true }, Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(d.join("dev/den/input")).unwrap(), "button up\n");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn input_requests_round_trip() {
        for m in [
            Msg::Tap { x: 0, y: 0, hold_ms: 0 },
            Msg::Tap { x: 319, y: 239, hold_ms: MAX_HOLD_MS },
            Msg::Button { down: true },
            Msg::Button { down: false },
        ] {
            assert_eq!(parse_input(&input_line(&m).unwrap()), Ok(m));
        }
        for bad in ["", "tap 1 2", "tap 1 2 3 4", "tap -1 2 3", "tap 1 2 10001", "button", "button left", "shot"] {
            assert!(parse_input(bad).is_err(), "{bad}");
        }
        assert!(input_line(&Msg::Shot).is_none());
    }

    /// A fake Chumby on a real session with a real daemon: its log kept,
    /// and a restart and a screenshot asked of it by the commands.
    #[test]
    fn end_to_end() {
        use std::net::{TcpListener, TcpStream};
        use std::sync::Arc;

        use neochumby_proto::dev::shot_chunks;
        use neochumby_proto::{Identity, hex};

        use crate::state::State;

        let d = dir("e2e");
        let state = State::open(d.clone()).unwrap();
        let identity = state.identity().unwrap();
        let server_key = identity.public();
        let chumby = Identity::generate().unwrap();
        std::fs::write(d.join("paired").join(hex(&chumby.public())), "den\n").unwrap();
        let api = Api {
            state,
            identity,
            nfs_port: 2049,
            tree: d.join("tree"),
            state_dir: d.clone(),
            hashes: Default::default(),
            links: Links::default(),
            confine: crate::confine::Confine::Refused("not needed here".into()),
        };
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || crate::api::serve(l, Arc::new(api)));

        let mut s = Session::connect(TcpStream::connect(addr).unwrap(), &chumby).unwrap();
        assert_eq!(s.peer(), &server_key);
        s.send(&Msg::Hello { name: "den".into() }).unwrap();
        assert_eq!(s.recv().unwrap(), Msg::Welcome);
        assert!(matches!(s.recv().unwrap(), Msg::Config(_)));
        s.send(&Msg::Dev).unwrap();
        s.send(&Msg::Log(vec!["neochumby: opening home".into(), "\x1b[2Jboo".into()])).unwrap();
        let next = |s: &mut Session| loop {
            if let Some(m) = s.recv_timeout(Duration::from_millis(50)).unwrap() {
                return m;
            }
        };

        // Restart: the command returns once the daemon has asked.
        let state_dir = d.clone();
        let asked = std::thread::spawn(move || restart(&state_dir, "den", Duration::from_secs(5)));
        assert_eq!(next(&mut s), Msg::Restart);
        asked.join().unwrap().unwrap();

        // A screenshot: the Chumby sends its framebuffer in pieces.
        let (state_dir, out) = (d.clone(), d.join("den.png"));
        let asked = std::thread::spawn(move || shot(&state_dir, "den", &out, Duration::from_secs(5)));
        assert_eq!(next(&mut s), Msg::Shot);
        let px: Vec<u8> = [0xf800u16, 0x001f].repeat(320 * 120).iter().flat_map(|p| p.to_le_bytes()).collect();
        s.send(&Msg::Screen { width: 320, height: 240 }).unwrap();
        for (i, c) in shot_chunks(&px).enumerate() {
            s.send(&Msg::Data(c.to_vec())).unwrap();
            if i == 1 {
                // Log lines may come between the pieces.
                s.send(&Msg::Log(vec!["mid-shot".into()])).unwrap();
            }
        }
        asked.join().unwrap().unwrap();
        let (w, h, raw) = png::tests::read(&std::fs::read(d.join("den.png")).unwrap());
        assert_eq!((w, h), (320, 240));
        assert_eq!(&raw[..6], [255, 0, 0, 0, 0, 255]);

        // Input, without dev=on: refused, and nothing reaches the Chumby.
        let ask = |m: Msg| {
            let state_dir = d.clone();
            std::thread::spawn(move || input(&state_dir, "den", &m, Duration::from_secs(5)))
        };
        let e = ask(Msg::Tap { x: 10, y: 20, hold_ms: 0 }).join().unwrap().unwrap_err();
        assert_eq!(e.to_string(), input_off("den"));
        let e = ask(Msg::Button { down: true }).join().unwrap().unwrap_err();
        assert_eq!(e.to_string(), input_off("den"));
        assert_eq!(s.recv_timeout(Duration::from_millis(300)).unwrap(), None);

        // With it: the settings again, then the tap, answered once done.
        State::open(d.clone()).unwrap().set("den", "dev", "on").unwrap();
        let asked = ask(Msg::Tap { x: 10, y: 20, hold_ms: 100 });
        let on = vec![("dev".to_string(), "on".to_string())];
        assert_eq!(next(&mut s), Msg::Config(on.clone()));
        assert_eq!(next(&mut s), Msg::Tap { x: 10, y: 20, hold_ms: 100 });
        assert!(!asked.is_finished());
        s.send(&Msg::Done { ok: true, why: String::new() }).unwrap();
        asked.join().unwrap().unwrap();
        // The button, and why it couldn't be pressed.
        let asked = ask(Msg::Button { down: true });
        assert_eq!(next(&mut s), Msg::Config(on));
        assert_eq!(next(&mut s), Msg::Button { down: true });
        s.send(&Msg::Done { ok: false, why: "game has the button\x1b[2J".into() }).unwrap();
        assert_eq!(asked.join().unwrap().unwrap_err().to_string(), "den: game has the button?[2J");
        // Turned off again: refused once more.
        State::open(d.clone()).unwrap().set("den", "dev", "").unwrap();
        let e = ask(Msg::Button { down: false }).join().unwrap().unwrap_err();
        assert_eq!(e.to_string(), input_off("den"));
        assert_eq!(s.recv_timeout(Duration::from_millis(300)).unwrap(), None);

        // What it logged, cleaned, after the daemon's own line.
        let kept = lines(&read_log(&d, "den").unwrap());
        assert_eq!(
            kept,
            [
                // A Chumby from before versions: version 0.
                "-- neochumbyd: den connected, protocol version 0",
                "neochumby: opening home",
                "?[2Jboo",
                "-- neochumbyd: restart asked",
                "mid-shot",
                "-- neochumbyd: tap 10 20 100",
                "-- neochumbyd: button down",
            ]
        );

        // A screen nobody asked for ends the link.
        s.send(&Msg::Data(vec![0; 10])).unwrap();
        let gone = (0..100).any(|_| s.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(gone, "the link should have ended");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
