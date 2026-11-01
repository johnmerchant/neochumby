//! Transitions: an animation from one app's screen to the next app's,
//! played by a program the owner chooses.
//!
//! An app provides one by naming a binary in its manifest's `transition`;
//! `transition=<app>` in this Chumby's settings from its neochumbyd, else
//! in /psp/neochumby.conf, chooses it, as `player` is chosen (player.rs).
//! Unset, apps switch as they always have, with no animation.
//!
//! It needs the LCD controller's overlay (the i.MX21's graphic window):
//! /dev/fb1, the screen's size and layout, composited over /dev/fb0 by the
//! hardware as /proc/driver/imxfb/{enable,alpha,key,key_en} say. A kernel
//! without it has no transitions: Linux 7's imxfb doesn't have it yet.
//!
//! As one app leaves, the session copies the screen into the overlay and
//! makes it opaque (cover), so the screen holds still while the next app
//! starts and draws underneath, unseen. Once that app's screen has settled
//! (it changed, then held still for two looks, or [`SETTLE`] passed; the
//! cover is put back meanwhile if setting fb0's mode undid it), the session runs the transition program, jailed
//! as its app with nothing beyond its own directory and /tmp, and
//! NEOCHUMBY_FROM and NEOCHUMBY_TO naming the two apps. On its stdin it
//! gets two pictures, the screen as the first app left it and as the
//! second has drawn it, each 320x240 RGB565, little-endian, a line after
//! another (153,600 bytes), and then end of file. What it writes to stdout is frames in the same form, shown
//! in the overlay as each arrives: it paces them itself. When it closes
//! stdout, or after [`LONGEST`], it's ended with whatever it started, and
//! the overlay is put back as it was, which shows the app beneath.
//!
//! An app that draws nothing within [`FIRST_DRAW`] gets no animation: the
//! overlay is put back, and the screen is the app's. Whatever happens next
//! (the app exits, the escape hatch, a service's request) cuts a
//! transition short first, so the overlay never covers a screen for longer
//! than that. Taps while the screen is covered go to the new app, as
//! always.
//!
//! The work runs on a thread of its own, so the session's loop carries on
//! watching the app, the button and the services meanwhile. It's told to
//! stop, and waited on, with atomics. The screens are read and written with pread and
//! pwrite, not mapped, since a mapping is never unmapped (DeviceMap).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use neochumby::apps::{App, WASM_ENV, WASM_TRUSTED_ENV};
use neochumby::fb::{H, W};
use neochumby::service;
use neochumby::session;
use neochumby_sys::jail::Process;

use crate::child;
use crate::jail::{self, Role};

/// One picture, as the program reads and writes them.
pub const FRAME: usize = W * H * 2;

/// How long the next app has to draw something.
const FIRST_DRAW: Duration = Duration::from_secs(2);
/// How long after its first change the screen is taken as it is, held
/// still or not (an app that animates from the start).
const SETTLE: Duration = Duration::from_millis(600);
/// How often the screen is looked at while it settles.
const LOOK_MS: u32 = 40;
/// The longest a program may play.
const LONGEST: Duration = Duration::from_secs(3);
/// Between SIGTERM and SIGKILL, when the program is ended.
const PATIENCE: Duration = Duration::from_millis(200);
/// How long stopping a transition is waited for.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// The app `choice` names (where the name came from, and the name), if
/// it may provide the transition here; else none, with why logged.
pub fn choose<'a>(apps: &'a [App], choice: Option<(&str, &str)>) -> Option<&'a App> {
    let (from, name) = choice?;
    let why = match apps.iter().find(|a| a.name == name) {
        None => "no such app here",
        Some(a) if a.transition.is_none() => "its manifest names no transition",
        Some(a) if !a.runnable() => "it needs a paired neochumbyd",
        Some(a) => return Some(a),
    };
    eprintln!("neochumby: transition={name} ({from}) can't be the transition: {why}");
    None
}

/// Where the screen and its overlay are: the Chumby's, or a test's.
#[derive(Clone, Debug)]
pub struct Screens {
    pub fb: PathBuf,
    pub overlay: PathBuf,
    /// The overlay's settings, one file each.
    pub control: PathBuf,
}

impl Default for Screens {
    fn default() -> Screens {
        Screens { fb: "/dev/fb0".into(), overlay: "/dev/fb1".into(), control: "/proc/driver/imxfb".into() }
    }
}

/// The overlay's settings, as its files print them, to be put back.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Setting {
    enable: String,
    alpha: String,
    key: String,
    key_en: String,
}

impl Screens {
    fn get(&self, name: &str) -> io::Result<String> {
        Ok(std::fs::read_to_string(self.control.join(name))?.trim().to_string())
    }

    /// The driver takes a number in any base (simple_strtoul), and logs
    /// each write.
    fn put(&self, name: &str, value: &str) -> io::Result<()> {
        std::fs::write(self.control.join(name), format!("{value}\n"))
    }

    fn setting(&self) -> io::Result<Setting> {
        Ok(Setting {
            enable: self.get("enable")?,
            alpha: self.get("alpha")?,
            key: self.get("key")?,
            key_en: self.get("key_en")?,
        })
    }

    /// Opaque, over everything: fb1 shows instead of fb0.
    fn cover(&self) -> io::Result<()> {
        self.put("key_en", "0")?;
        self.put("alpha", "0xff")?;
        self.put("enable", "1")
    }

    /// Covering, as [`Screens::cover`] leaves it.
    fn covering(s: &Setting) -> bool {
        s.enable == "1" && s.alpha == "0xff" && s.key_en == "0"
    }

    fn read(&self, f: &File) -> io::Result<Vec<u8>> {
        let mut px = vec![0; FRAME];
        f.read_exact_at(&mut px, 0)?;
        Ok(px)
    }

    /// Off, black, then `s`: black, as the overlay's colour key may leave
    /// it see-through or blend it at its alpha.
    fn restore(&self, overlay: &File, s: &Setting) -> io::Result<()> {
        self.put("enable", "0")?;
        overlay.write_all_at(&[0; FRAME], 0)?;
        self.put("key", &s.key)?;
        self.put("key_en", &s.key_en)?;
        self.put("alpha", &s.alpha)?;
        self.put("enable", &s.enable)
    }
}

/// The chosen transition, on a screen with an overlay.
pub struct Transitions {
    app: App,
    exe: PathBuf,
    root: PathBuf,
    screens: Screens,
    /// The overlay's settings while no transition plays.
    setting: Setting,
}

impl Transitions {
    /// `app`'s transition on `screens`, if they have an overlay. One a
    /// session that died left covering the screen is put back first.
    pub fn new(app: &App, root: &Path, screens: Screens) -> Option<Transitions> {
        let exe = app.transition.clone()?;
        let found = screens.setting().and_then(|s| {
            let overlay = OpenOptions::new().write(true).open(&screens.overlay)?;
            Ok((s, overlay))
        });
        let (mut setting, overlay) = match found {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "neochumby: no transitions: the screen has no overlay ({}, {}: {e})",
                    screens.overlay.display(),
                    screens.control.display()
                );
                return None;
            }
        };
        if Screens::covering(&setting) {
            eprintln!("neochumby: the overlay was left covering the screen; turning it off");
            setting = Setting { enable: "0".into(), alpha: "0x80".into(), key: "0x000000".into(), key_en: "1".into() };
            if let Err(e) = screens.restore(&overlay, &setting) {
                eprintln!("neochumby: the overlay: {e}");
            }
        }
        eprintln!("neochumby: transitions by {}", app.name);
        Some(Transitions { app: app.clone(), exe, root: root.to_path_buf(), screens, setting })
    }

    /// Holds the screen as it is, as `from` leaves; None, and the screen as
    /// it was, if it can't.
    pub fn cover(&self, from: &str) -> Option<Covered> {
        let cover = || -> io::Result<Covered> {
            let fb = File::open(&self.screens.fb)?;
            let overlay = OpenOptions::new().write(true).open(&self.screens.overlay)?;
            let px = self.screens.read(&fb)?;
            overlay.write_all_at(&px, 0)?;
            let covered = Covered {
                from: from.to_string(),
                px,
                fb,
                overlay,
                screens: self.screens.clone(),
                setting: self.setting.clone(),
                restored: false,
            };
            // Dropped on an error from here on: put back.
            self.screens.cover()?;
            Ok(covered)
        };
        cover().inspect_err(|e| eprintln!("neochumby: covering the screen: {e}")).ok()
    }
}

/// The screen held in the overlay. Dropped, the overlay is put back.
pub struct Covered {
    from: String,
    px: Vec<u8>,
    fb: File,
    overlay: File,
    screens: Screens,
    setting: Setting,
    restored: bool,
}

impl Covered {
    /// Plays `t` into the app `to` (opened, and drawing beneath) on a
    /// thread of its own, until it ends or the result is dropped.
    pub fn play(self, t: &Transitions, to: &str) -> Playing {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (s, d) = (stop.clone(), done.clone());
        let program = Program { app: t.app.clone(), exe: t.exe.clone(), root: t.root.clone(), to: to.to_string() };
        std::thread::spawn(move || {
            neochumby_sys::process::lower_priority(5);
            let mut covered = self;
            if let Err(e) = covered.run(&program, &s) {
                eprintln!("neochumby: transition {}: {e}", program.app.name);
            }
            drop(covered);
            d.store(true, Ordering::SeqCst);
        });
        Playing { stop, done }
    }

    fn run(&mut self, p: &Program, stop: &AtomicBool) -> io::Result<()> {
        let started = Instant::now();
        let mut settle = Settle::new(&self.px);
        let to = loop {
            neochumby::sleep_ms(LOOK_MS);
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            // A driver may turn the overlay off when an app sets
            // fb0's mode (FBIOPUT_VSCREENINFO).
            if !Screens::covering(&self.screens.setting()?) {
                self.screens.cover()?;
            }
            let px = self.screens.read(&self.fb)?;
            match settle.look(px, started.elapsed()) {
                Look::Wait => {}
                Look::Nothing => {
                    eprintln!("neochumby: transition: {} drew nothing; no animation", p.to);
                    return Ok(());
                }
                Look::Settled(px) => break px,
            }
        };
        let (mut ours_in, theirs_in) = UnixStream::pair()?;
        let (mut ours_out, theirs_out) = UnixStream::pair()?;
        let mut child = p.spawn(&self.from, theirs_in, theirs_out)?;
        ours_in.set_nonblocking(true)?;
        ours_out.set_nonblocking(true)?;
        let input = [self.px.as_slice(), to.as_slice()].concat();
        let shown = pump(&mut ours_in, &mut ours_out, &input, stop, Instant::now() + LONGEST, |frame| {
            self.overlay.write_all_at(frame, 0)
        });
        let _ = child::end(&mut child, PATIENCE);
        let shown = shown?;
        eprintln!(
            "neochumby: transition {} -> {}: {shown} frames in {} ms",
            self.from,
            p.to,
            started.elapsed().as_millis()
        );
        Ok(())
    }
}

impl Drop for Covered {
    fn drop(&mut self) {
        if !std::mem::replace(&mut self.restored, true)
            && let Err(e) = self.screens.restore(&self.overlay, &self.setting)
        {
            eprintln!("neochumby: putting the overlay back: {e}");
        }
    }
}

/// A transition playing. Dropped, it's stopped and waited for.
pub struct Playing {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl Drop for Playing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + STOP_WAIT;
        while !self.done.load(Ordering::SeqCst) && Instant::now() < deadline {
            neochumby::sleep_ms(10);
        }
    }
}

/// The transition program, for one switch.
struct Program {
    app: App,
    exe: PathBuf,
    root: PathBuf,
    to: String,
}

impl Program {
    /// Starts it with `stdin` and `stdout`, jailed as its app unless jails
    /// are off.
    fn spawn(&self, from: &str, stdin: UnixStream, stdout: UnixStream) -> io::Result<Process> {
        let env = vec![("NEOCHUMBY_FROM".to_string(), from.to_string()), ("NEOCHUMBY_TO".to_string(), self.to.clone())];
        if jail::jailed(&self.app) {
            let given = jail::Given { stdio: Some((stdin.as_raw_fd(), stdout.as_raw_fd())), ..Default::default() };
            return jail::start(&self.app, &self.exe, Role::Transition, &self.root, env, &[], given);
        }
        // Without what the session tells only the apps it means to.
        let mut cmd = Command::new(&self.exe);
        cmd.current_dir(&self.app.dir).env("NEOCHUMBY_ROOT", &self.root).env("HOME", "/tmp");
        cmd.env_remove(session::PLAYER_ENV).env_remove(WASM_ENV).env_remove(WASM_TRUSTED_ENV);
        cmd.env_remove(service::CLIENTS_ENV).env_remove(service::FRONTEND_ENV).env_remove(service::LISTENER_ENV);
        cmd.envs(env);
        cmd.stdin(Stdio::from(OwnedFd::from(stdin))).stdout(Stdio::from(OwnedFd::from(stdout))).process_group(0);
        Ok(Process::from(cmd.spawn()?))
    }
}

/// Writes `input` to the program and shows each whole frame it writes back
/// with `show`, until it closes its output, `stop` or `deadline`; how many
/// it showed. Both streams are non-blocking, so neither end can hold up the
/// other: a program may write before it has read everything.
fn pump(
    to: &mut UnixStream,
    from: &mut UnixStream,
    input: &[u8],
    stop: &AtomicBool,
    deadline: Instant,
    mut show: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<usize> {
    let mut written = 0;
    let mut frame = vec![0; FRAME];
    let (mut got, mut shown) = (0, 0);
    while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
        let mut busy = false;
        if written < input.len() {
            match to.write(&input[written..]) {
                Ok(n) => {
                    written += n;
                    busy = true;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                // It stopped reading: it has what it wanted.
                Err(_) => written = input.len(),
            }
            if written == input.len() {
                let _ = to.shutdown(std::net::Shutdown::Write);
            }
        }
        match from.read(&mut frame[got..]) {
            Ok(0) => break,
            Ok(n) => {
                got += n;
                busy = true;
                if got == FRAME {
                    show(&frame)?;
                    shown += 1;
                    got = 0;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
        if !busy {
            neochumby::sleep_ms(4);
        }
    }
    Ok(shown)
}

/// Waiting for the next app's screen: whether it has drawn, and settled.
struct Settle<'a> {
    from: &'a [u8],
    /// When it first changed, and how it looked last.
    changed: Option<Duration>,
    last: Vec<u8>,
    /// Looks in a row that found it as it was.
    still: u32,
}

#[derive(Debug, PartialEq, Eq)]
enum Look {
    Wait,
    /// It drew nothing within FIRST_DRAW.
    Nothing,
    Settled(Vec<u8>),
}

impl<'a> Settle<'a> {
    fn new(from: &'a [u8]) -> Settle<'a> {
        Settle { from, changed: None, last: Vec::new(), still: 0 }
    }

    /// The screen `px`, `at` since the app was opened.
    fn look(&mut self, px: Vec<u8>, at: Duration) -> Look {
        let Some(changed) = self.changed else {
            if px != self.from {
                self.changed = Some(at);
                self.last = px;
            } else if at >= FIRST_DRAW {
                return Look::Nothing;
            }
            return Look::Wait;
        };
        self.still = if px == self.last { self.still + 1 } else { 0 };
        if self.still >= 2 || at - changed >= SETTLE {
            return Look::Settled(px);
        }
        self.last = px;
        Look::Wait
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use neochumby::apps::{Access, Needs};

    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_next_screen_settles_or_is_taken_as_it_is() {
        let from = vec![0u8; 4];
        let (a, b) = (vec![1u8; 4], vec![2u8; 4]);
        let mut s = Settle::new(&from);
        assert_eq!(s.look(from.clone(), ms(40)), Look::Wait);
        assert_eq!(s.look(a.clone(), ms(80)), Look::Wait);
        assert_eq!(s.look(b.clone(), ms(120)), Look::Wait, "still drawing");
        assert_eq!(s.look(b.clone(), ms(160)), Look::Wait);
        assert_eq!(s.look(b.clone(), ms(200)), Look::Settled(b.clone()), "held still twice");

        let mut s = Settle::new(&from);
        assert_eq!(s.look(from.clone(), FIRST_DRAW), Look::Nothing);

        // An app that never holds still is taken SETTLE after it first drew.
        let mut s = Settle::new(&from);
        assert_eq!(s.look(a.clone(), ms(100)), Look::Wait);
        let mut at = 100;
        let mut look = Look::Wait;
        while look == Look::Wait {
            at += 40;
            look = s.look(vec![at as u8; 4], ms(at));
        }
        assert!(at >= 100 + SETTLE.as_millis() as u64 && at < 140 + SETTLE.as_millis() as u64, "{at}");
    }

    fn scratch(test: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("neochumby-transition-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("control")).unwrap();
        d
    }

    /// A screen and an overlay as plain files, the overlay set as the
    /// stock firmware leaves it.
    fn screens(dir: &Path, left: &[(&str, &str)]) -> Screens {
        std::fs::write(dir.join("fb0"), vec![0x11; FRAME]).unwrap();
        std::fs::write(dir.join("fb1"), vec![0; FRAME]).unwrap();
        for (k, v) in left {
            std::fs::write(dir.join("control").join(k), format!("{v}\n")).unwrap();
        }
        Screens { fb: dir.join("fb0"), overlay: dir.join("fb1"), control: dir.join("control") }
    }

    const STOCK: &[(&str, &str)] = &[("enable", "1"), ("alpha", "0x80"), ("key", "0x000000"), ("key_en", "1")];

    fn app(dir: &Path, script: &str) -> App {
        let exe = dir.join("wipe");
        std::fs::write(&exe, format!("#!/bin/sh\n{script}")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        App {
            dir: dir.to_path_buf(),
            name: "wipes".into(),
            title: None,
            exec: None,
            color: 0,
            service: None,
            companion: false,
            needs: Needs::Nothing,
            start: false,
            interrupt: true,
            button: false,
            player: false,
            transition: Some(exe),
            access: Access::default(),
            font: None,
            tap: Vec::new(),
            frontend: None,
            wasm: None,
            catalog: false,
        }
    }

    fn control(s: &Screens) -> Setting {
        s.setting().unwrap()
    }

    #[test]
    fn covers_plays_and_puts_the_overlay_back() {
        let dir = scratch("plays");
        let s = screens(&dir, STOCK);
        // Checks what it was given, then writes three frames of 0x22,
        // 0x33 and the picture it was given last.
        let script = format!(
            r#"test "$NEOCHUMBY_FROM $NEOCHUMBY_TO" = "home doom" || exit 1
cat > "{d}/in"
for b in 042 063; do head -c {FRAME} /dev/zero | tr '\000' "\\$b"; done
tail -c {FRAME} "{d}/in"
"#,
            d = dir.display()
        );
        let a = app(&dir, &script);
        let t = Transitions::new(&a, &dir, s.clone()).expect("an overlay");
        let covered = t.cover("home").expect("covered");
        assert_eq!(std::fs::read(dir.join("fb1")).unwrap(), vec![0x11; FRAME], "the screen, held");
        assert_eq!(control(&s).alpha, "0xff");
        assert_eq!(control(&s).key_en, "0");
        // The next app draws beneath.
        std::fs::write(dir.join("fb0"), vec![0x44; FRAME]).unwrap();
        let playing = covered.play(&t, "doom");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !playing.done.load(Ordering::SeqCst) && Instant::now() < deadline {
            neochumby::sleep_ms(20);
        }
        assert!(playing.done.load(Ordering::SeqCst));
        assert_eq!(std::fs::read(dir.join("in")).unwrap(), [vec![0x11; FRAME], vec![0x44; FRAME]].concat());
        // Put back as it was: black, and the stock settings.
        assert_eq!(std::fs::read(dir.join("fb1")).unwrap(), vec![0; FRAME]);
        assert_eq!(
            control(&s),
            Setting { enable: "1".into(), alpha: "0x80".into(), key: "0x000000".into(), key_en: "1".into() }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_program_that_hangs_is_cut_short() {
        let dir = scratch("hangs");
        let s = screens(&dir, STOCK);
        let a = app(&dir, "exec sleep 30\n");
        let t = Transitions::new(&a, &dir, s.clone()).unwrap();
        let covered = t.cover("home").unwrap();
        std::fs::write(dir.join("fb0"), vec![0x44; FRAME]).unwrap();
        let start = Instant::now();
        let playing = covered.play(&t, "doom");
        neochumby::sleep_ms(300);
        drop(playing);
        assert!(start.elapsed() < Duration::from_secs(2), "{:?}", start.elapsed());
        assert_eq!(control(&s).alpha, "0x80");
        assert_eq!(control(&s).enable, "1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn left_covering_is_put_right_and_without_an_overlay_nothing_plays() {
        let dir = scratch("left");
        let s = screens(&dir, &[("enable", "1"), ("alpha", "0xff"), ("key", "0x000000"), ("key_en", "0")]);
        let a = app(&dir, "exit 0\n");
        assert!(Transitions::new(&a, &dir, s.clone()).is_some());
        assert_eq!(control(&s).enable, "0");
        assert_eq!(control(&s).key_en, "1");

        std::fs::remove_file(dir.join("control").join("alpha")).unwrap();
        assert!(Transitions::new(&a, &dir, s).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_an_app_with_a_transition_is_chosen() {
        let dir = scratch("choose");
        let apps = vec![app(&dir, ""), App { transition: None, name: "home".into(), ..app(&dir, "") }];
        assert_eq!(choose(&apps, Some(("settings", "wipes"))).map(|a| a.name.as_str()), Some("wipes"));
        assert!(choose(&apps, Some(("settings", "home"))).is_none());
        assert!(choose(&apps, Some(("settings", "gone"))).is_none());
        assert!(choose(&apps, None).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
