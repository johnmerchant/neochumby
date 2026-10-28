//! End-to-end tests on an emulated Chumby Classic: the stock firmware
//! boots, and neochumby boots from a tree, opens an app, gets out of it by
//! the corner hold, and hands back to the stock software.
//!
//! The firmware is the developer's own (README.md, "Tests"), named by
//! NEOCHUMBY_EMU_KERNEL, NEOCHUMBY_EMU_NAND and NEOCHUMBY_EMU_PSP, and the
//! tree by NEOCHUMBY_EMU_TREE (tests/run builds one). A scenario whose
//! images aren't there is skipped, not failed.
//!
//!   cargo test --release --test scenarios [-- NAME...]
//!
//! Each scenario boots its own Chumby, in WORK/<name> (NEOCHUMBY_EMU_WORK,
//! else target/tmp/emutest), where its console log, screen dumps and
//! images stay for a look afterwards.

mod harness;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use harness::backend::Backend;
use harness::chumby_emu::ChumbyEmu;
use harness::images::{self, Inputs};
use harness::qemu::Qemu;
use harness::screen::Rgb;
use harness::{Chumby, Result, Screen, secs};

/// The stock boot, from power on to its console shell, with the first-boot
/// tour off: a few emulated minutes, longer on a slow host.
fn boot_timeout() -> Duration {
    secs(900)
}

// neochumby-home's colours (apps/home), as RGB565 shows them.
const HOME_BG: Rgb = (16, 16, 32);
const HOME_BUTTON: Rgb = (120, 200, 255);
// neochumby-probe's (tests/probe).
const MAGENTA: Rgb = (255, 0, 255);
const WHITE: Rgb = (255, 255, 255);

/// neochumby-home: its background, the CHUMBY MODE bar along the bottom,
/// and its orange title top left. The clock (top right) isn't looked at.
fn home(s: &Screen) -> std::result::Result<(), String> {
    s.expect("home's background", &[(160, 36), (300, 36), (2, 120), (317, 120)], HOME_BG)?;
    s.expect("the CHUMBY MODE bar", &[(8, 208), (312, 232), (60, 220)], HOME_BUTTON)?;
    let title = s.count(10, 12, 150, 16, |(r, g, b)| r > 200 && g > 140 && b < 170);
    if title < 40 {
        return Err(format!("home's title: {title} orange pixels"));
    }
    Ok(())
}

/// neochumby-home with the probe app as its first row (its colour bar).
fn home_with_probe(s: &Screen) -> std::result::Result<(), String> {
    home(s)?;
    s.expect("PROBE's row", &[(6, 50), (10, 76)], MAGENTA)
}

/// neochumby-probe's screen: magenta, with a white band across the middle.
fn probe(s: &Screen) -> std::result::Result<(), String> {
    s.expect("the probe's background", &[(5, 5), (314, 5), (5, 234), (314, 234), (160, 60)], MAGENTA)?;
    s.expect("the probe's band", &[(5, 104), (314, 136)], WHITE)
}

/// The stock software's UI: light and mostly pale blue, as its control
/// panel's screens are, unlike the boot logo (dark), a blank screen or
/// neochumby. Only that much is checked: nothing about its artwork.
fn stock_ui(s: &Screen) -> std::result::Result<(), String> {
    let light = s.fraction(|(r, g, b)| r.min(g).min(b) > 150);
    let blue = s.fraction(|(r, g, b)| b > r.saturating_add(20) && r.min(g).min(b) > 120);
    if light < 0.5 || blue < 0.2 {
        return Err(format!("not the stock UI: {:.0}% light, {:.0}% pale blue", light * 100.0, blue * 100.0));
    }
    Ok(())
}

/// Waits for the stock software: the console shell (the stock boot has
/// finished), its Flash player running, and its UI on the screen.
fn reaches_stock_ui(c: &mut Chumby) -> Result<()> {
    c.open_shell(boot_timeout())?;
    let pid = c.sh("pidof chumbyflashplayer.x", secs(60))?;
    if pid.trim().is_empty() {
        return Err("the stock Flash player isn't running".into());
    }
    c.wait_screen("stock-ui", secs(300), stock_ui)?;
    Ok(())
}

/// The stock firmware boots to its control panel.
fn stock_boot(c: &mut Chumby) -> Result<()> {
    c.expect("Starting /sbin/init", secs(300))?;
    reaches_stock_ui(c)
}

/// The boot hook runs neochumby from the stick, and the session shows
/// neochumby-home.
fn session_home(c: &mut Chumby) -> Result<()> {
    c.expect("emutest: stick mounted", boot_timeout())?;
    c.expect("apps from the stick", secs(60))?;
    c.wait_screen("home", secs(300), home)?;
    Ok(())
}

/// An app opened from home, then the corner hold ends it and home comes
/// back.
fn app_and_escape(c: &mut Chumby) -> Result<()> {
    session_home(c)?;
    c.wait_screen("home-with-probe", secs(10), home_with_probe)?;
    c.tap(160, 62)?;
    c.expect("neochumby-probe: running", secs(120))?;
    c.wait_screen("probe", secs(120), probe)?;
    // A tap reaches the app as a touch where it was made.
    c.tap(200, 180)?;
    c.expect("neochumby-probe: down", secs(60))?;
    c.hold(8, 8, Duration::from_secs(4))?;
    c.expect("neochumby: escape: ending", secs(120))?;
    c.wait_screen("home-again", secs(300), home)?;
    // And stays: the new home screen opened while the corner was still
    // held, and the release must not be taken for a tap anywhere.
    std::thread::sleep(Duration::from_secs(5));
    if c.seen("neochumby exited") {
        return Err("the session ended after the corner hold".into());
    }
    c.wait_screen("home-stays", secs(10), home)?;
    Ok(())
}

/// CHUMBY MODE ends neochumby, and the stock boot carries on to its UI.
fn chumby_mode(c: &mut Chumby) -> Result<()> {
    session_home(c)?;
    c.tap(160, 220)?;
    c.expect("neochumby exited: 0", secs(120))?;
    c.expect("emutest: hook returned", secs(60))?;
    reaches_stock_ui(c)
}

struct Scenario {
    name: &'static str,
    /// Boots neochumby from NEOCHUMBY_EMU_TREE.
    tree: bool,
    run: fn(&mut Chumby) -> Result<()>,
}

const SCENARIOS: &[Scenario] = &[
    Scenario { name: "stock_boot", tree: false, run: stock_boot },
    Scenario { name: "session_home", tree: true, run: session_home },
    Scenario { name: "app_and_escape", tree: true, run: app_and_escape },
    Scenario { name: "chumby_mode", tree: true, run: chumby_mode },
];

fn backend() -> Box<dyn Backend> {
    match std::env::var("NEOCHUMBY_EMU_BACKEND").as_deref() {
        Ok("qemu") => Box::new(Qemu),
        _ => Box::new(ChumbyEmu::built()),
    }
}

enum Outcome {
    Passed,
    Skipped(String),
    Failed(String),
}

fn run(s: &Scenario, backend: &dyn Backend, work: &std::path::Path) -> Outcome {
    if let Err(why) = backend.available() {
        return Outcome::Skipped(why);
    }
    let inputs = match Inputs::from_env() {
        Ok(i) => i,
        Err(why) => return Outcome::Skipped(why),
    };
    if s.tree && inputs.tree.is_none() {
        return Outcome::Skipped("NEOCHUMBY_EMU_TREE is not set".into());
    }
    let r = images::build(&inputs, s.tree, &ChumbyEmu::built().exe, work)
        .and_then(|media| Chumby::boot(backend, &media, work))
        .and_then(|mut c| {
            let r = (s.run)(&mut c);
            let _ = c.stop();
            r
        });
    match r {
        Ok(()) => Outcome::Passed,
        Err(e) => Outcome::Failed(e),
    }
}

fn main() -> ExitCode {
    // Names to run (all if none); cargo's own flags are ignored.
    let only: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    if std::env::args().any(|a| a == "--list") {
        for s in SCENARIOS {
            println!("{}: test", s.name);
        }
        return ExitCode::SUCCESS;
    }
    let backend = backend();
    let root = std::env::var_os("NEOCHUMBY_EMU_WORK")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("emutest"));
    let chosen: Vec<&Scenario> =
        SCENARIOS.iter().filter(|s| only.is_empty() || only.iter().any(|o| s.name.contains(o.as_str()))).collect();
    println!("\nrunning {} emulator scenarios on {}", chosen.len(), backend.name());
    let (mut passed, mut skipped, mut failed) = (0, 0, Vec::new());
    for s in chosen {
        let work = root.join(s.name);
        let t = Instant::now();
        match run(s, backend.as_ref(), &work) {
            Outcome::Passed => {
                passed += 1;
                println!("scenario {} ... ok ({:.0}s)", s.name, t.elapsed().as_secs_f64());
            }
            Outcome::Skipped(why) => {
                skipped += 1;
                println!("scenario {} ... skipped: {why}", s.name);
            }
            Outcome::Failed(e) => {
                println!(
                    "scenario {} ... FAILED ({:.0}s): {e}\n    see {}",
                    s.name,
                    t.elapsed().as_secs_f64(),
                    work.display()
                );
                failed.push(s.name);
            }
        }
    }
    println!("\n{passed} passed, {skipped} skipped, {} failed", failed.len());
    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
