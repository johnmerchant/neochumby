//! The host API: everything a module can call, and so everything it can
//! do. Two import modules:
//!
//! - `neochumby`: drawing as whole operations (fills, blits, text, run by
//!   native code, not as per-pixel wasm), touches, the accelerometer, the
//!   time, sleeping, the log and the session's request pipe. Each needs its
//!   access word ([`crate::grant`]), and also checks it holds what that
//!   word gave it, so an ungranted call can only trap.
//! - `wasi_snapshot_preview1`, deliberately small, so that a program built
//!   for `wasm32-wasip1` (Rust's std, or C with wasi-libc) runs as it is:
//!   stdin, stdout and stderr and nothing else (no preopened directories,
//!   so no file system), clocks, random bytes, sleeping, exit, and empty
//!   arguments and environment. A text-mode app's terminal is its stdin
//!   and stdout, as the session gave them to the runner.
//!
//! Every pointer a module passes is checked against its memory: out of
//! bounds traps.

use std::io::{Read, Write};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use neochumby_wamr::{Caller, Func, Memory, Trap, Val};

/// The screen, as the host draws on it: 16-bit RGB565.
pub trait Screen {
    /// Its width and height, in pixels.
    fn size(&self) -> (u32, u32);
    /// Fills a rectangle, already clipped to the screen.
    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, rgb565: u16);
    /// Copies `h` rows of `w` pixels from `px`, row `r` at `px[r * stride
    /// ..]`, to (x, y); already clipped.
    fn blit(&mut self, x: u32, y: u32, w: u32, h: u32, px: &[u16], stride: u32);
    /// Draws `s` in the library's 8x8 font, each pixel `scale` pixels square.
    fn text(&mut self, x: u32, y: u32, s: &str, scale: u32, rgb565: u16);
}

/// A touch, as `touch_poll` reports it: down (or moved) at (x, y), or up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    Down(i32, i32),
    Up,
}

/// The touchscreen.
pub trait Touchscreen {
    /// The next touch since the last, if any.
    fn next(&mut self) -> Option<Touch>;
}

/// stdin, which may be waited on.
pub trait Input: Read {
    /// Whether it has something to read (or has closed) within
    /// `timeout_ms` (forever if negative).
    fn ready(&mut self, timeout_ms: i32) -> bool;
}

/// What the host functions work with: only what the app was granted.
pub struct Host {
    pub stdin: Option<Box<dyn Input>>,
    pub stdout: Box<dyn Write>,
    pub stderr: Box<dyn Write>,
    /// `fb`: the framebuffer, drawn on through the host.
    pub screen: Option<Box<dyn Screen>>,
    /// `touch`.
    pub touch: Option<Box<dyn Touchscreen>>,
    /// `accel`: a reading of the three axes.
    pub accel: Option<Box<dyn FnMut() -> Option<[i32; 3]>>>,
    /// The session's request pipe, for `request`.
    pub session: Option<Box<dyn Write>>,
    /// Random bytes (/dev/urandom, opened before the lock-down).
    pub random: Box<dyn Read>,
    /// Sleeps about this many milliseconds.
    pub sleep: fn(u32),
    /// The app's name, for its log lines.
    pub name: String,
    start: Instant,
}

impl Host {
    /// A host with nothing granted yet, writing to `stdout` and `stderr`.
    pub fn new(name: &str, stdout: Box<dyn Write>, stderr: Box<dyn Write>, random: Box<dyn Read>) -> Host {
        Host {
            stdin: None,
            stdout,
            stderr,
            screen: None,
            touch: None,
            accel: None,
            session: None,
            random,
            sleep: |ms| std::thread::sleep(std::time::Duration::from_millis(ms.into())),
            name: name.to_string(),
            start: Instant::now(),
        }
    }
}

const I: Val = Val::I32;
const L: Val = Val::I64;

fn host<'a>(c: &'a mut Caller<'_>) -> Result<&'a mut Host, Trap> {
    c.state::<Host>().ok_or_else(|| Trap::Other("no host state".into()))
}

fn split<'a, 'b>(c: &'a mut Caller<'b>) -> Result<(&'a mut Memory<'b>, &'a mut Host), Trap> {
    c.split::<Host>().ok_or_else(|| Trap::Other("no host state".into()))
}

/// A u32 argument (wasm's i32, as an unsigned offset or length).
fn u(a: u64) -> u32 {
    a as u32
}

fn not_granted(what: &str) -> Trap {
    Trap::Other(format!("{what}: not granted"))
}

// ---- neochumby ----

/// The `neochumby` imports every app has.
pub const ALWAYS: &[&str] = &["now_ms", "sleep_ms", "log", "request"];

/// `fb_size() -> (width << 16) | height`.
fn fb_size(c: &mut Caller<'_>, _: &[u64]) -> Result<u64, Trap> {
    let s = host(c)?.screen.as_ref().ok_or_else(|| not_granted("fb_size"))?;
    let (w, h) = s.size();
    Ok(u64::from((w << 16) | h))
}

/// A rectangle clipped to a w x h screen, or None if none of it is on it.
fn clip(x: u32, y: u32, w: u32, h: u32, (sw, sh): (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    if x >= sw || y >= sh || w == 0 || h == 0 {
        return None;
    }
    Some((x, y, w.min(sw - x), h.min(sh - y)))
}

/// `fb_fill(x, y, w, h, rgb565)`.
fn fb_fill(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let s = host(c)?.screen.as_mut().ok_or_else(|| not_granted("fb_fill"))?;
    if let Some((x, y, w, h)) = clip(u(a[0]), u(a[1]), u(a[2]), u(a[3]), s.size()) {
        s.fill(x, y, w, h, a[4] as u16);
    }
    Ok(0)
}

/// `fb_blit(x, y, w, h, ptr, stride)`: `h` rows of `w` RGB565 pixels from
/// the module's memory at `ptr`, `stride` pixels apart. The whole source is
/// checked against the module's memory, clipped or not.
fn fb_blit(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (x, y, w, h, ptr, stride) = (u(a[0]), u(a[1]), u(a[2]), u(a[3]), u(a[4]), u(a[5]));
    if w == 0 || h == 0 {
        return Ok(0);
    }
    if stride < w {
        return Err(Trap::Other("fb_blit: stride narrower than the width".into()));
    }
    let len = u64::from(stride) * u64::from(h - 1) + u64::from(w);
    let bytes = u32::try_from(len * 2).map_err(|_| Trap::Bounds)?;
    let (mem, host) = split(c)?;
    let s = host.screen.as_mut().ok_or_else(|| not_granted("fb_blit"))?;
    let src = mem.bytes(ptr, bytes)?;
    let Some((x, y, cw, ch)) = clip(x, y, w, h, s.size()) else { return Ok(0) };
    let px: Vec<u16> = src.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes(*p)).collect();
    s.blit(x, y, cw, ch, &px, stride);
    Ok(0)
}

/// `fb_text(x, y, ptr, len, scale, rgb565)`: UTF-8 text.
fn fb_text(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (mem, host) = split(c)?;
    let s = host.screen.as_mut().ok_or_else(|| not_granted("fb_text"))?;
    let text = String::from_utf8_lossy(mem.bytes(u(a[2]), u(a[3]))?).into_owned();
    let (sw, sh) = s.size();
    let (x, y, scale) = (u(a[0]), u(a[1]), u(a[4]).clamp(1, 16));
    if x < sw && y < sh {
        s.text(x, y, &text, scale, a[5] as u16);
    }
    Ok(0)
}

/// `touch_poll(ptr) -> 1 | 0`: the next touch as three i32s at `ptr`
/// (1 down or moved, 0 up; x; y), or 0 if there's none.
fn touch_poll(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (mem, host) = split(c)?;
    let t = host.touch.as_mut().ok_or_else(|| not_granted("touch_poll"))?;
    let out = mem.bytes_mut(u(a[0]), 12)?;
    let (down, x, y) = match t.next() {
        None => return Ok(0),
        Some(Touch::Down(x, y)) => (1i32, x, y),
        Some(Touch::Up) => (0, 0, 0),
    };
    out[0..4].copy_from_slice(&down.to_le_bytes());
    out[4..8].copy_from_slice(&x.to_le_bytes());
    out[8..12].copy_from_slice(&y.to_le_bytes());
    Ok(1)
}

/// `accel(ptr) -> 1 | 0`: the three axes as i32s at `ptr`, or 0 if there's
/// no reading.
fn accel(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (mem, host) = split(c)?;
    let read = host.accel.as_mut().ok_or_else(|| not_granted("accel"))?;
    let out = mem.bytes_mut(u(a[0]), 12)?;
    let Some(v) = read() else { return Ok(0) };
    for (i, n) in v.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&n.to_le_bytes());
    }
    Ok(1)
}

/// `now_ms() -> i64`: milliseconds since the app started, never going back.
fn now_ms(c: &mut Caller<'_>, _: &[u64]) -> Result<u64, Trap> {
    Ok(host(c)?.start.elapsed().as_millis() as u64)
}

/// `sleep_ms(ms)`.
fn sleep_ms(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    (host(c)?.sleep)(u(a[0]));
    Ok(0)
}

/// `log(ptr, len)`: a line in the session's log, as the app's.
fn log(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (mem, host) = split(c)?;
    let text = String::from_utf8_lossy(mem.bytes(u(a[0]), u(a[1]))?).into_owned();
    for line in text.lines() {
        let _ = writeln!(host.stderr, "{}: {line}", host.name);
    }
    Ok(0)
}

/// `request(ptr, len) -> 0 | -1`: one line to the session (`open <app>`, as
/// neochumby::session's requests): -1 if it has no pipe or the line isn't
/// one line.
fn request(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (mem, host) = split(c)?;
    let line = mem.bytes(u(a[0]), u(a[1]))?.to_vec();
    let Some(pipe) = host.session.as_mut() else { return Ok(-1i32 as u32 as u64) };
    if line.is_empty() || line.len() > 256 || line.contains(&b'\n') {
        return Ok(-1i32 as u32 as u64);
    }
    let ok = pipe.write_all(&line).and_then(|_| pipe.write_all(b"\n")).and_then(|_| pipe.flush()).is_ok();
    Ok(if ok { 0 } else { -1i32 as u32 as u64 })
}

/// The `neochumby` module.
pub static NEOCHUMBY: [Func; 10] = [
    Func { name: "fb_size", params: &[], result: Some(I), call: fb_size },
    Func { name: "fb_fill", params: &[I, I, I, I, I], result: None, call: fb_fill },
    Func { name: "fb_blit", params: &[I, I, I, I, I, I], result: None, call: fb_blit },
    Func { name: "fb_text", params: &[I, I, I, I, I, I], result: None, call: fb_text },
    Func { name: "touch_poll", params: &[I], result: Some(I), call: touch_poll },
    Func { name: "accel", params: &[I], result: Some(I), call: accel },
    Func { name: "now_ms", params: &[], result: Some(L), call: now_ms },
    Func { name: "sleep_ms", params: &[I], result: None, call: sleep_ms },
    Func { name: "log", params: &[I, I], result: None, call: log },
    Func { name: "request", params: &[I, I], result: Some(I), call: request },
];

// ---- wasi_snapshot_preview1 ----

/// WASI's errno values the host returns.
const SUCCESS: u64 = 0;
const EBADF: u64 = 8;
const EINVAL: u64 = 28;
const EIO: u64 = 29;
const ESPIPE: u64 = 70;

fn put_u32(mem: &mut Memory<'_>, at: u32, v: u32) -> Result<(), Trap> {
    mem.bytes_mut(at, 4)?.copy_from_slice(&v.to_le_bytes());
    Ok(())
}

fn put_u64(mem: &mut Memory<'_>, at: u32, v: u64) -> Result<(), Trap> {
    mem.bytes_mut(at, 8)?.copy_from_slice(&v.to_le_bytes());
    Ok(())
}

/// `args_sizes_get`, `environ_sizes_get`: none, and no bytes.
fn sizes_none(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (mem, _) = split(c)?;
    put_u32(mem, u(a[0]), 0)?;
    put_u32(mem, u(a[1]), 0)?;
    Ok(SUCCESS)
}

/// `args_get`, `environ_get`: nothing to write.
fn get_none(_: &mut Caller<'_>, _: &[u64]) -> Result<u64, Trap> {
    Ok(SUCCESS)
}

/// The time on clock `id` in nanoseconds: realtime (0), or monotonic (1;
/// and 2 and 3, the process's and thread's CPU time, stand-ins).
fn clock(host: &Host, id: u32) -> Option<u64> {
    match id {
        0 => SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_nanos() as u64),
        1..=3 => Some(host.start.elapsed().as_nanos() as u64),
        _ => None,
    }
}

/// `clock_res_get(id, out)`: a microsecond, near enough.
fn clock_res_get(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    if u(a[0]) > 3 {
        return Ok(EINVAL);
    }
    let (mem, _) = split(c)?;
    put_u64(mem, u(a[1]), 1000)?;
    Ok(SUCCESS)
}

/// `clock_time_get(id, precision, out)`.
fn clock_time_get(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (mem, host) = split(c)?;
    let Some(t) = clock(host, u(a[0])) else { return Ok(EINVAL) };
    put_u64(mem, u(a[2]), t)?;
    Ok(SUCCESS)
}

/// The buffers of an iovec array: (offset, length) pairs.
fn iovecs(mem: &Memory<'_>, at: u32, n: u32) -> Result<Vec<(u32, u32)>, Trap> {
    if n > 1024 {
        return Err(Trap::Other("too many iovecs".into()));
    }
    let raw = mem.bytes(at, n * 8)?;
    Ok(raw
        .as_chunks::<8>()
        .0
        .iter()
        .map(|v| (u32::from_le_bytes([v[0], v[1], v[2], v[3]]), u32::from_le_bytes([v[4], v[5], v[6], v[7]])))
        .collect())
}

/// `fd_write(fd, iovs, n, written)`: to stdout (1) or stderr (2) only.
fn fd_write(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (mem, host) = split(c)?;
    let out: &mut dyn Write = match u(a[0]) {
        1 => &mut *host.stdout,
        2 => &mut *host.stderr,
        _ => return Ok(EBADF),
    };
    let mut total = 0u32;
    for (p, len) in iovecs(mem, u(a[1]), u(a[2]))? {
        if out.write_all(mem.bytes(p, len)?).is_err() {
            return Ok(EIO);
        }
        total = total.saturating_add(len);
    }
    let _ = out.flush();
    put_u32(mem, u(a[3]), total)?;
    Ok(SUCCESS)
}

/// `fd_read(fd, iovs, n, read)`: from stdin (0) only, into its first
/// non-empty buffer, as much as one read gives.
fn fd_read(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (mem, host) = split(c)?;
    if u(a[0]) != 0 {
        return Ok(EBADF);
    }
    let Some(stdin) = host.stdin.as_mut() else { return Ok(EBADF) };
    let mut n = 0;
    if let Some((p, len)) = iovecs(mem, u(a[1]), u(a[2]))?.into_iter().find(|(_, l)| *l > 0) {
        let buf = mem.bytes_mut(p, len)?;
        n = match stdin.read(buf) {
            Ok(n) => n as u32,
            Err(_) => return Ok(EIO),
        };
    }
    put_u32(mem, u(a[3]), n)?;
    Ok(SUCCESS)
}

/// `fd_close(fd)`: stdio may be "closed"; nothing else is open.
fn fd_close(_: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    Ok(if u(a[0]) <= 2 { SUCCESS } else { EBADF })
}

/// `fd_seek`: stdio can't.
fn fd_seek(_: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    Ok(if u(a[0]) <= 2 { ESPIPE } else { EBADF })
}

/// `fd_fdstat_get(fd, out)`: stdio is a character device, nothing else is
/// open.
fn fd_fdstat_get(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    if u(a[0]) > 2 {
        return Ok(EBADF);
    }
    let (mem, _) = split(c)?;
    let out = mem.bytes_mut(u(a[1]), 24)?;
    out.fill(0);
    // The filetype, character_device; reading or writing, as the fd is.
    out[0] = 2;
    let rights: u64 = if u(a[0]) == 0 { 1 << 1 } else { 1 << 6 };
    out[8..16].copy_from_slice(&rights.to_le_bytes());
    Ok(SUCCESS)
}

/// `fd_prestat_get`, `fd_prestat_dir_name`: there are no preopened
/// directories.
fn no_preopens(_: &mut Caller<'_>, _: &[u64]) -> Result<u64, Trap> {
    Ok(EBADF)
}

/// `proc_exit(code)`.
fn proc_exit(_: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    Err(Trap::Exit(u(a[0])))
}

/// `random_get(buf, len)`.
fn random_get(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (mem, host) = split(c)?;
    let buf = mem.bytes_mut(u(a[0]), u(a[1]))?;
    Ok(if host.random.read_exact(buf).is_ok() { SUCCESS } else { EIO })
}

/// `sched_yield()`.
fn sched_yield(_: &mut Caller<'_>, _: &[u64]) -> Result<u64, Trap> {
    Ok(SUCCESS)
}

/// `poll_oneoff(in, out, n, nevents)`: clock subscriptions (sleeping) and
/// stdin's readiness; anything else gets an EBADF event. It waits for the
/// earliest clock, or stdin, whichever comes first.
fn poll_oneoff(c: &mut Caller<'_>, a: &[u64]) -> Result<u64, Trap> {
    let (mem, host) = split(c)?;
    let (subs, out, n) = (u(a[0]), u(a[1]), u(a[2]));
    if n == 0 || n > 64 {
        return Ok(EINVAL);
    }
    let raw = mem.bytes(subs, n * 48)?.to_vec();
    let now = host.start.elapsed().as_nanos() as u64;
    let mut wait_ns: Option<u64> = None;
    let mut stdin_sub = None;
    let mut bad = Vec::new();
    let mut clocks = Vec::new();
    for (i, s) in raw.as_chunks::<48>().0.iter().enumerate() {
        let userdata = u64::from_le_bytes(s[0..8].try_into().unwrap());
        match s[8] {
            0 => {
                let id = u32::from_le_bytes(s[16..20].try_into().unwrap());
                let t = u64::from_le_bytes(s[24..32].try_into().unwrap());
                let abs = u16::from_le_bytes([s[40], s[41]]) & 1 != 0;
                let Some(at) = clock(host, id) else {
                    bad.push((userdata, 0u8));
                    continue;
                };
                let ns = if abs { t.saturating_sub(at) } else { t };
                wait_ns = Some(wait_ns.map_or(ns, |w| w.min(ns)));
                clocks.push((i, userdata, ns));
            }
            1 if u32::from_le_bytes(s[16..20].try_into().unwrap()) == 0 && host.stdin.is_some() => {
                stdin_sub = Some(userdata);
            }
            kind => bad.push((userdata, kind)),
        }
    }
    let mut events: Vec<[u8; 32]> = Vec::new();
    let event = |userdata: u64, errno: u16, kind: u8| {
        let mut e = [0u8; 32];
        e[0..8].copy_from_slice(&userdata.to_le_bytes());
        e[8..10].copy_from_slice(&errno.to_le_bytes());
        e[10] = kind;
        e
    };
    for (userdata, kind) in &bad {
        events.push(event(*userdata, EBADF as u16, *kind));
    }
    if events.is_empty() {
        let ms = wait_ns.map(|ns| ns.div_ceil(1_000_000).min(i32::MAX as u64) as i32);
        let ready = match (stdin_sub, host.stdin.as_mut()) {
            (Some(_), Some(i)) => i.ready(ms.unwrap_or(-1)),
            _ => {
                if let Some(ms) = ms {
                    (host.sleep)(ms as u32);
                }
                false
            }
        };
        if let (true, Some(ud)) = (ready, stdin_sub) {
            let mut e = event(ud, 0, 1);
            e[16..24].copy_from_slice(&1u64.to_le_bytes());
            events.push(e);
        }
        let elapsed = (host.start.elapsed().as_nanos() as u64).saturating_sub(now);
        for (_, ud, ns) in &clocks {
            if *ns <= elapsed || !ready {
                events.push(event(*ud, 0, 0));
            }
        }
    }
    let out_bytes = mem.bytes_mut(out, events.len() as u32 * 32)?;
    for (i, e) in events.iter().enumerate() {
        out_bytes[i * 32..i * 32 + 32].copy_from_slice(e);
    }
    put_u32(mem, u(a[3]), events.len() as u32)?;
    Ok(SUCCESS)
}

/// The `wasi_snapshot_preview1` module: only these.
pub static WASI: [Func; 17] = [
    Func { name: "args_sizes_get", params: &[I, I], result: Some(I), call: sizes_none },
    Func { name: "args_get", params: &[I, I], result: Some(I), call: get_none },
    Func { name: "environ_sizes_get", params: &[I, I], result: Some(I), call: sizes_none },
    Func { name: "environ_get", params: &[I, I], result: Some(I), call: get_none },
    Func { name: "clock_res_get", params: &[I, I], result: Some(I), call: clock_res_get },
    Func { name: "clock_time_get", params: &[I, L, I], result: Some(I), call: clock_time_get },
    Func { name: "fd_write", params: &[I, I, I, I], result: Some(I), call: fd_write },
    Func { name: "fd_read", params: &[I, I, I, I], result: Some(I), call: fd_read },
    Func { name: "fd_close", params: &[I], result: Some(I), call: fd_close },
    Func { name: "fd_seek", params: &[I, L, I, I], result: Some(I), call: fd_seek },
    Func { name: "fd_fdstat_get", params: &[I, I], result: Some(I), call: fd_fdstat_get },
    Func { name: "fd_prestat_get", params: &[I, I], result: Some(I), call: no_preopens },
    Func { name: "fd_prestat_dir_name", params: &[I, I, I], result: Some(I), call: no_preopens },
    Func { name: "proc_exit", params: &[I], result: None, call: proc_exit },
    Func { name: "random_get", params: &[I, I], result: Some(I), call: random_get },
    Func { name: "sched_yield", params: &[], result: Some(I), call: sched_yield },
    Func { name: "poll_oneoff", params: &[I, I, I, I], result: Some(I), call: poll_oneoff },
];
