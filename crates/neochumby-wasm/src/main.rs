//! neochumby-wasm: runs an app built as WebAssembly, in its jail, with only
//! what its manifest grants (the library's documentation has the steps).
//!
//!   NEOCHUMBY_WASM=apps/clock/clock.wasm neochumby-wasm
//!   neochumby-wasm --selftest-seccomp      locks down, then makes a call
//!                                          the filter forbids: on a kernel
//!                                          with seccomp it dies of SIGSYS
//!
//! It exits with the module's own exit code, or 1 if the module trapped,
//! 2 if it was refused.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[cfg(target_os = "linux")]
use neochumby_sys::lockdown;
use neochumby_wasm::host::{Host, Input};
use neochumby_wasm::manifest::Manifest;
use neochumby_wasm::{ADDRESS_SPACE, MAX_PAGES, STACK, TRUSTED_ENV, WASM_ENV};

/// Elsewhere than Linux (a Mac, building the workspace) there's nothing to
/// lock down with.
#[cfg(not(target_os = "linux"))]
mod lockdown {
    #[derive(Default)]
    pub struct Locked {
        pub seccomp: bool,
    }

    impl std::fmt::Display for Locked {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("nothing (not Linux)")
        }
    }

    pub fn compute_only() {}

    pub enum Violation {
        Kill,
        Report,
    }

    pub fn lock_down(_: &(), _: Violation) -> std::io::Result<Locked> {
        Ok(Locked::default())
    }
}

/// The runner's own stdin, which may be waited on.
struct Stdin(std::io::Stdin);

impl Read for Stdin {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Input for Stdin {
    fn ready(&mut self, timeout_ms: i32) -> bool {
        neochumby_sys::fd::readable(&[self.0.as_fd()], timeout_ms).map(|r| r[0]).unwrap_or(true)
    }
}

/// The Chumby's devices, as the host API draws and reads them.
#[cfg(target_arch = "arm")]
mod devices {
    use neochumby::fb::Fb;
    use neochumby::input::{self, Touchscreen};
    use neochumby_wasm::host::{self, Screen, Touch};

    pub struct Screen565(pub Fb);

    impl Screen for Screen565 {
        fn size(&self) -> (u32, u32) {
            (neochumby::fb::W as u32, neochumby::fb::H as u32)
        }

        fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, c: u16) {
            self.0.rect(x as usize, y as usize, w as usize, h as usize, c);
        }

        fn blit(&mut self, x: u32, y: u32, w: u32, h: u32, px: &[u16], stride: u32) {
            let fb = self.0.pixels();
            let sw = neochumby::fb::W;
            for r in 0..h as usize {
                let src = &px[r * stride as usize..][..w as usize];
                let at = (y as usize + r) * sw + x as usize;
                fb[at..at + w as usize].copy_from_slice(src);
            }
        }

        fn text(&mut self, x: u32, y: u32, s: &str, scale: u32, c: u16) {
            self.0.text(x as usize, y as usize, s, scale as usize, c);
        }
    }

    /// The touchscreen, a report at a time.
    pub struct Touches(pub Touchscreen, pub std::collections::VecDeque<input::Touch>);

    impl host::Touchscreen for Touches {
        fn next(&mut self) -> Option<Touch> {
            if self.1.is_empty() {
                self.1.extend(self.0.poll());
            }
            self.1.pop_front().map(|t| match t {
                input::Touch::Down { x, y, .. } => Touch::Down(x, y),
                input::Touch::Up => Touch::Up,
            })
        }
    }
}

/// Gives `host` the devices `manifest` grants, opened now, before the
/// lock-down.
#[cfg(target_arch = "arm")]
fn grant_devices(host: &mut Host, manifest: &Manifest) -> Result<(), String> {
    if manifest.grants("fb") {
        let fb = neochumby::fb::Fb::open().map_err(|e| format!("the screen: {e}"))?;
        host.screen = Some(Box::new(devices::Screen565(fb)));
    }
    if manifest.grants("touch") {
        host.touch = Some(Box::new(devices::Touches(neochumby::input::Touchscreen::open(), Default::default())));
    }
    if manifest.grants("accel") {
        let mut a = neochumby::input::Accel::open();
        host.accel = Some(Box::new(move || a.read()));
    }
    Ok(())
}

/// Elsewhere (the host, for tests) there are no devices: text-mode apps
/// only.
#[cfg(not(target_arch = "arm"))]
fn grant_devices(_: &mut Host, manifest: &Manifest) -> Result<(), String> {
    match ["fb", "touch", "accel"].into_iter().find(|w| manifest.grants(w)) {
        Some(w) => Err(format!("\"{w}\": there is no {w} here")),
        None => Ok(()),
    }
}

/// Local time's offset from UTC now, in seconds, from the C library's zone.
fn utc_offset_s() -> i32 {
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let l = neochumby_sys::time::local_time_at(now);
    let local = i64::from(l.hour) * 3600 + i64::from(l.minute) * 60 + i64::from(l.second);
    // Within a day of UTC's time of day: -12 h to +14 h.
    let mut d = local - now.rem_euclid(86400);
    if d > 14 * 3600 {
        d -= 86400;
    } else if d < -12 * 3600 {
        d += 86400;
    }
    d as i32
}

/// The session's request pipe, if it gave one (neochumby::session).
fn session_pipe() -> Option<Box<dyn Write>> {
    let fd: i32 = std::env::var("NEOCHUMBY_SESSION").ok()?.parse().ok()?;
    let owned = neochumby_sys::fd::handed_over(fd)?;
    Some(Box::new(File::from(owned)))
}

/// The app's name: its directory's.
fn app_name(dir: &Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "wasm".into())
}

fn refuse(name: &str, why: &str) -> ExitCode {
    eprintln!("neochumby-wasm: {name}: refused: {why}");
    ExitCode::from(2)
}

/// Locks down and makes a call the filter doesn't allow.
fn selftest_seccomp() -> ExitCode {
    match lockdown::lock_down(&lockdown::compute_only(), lockdown::Violation::Kill) {
        Ok(l) if l.seccomp => {
            eprintln!("neochumby-wasm: selftest: locked down ({l}); now a forbidden call");
            let _ = std::fs::File::open("/");
            eprintln!("neochumby-wasm: selftest: FAILED: the forbidden call returned");
            ExitCode::from(1)
        }
        Ok(l) => {
            eprintln!("neochumby-wasm: selftest: no seccomp on this kernel ({l})");
            ExitCode::from(77)
        }
        Err(e) => {
            eprintln!("neochumby-wasm: selftest: lock-down failed: {e}");
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--selftest-seccomp") {
        return selftest_seccomp();
    }
    let Some(wasm_path) = std::env::var_os(WASM_ENV).or_else(|| std::env::args_os().nth(1)).map(PathBuf::from) else {
        eprintln!("usage: {WASM_ENV}=path/to/app.wasm neochumby-wasm");
        return ExitCode::from(2);
    };
    let dir = wasm_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let name = app_name(&dir);
    let manifest = Manifest::parse(&std::fs::read_to_string(dir.join("manifest")).unwrap_or_default(), &dir);
    let trusted = std::env::var(TRUSTED_ENV).as_deref() == Ok("1");

    let random: Box<dyn Read> = match File::open("/dev/urandom") {
        Ok(f) => Box::new(f),
        Err(e) => return refuse(&name, &format!("/dev/urandom: {e}")),
    };
    let mut host = Host::new(&name, Box::new(std::io::stdout()), Box::new(std::io::stderr()), random);
    host.stdin = Some(Box::new(Stdin(std::io::stdin())));
    host.session = session_pipe();
    host.sleep = neochumby_sys::time::sleep_ms;
    host.utc_offset_s = utc_offset_s();
    if let Err(e) = grant_devices(&mut host, &manifest) {
        return refuse(&name, &e);
    }

    let wasm = match std::fs::read(&wasm_path) {
        Ok(b) => b,
        Err(e) => return refuse(&name, &format!("{}: {e}", wasm_path.display())),
    };
    let aot = manifest.aot.as_ref().and_then(|p| std::fs::read(p).ok());
    let rt = match neochumby_wamr::Runtime::init() {
        Ok(rt) => rt,
        Err(e) => return refuse(&name, &e.to_string()),
    };
    if let Err(e) = neochumby_wasm::register(&rt) {
        return refuse(&name, &e.to_string());
    }
    let loaded = match neochumby_wasm::load(&rt, wasm, aot, &manifest) {
        Ok(l) => l,
        Err(why) => return refuse(&name, &why),
    };
    match &loaded.interpreted {
        Some(why) => eprintln!("neochumby-wasm: {name}: {why}"),
        None => eprintln!("neochumby-wasm: {name}: compiled (AOT)"),
    }
    // Limited first, so that a module asking for a huge memory can't have
    // it even for a moment.
    if let Err(e) = neochumby_sys::process::limit_memory(ADDRESS_SPACE) {
        return refuse(&name, &format!("limiting its memory: {e}"));
    }

    // Locked down before the module is instantiated: a start function runs
    // inside instantiation, and must run under the filter too. Nothing is
    // opened from here on, and nothing is dropped (closing is more than the
    // filter allows): a refusal exits there and then.
    // NEOCHUMBY_WASM_SECCOMP=report says which call a violation was, for
    // finding what the filter is missing; it still never makes the call.
    let violation = match std::env::var("NEOCHUMBY_WASM_SECCOMP").as_deref() {
        Ok("report") => lockdown::Violation::Report,
        _ => lockdown::Violation::Kill,
    };
    let locked = match lockdown::lock_down(&lockdown::compute_only(), violation) {
        Ok(l) => l,
        Err(e) => return refuse(&name, &format!("locking down: {e}")),
    };
    eprintln!("neochumby-wasm: {name}: locked down: {locked}");
    let refused = |why: &str| -> ! {
        eprintln!("neochumby-wasm: {name}: refused: {why}");
        std::process::exit(2)
    };
    if manifest.catalog && !locked.seccomp && !trusted {
        refused("a catalog app runs only on a kernel with seccomp (Linux 5 and later), unless its owner trusts it");
    }
    let mut inst = match loaded.module.instantiate(STACK, MAX_PAGES, Box::new(host)) {
        Ok(i) => i,
        Err(e) => refused(&e.to_string()),
    };
    // WAMR caps how far memory grows, not where it starts.
    if inst.memory_pages() > u64::from(MAX_PAGES) {
        refused(&format!("its memory starts above the cap of {MAX_PAGES} pages"));
    }
    let r = inst.call("_start");
    // Straight out, dropping nothing: closing what it holds is more than
    // the filter allows, and the kernel closes it all anyway.
    std::process::exit(neochumby_wasm::exit_code(r, &format!("neochumby-wasm: {name}")).into())
}
