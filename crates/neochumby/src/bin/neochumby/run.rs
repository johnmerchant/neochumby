//! The session's loop: one app in front at a time, full-screen.
//!
//! It opens the start app (start.rs). When the app in front asks, over its
//! request pipe, for another app or for CHUMBY MODE, and exits, the session
//! does that next. When it exits without asking, the session goes back to
//! the start app. The start app is meant to keep running: if it exits
//! without asking five times within a minute, `home` becomes the start app
//! instead, and if that's `home` already, the Chumby goes back to its own
//! software.
//!
//! The escape hatch (a touch held in the top-left corner) ends whatever is
//! in front, with its process group, and opens `home`, even if the start
//! app is another one.
//!
//! The apps' services run alongside (services.rs). A service's request ends
//! the app in front first, so the session takes it only:
//! - from the start app's service, so services don't fight over the
//!   screen;
//! - when the app in front doesn't say `interrupt = false` (games);
//! - not after the escape hatch, until an app is opened by hand (asked for
//!   by the app in front), so a service can't trap the Chumby in an app
//!   gone wrong.
//!
//! When a service exits, the session mutes the speaker and puts back the
//! volume the Chumby booted with (neochumby::audio), so a service that dies
//! while playing can't leave it on. It can't tell whether another service
//! is playing at that moment (the library keeps no record of who holds the
//! speaker), so that one is muted too, until it next unmutes.

use std::path::Path;
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

use neochumby::apps::App;
use neochumby::session::{Pipe, Request};

use crate::child;
use crate::services::{Backoff, Services};
use crate::start;

/// What the session needs of the Chumby itself.
pub trait Device {
    /// Whether the escape hatch was used since the last call.
    fn escaped(&mut self) -> bool;
    /// Mutes the speaker and puts back the volume the Chumby booted with.
    fn reset_speaker(&mut self);
}

pub struct Timing {
    /// How often the session looks at what it runs.
    pub tick_ms: u32,
    /// How long an app that asked for something has to exit.
    pub grace: Duration,
    /// Between SIGTERM and SIGKILL, when the session ends an app.
    pub patience: Duration,
    /// The pause before starting the start app again after it failed.
    pub retry: Duration,
    /// The waits before starting a service again (services.rs).
    pub service_backoff: Backoff,
    /// Between SIGTERM and SIGKILL, when the session stops the services.
    pub service_patience: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            tick_ms: 50,
            grace: Duration::from_secs(5),
            patience: Duration::from_secs(2),
            retry: Duration::from_secs(1),
            service_backoff: Backoff::new(Duration::from_secs(1), Duration::from_secs(60)),
            service_patience: Duration::from_secs(5),
        }
    }
}

/// Five failures of the start app within this long: it can't start.
const FAILURES: usize = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);

/// What the session does next.
#[derive(Clone, Copy, Debug)]
enum Next<'a> {
    /// Opens the app, telling it why (NEOCHUMBY_REASON) if there's a reason.
    Open(&'a App, Option<&'static str>),
    ChumbyMode,
}

/// The app in front.
struct Front<'a> {
    app: &'a App,
    child: Child,
    pipe: Pipe,
    /// What it asked for, and when it first asked.
    asked: Option<(Next<'a>, Instant)>,
}

pub struct Session<'a, D: Device> {
    apps: &'a [App],
    root: &'a Path,
    start: &'a App,
    device: D,
    timing: Timing,
    failures: Vec<Instant>,
    services: Services<'a>,
    /// Since the escape hatch: services' requests are refused until an app
    /// is opened by hand.
    suppressed: bool,
}

/// Whether the service of the app `from` may replace `front` in front;
/// why not, if it may not.
pub fn may_interrupt(from: &str, start: &str, front: &App, suppressed: bool) -> Result<(), &'static str> {
    if from != start {
        Err("only the start app's service may")
    } else if !front.interrupt {
        Err("the app in front says interrupt = false")
    } else if suppressed {
        Err("not since the escape hatch, until an app is opened by hand")
    } else {
        Ok(())
    }
}

impl<'a, D: Device> Session<'a, D> {
    pub fn new(apps: &'a [App], root: &'a Path, start: &'a App, device: D, timing: Timing) -> Session<'a, D> {
        let services = Services::new(apps, root, timing.service_backoff);
        Session { apps, root, start, device, timing, failures: Vec::new(), services, suppressed: false }
    }

    /// Runs apps, and the services, until CHUMBY MODE.
    pub fn run(mut self) {
        let mut next = Next::Open(self.start, None);
        while let Next::Open(app, reason) = next {
            next = match self.open(app, reason) {
                Ok(front) => self.watch(front),
                Err(e) => {
                    eprintln!("neochumby: {}: {e}", app.name);
                    self.failed(app)
                }
            };
        }
        eprintln!("neochumby: CHUMBY MODE");
        self.services.stop(self.timing.service_patience);
        self.device.reset_speaker();
    }

    fn open(&mut self, app: &'a App, reason: Option<&'static str>) -> std::io::Result<Front<'a>> {
        let exe = app.exec.as_ref().ok_or_else(|| std::io::Error::other("no exec"))?;
        let mut cmd = child::command(exe, &app.dir, self.root);
        if let Some(r) = reason {
            cmd.env(neochumby::session::REASON_ENV, r);
        }
        eprintln!("neochumby: opening {}{}", app.name, reason.map(|r| format!(" ({r})")).unwrap_or_default());
        let (child, pipe) = child::spawn(cmd, &[])?;
        self.services.front(&app.name);
        Ok(Front { app, child, pipe, asked: None })
    }

    /// Watches the app in front until it has gone; what to do next.
    fn watch(&mut self, mut f: Front<'a>) -> Next<'a> {
        loop {
            neochumby::sleep_ms(self.timing.tick_ms);
            if self.device.escaped() {
                eprintln!("neochumby: escape: ending {} and what it started", f.app.name);
                child::end(&mut f.child, self.timing.patience);
                self.suppressed = true;
                return Next::Open(start::home(self.apps).unwrap_or(self.start), None);
            }
            self.read(&mut f);
            let ev = self.services.poll(&f.app.name);
            if !ev.died.is_empty() {
                self.device.reset_speaker();
            }
            for (from, req) in ev.requests {
                if let Some(next) = self.interrupting(&f, from, req) {
                    eprintln!("neochumby: service {} replaces {}", from.name, f.app.name);
                    child::end(&mut f.child, self.timing.patience);
                    return next;
                }
            }
            let status = match f.child.try_wait() {
                Ok(None) => match f.asked {
                    Some((_, at)) if at.elapsed() >= self.timing.grace => {
                        eprintln!("neochumby: {} asked, but hasn't exited; ending it", f.app.name);
                        child::end(&mut f.child, self.timing.patience);
                        continue;
                    }
                    _ => continue,
                },
                Ok(Some(s)) => Ok(s),
                Err(e) => {
                    child::end(&mut f.child, self.timing.patience);
                    Err(e)
                }
            };
            // What it wrote just before it exited.
            self.read(&mut f);
            return self.ended(f, status);
        }
    }

    /// Takes in what the app in front has asked for.
    fn read(&mut self, f: &mut Front<'a>) {
        for line in f.pipe.receive().lines {
            let next = match Request::parse(&line) {
                Some(Request::ChumbyMode) => Next::ChumbyMode,
                Some(Request::Open(name)) => match self.openable(&name) {
                    Ok(app) => {
                        // Opened by hand.
                        self.suppressed = false;
                        Next::Open(app, None)
                    }
                    Err(why) => {
                        eprintln!("neochumby: {} asks to open {name}: {why}", f.app.name);
                        continue;
                    }
                },
                None => {
                    eprintln!("neochumby: {}: ignoring {line:?}", f.app.name);
                    continue;
                }
            };
            eprintln!("neochumby: {} asks: {line}", f.app.name);
            let at = f.asked.map_or_else(Instant::now, |(_, at)| at);
            f.asked = Some((next, at));
        }
    }

    /// What a service's request makes the session do next, ending the app in
    /// front, if it may.
    fn interrupting(&self, f: &Front<'a>, from: &App, req: Request) -> Option<Next<'a>> {
        let next = match &req {
            Request::ChumbyMode => Next::ChumbyMode,
            Request::Open(name) if *name == f.app.name => return None,
            Request::Open(name) => match self.openable(name) {
                Ok(app) => Next::Open(app, None),
                Err(why) => {
                    eprintln!("neochumby: service {} asks to open {name}: {why}", from.name);
                    return None;
                }
            },
        };
        match may_interrupt(&from.name, &self.start.name, f.app, self.suppressed) {
            Ok(()) => Some(next),
            Err(why) => {
                eprintln!("neochumby: service {} asks: {}; refused: {why}", from.name, req.line().trim_end());
                None
            }
        }
    }

    /// An app that can be opened: in the tree, with an `exec`, and runnable.
    fn openable(&self, name: &str) -> Result<&'a App, &'static str> {
        let app = self.apps.iter().find(|a| a.name == name).ok_or("no such app here")?;
        if app.exec.is_none() {
            Err("it has nothing to open (no exec)")
        } else if !app.runnable() {
            Err("it needs a paired neochumbyd")
        } else {
            Ok(app)
        }
    }

    /// What to do once the app in front has exited.
    fn ended(&mut self, f: Front<'a>, status: std::io::Result<ExitStatus>) -> Next<'a> {
        if let Some((next, _)) = f.asked {
            return next;
        }
        eprintln!("neochumby: {} ended: {status:?}", f.app.name);
        if f.app.name == self.start.name {
            return self.failed(f.app);
        }
        Next::Open(self.start, None)
    }

    /// `app` failed to start, or exited without asking for anything. When
    /// that's the start app, it counts towards falling back.
    fn failed(&mut self, app: &'a App) -> Next<'a> {
        if app.name != self.start.name {
            return Next::Open(self.start, None);
        }
        self.failures.retain(|t| t.elapsed() < FAILURE_WINDOW);
        self.failures.push(Instant::now());
        if self.failures.len() >= FAILURES {
            match start::home(self.apps) {
                Some(home) if home.name != self.start.name => {
                    eprintln!("neochumby: {} keeps failing; {} is the start app instead", self.start.name, home.name);
                    self.start = home;
                    self.failures.clear();
                }
                _ => {
                    eprintln!("neochumby: {} keeps failing; back to the Chumby", self.start.name);
                    return Next::ChumbyMode;
                }
            }
        }
        neochumby::sleep_ms(self.timing.retry.as_millis() as u32);
        Next::Open(self.start, None)
    }
}

#[cfg(test)]
mod tests {
    //! The session driven for real, with shell scripts as apps and
    //! services. Each writes a line to `log` in the tree as it starts, and
    //! its requests to the pipe as an app would.

    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use neochumby::apps;

    use super::*;

    /// One test at a time: a script being written while another test forks
    /// can fail to run ("text file busy").
    static ONE: Mutex<()> = Mutex::new(());

    #[derive(Clone, Default)]
    struct Fake {
        escape: Arc<AtomicBool>,
        resets: Arc<AtomicUsize>,
    }

    impl Device for Fake {
        fn escaped(&mut self) -> bool {
            self.escape.swap(false, Ordering::SeqCst)
        }

        fn reset_speaker(&mut self) {
            self.resets.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn timing() -> Timing {
        Timing {
            tick_ms: 10,
            grace: Duration::from_millis(300),
            patience: Duration::from_secs(1),
            retry: Duration::from_millis(10),
            service_backoff: Backoff::new(Duration::from_millis(50), Duration::from_millis(200)),
            service_patience: Duration::from_secs(1),
        }
    }

    /// Writes an executable script that logs `{tag} $NEOCHUMBY_REASON` as it
    /// starts, then runs `body` with $runs (how many times `tag` has
    /// started, this one included) and `ask` (writes a line to the pipe).
    fn script(path: &Path, tag: &str, body: &str) {
        let script = format!(
            "#!/bin/sh\nlog=\"$NEOCHUMBY_ROOT/log\"\necho \"{tag} $NEOCHUMBY_REASON\" >> \"$log\"\n\
             runs=$(grep -c '^{tag} ' \"$log\")\nask() {{ echo \"$1\" >&\"$NEOCHUMBY_SESSION\"; }}\n{body}\n"
        );
        std::fs::write(path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A tree of apps: (name, manifest lines after exec, script body).
    fn tree(test: &str, apps: &[(&str, &str, &str)]) -> PathBuf {
        tree_with(test, apps, &[])
    }

    /// A tree of apps, and services: (app, script body), logged as
    /// `<app>-svc`.
    fn tree_with(test: &str, apps: &[(&str, &str, &str)], services: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("neochumby-session-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, manifest, body) in apps {
            let dir = root.join("apps").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let mut manifest = format!("exec={name}\n{manifest}");
            if let Some((_, body)) = services.iter().find(|(app, _)| app == name) {
                manifest += "service=svc\n";
                script(&dir.join("svc"), &format!("{name}-svc"), body);
            }
            std::fs::write(dir.join("manifest"), manifest).unwrap();
            script(&dir.join(name), name, body);
        }
        std::fs::write(root.join("log"), "").unwrap();
        root
    }

    fn log(root: &Path) -> Vec<String> {
        std::fs::read_to_string(root.join("log")).unwrap().lines().map(|l| l.trim_end().to_string()).collect()
    }

    /// The log's lines from apps, leaving out the services'.
    fn apps_log(root: &Path) -> Vec<String> {
        log(root).into_iter().filter(|l| !l.contains("-svc") && !l.starts_with("svc-heard")).collect()
    }

    fn run(root: &Path, choices: &[(&str, &str)], escape: Arc<AtomicBool>) {
        run_on(root, choices, Fake { escape, ..Fake::default() });
    }

    fn run_on(root: &Path, choices: &[(&str, &str)], fake: Fake) {
        let all = apps::all(root);
        let start = start::choose(&all, choices).expect("a start app");
        Session::new(&all, root, start, fake, timing()).run();
    }

    /// Waits for `pid` to be gone.
    fn gone(pid_file: &Path) {
        let pid = std::fs::read_to_string(pid_file).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let running = || std::process::Command::new("kill").args(["-0", pid.trim()]).status().unwrap().success();
        while running() {
            assert!(Instant::now() < deadline, "{} still running", pid_file.display());
            neochumby::sleep_ms(20);
        }
    }

    /// A service's loop: for each notice, logs it, then runs `cases` (a
    /// shell `case` body on $line).
    fn listen(cases: &str) -> String {
        format!(
            "while read -r line <&\"$NEOCHUMBY_SESSION\"; do\n  echo \"svc-heard $line\" >> \"$log\"\n  \
             case \"$line\" in\n{cases}\n  esac\ndone"
        )
    }

    #[test]
    fn apps_open_apps_and_the_start_app_comes_back() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tree(
            "open",
            &[
                (
                    "home",
                    "start=true\n",
                    "[ $runs = 1 ] || { ask chumby-mode; exit 0; }\nprintf 'hello\\nopen nosuch\\nopen Bad\\n' >&\"$NEOCHUMBY_SESSION\"\nask 'open game'",
                ),
                // Exits without asking: back to the start app.
                ("game", "title=GAME\n", "exit 3"),
                // Can't be the start app: no start=true.
                ("wx", "", "ask chumby-mode"),
            ],
        );
        run(&root, &[("settings", "wx")], Arc::default());
        assert_eq!(log(&root), ["home", "game", "home"]);
    }

    #[test]
    fn a_failing_start_app_falls_back_to_home() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tree("fail", &[("home", "start=true\n", "ask chumby-mode"), ("wx", "start=true\n", "exit 1")]);
        run(&root, &[("settings", "wx")], Arc::default());
        assert_eq!(log(&root), ["wx", "wx", "wx", "wx", "wx", "home"]);
    }

    #[test]
    fn home_failing_hands_back_to_the_chumby() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tree("homefail", &[("home", "start=true\n", "exit 0")]);
        run(&root, &[], Arc::default());
        assert_eq!(log(&root).len(), FAILURES);
    }

    #[test]
    fn an_app_that_asks_and_stays_is_ended() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tree(
            "grace",
            &[
                (
                    "home",
                    "start=true\n",
                    "[ $runs = 1 ] || { ask chumby-mode; exit 0; }\nask 'open game'\nexec sleep 30",
                ),
                ("game", "", "exit 0"),
            ],
        );
        let t = Instant::now();
        run(&root, &[], Arc::default());
        assert_eq!(log(&root), ["home", "game", "home"]);
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_escape_hatch_ends_the_front_app_and_opens_home() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tree(
            "escape",
            &[
                ("home", "start=true\n", "ask chumby-mode"),
                // It and what it started: both should go.
                ("wx", "start=true\n", "sleep 30 &\necho $! > \"$NEOCHUMBY_ROOT/pid\"\nwait"),
            ],
        );
        let escape = Arc::new(AtomicBool::new(false));
        let flag = escape.clone();
        let pid_file = root.join("pid");
        std::thread::spawn(move || {
            while !pid_file.exists() {
                neochumby::sleep_ms(10);
            }
            flag.store(true, Ordering::SeqCst);
        });
        run(&root, &[("conf", "wx")], escape);
        assert_eq!(log(&root), ["wx", "home"]);
        gone(&root.join("pid"));
    }

    #[test]
    fn rules_for_services() {
        let mut game = start::tests::app("doom", false);
        let wx = start::tests::app("wx", true);
        assert_eq!(may_interrupt("wx", "wx", &game, false), Ok(()));
        assert_eq!(may_interrupt("wx", "wx", &wx, false), Ok(()));
        assert!(may_interrupt("airplay", "wx", &game, false).is_err());
        assert!(may_interrupt("wx", "wx", &game, true).is_err());
        game.interrupt = false;
        assert!(may_interrupt("wx", "wx", &game, false).is_err());
    }

    #[test]
    fn the_start_apps_service_opens_apps_over_others() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tree_with(
            "service",
            &[
                ("wx", "start=true\n", "[ $runs = 1 ] || { ask chumby-mode; exit 0; }\nexec sleep 30"),
                ("game", "", "exec sleep 30"),
            ],
            &[(
                "wx",
                &listen(
                    "    'front wx') [ -f \"$NEOCHUMBY_ROOT/once\" ] || { touch \"$NEOCHUMBY_ROOT/once\"; ask 'open game'; } ;;\n    \
                     'front game') until grep -q '^game' \"$log\"; do sleep 0.05; done; ask 'open wx' ;;",
                ),
            )],
        );
        run(&root, &[("settings", "wx")], Arc::default());
        assert_eq!(apps_log(&root), ["wx", "game", "wx"]);
        let heard: Vec<String> = log(&root).into_iter().filter(|l| l.starts_with("svc-heard")).collect();
        assert_eq!(heard[..2], ["svc-heard front wx", "svc-heard front game"]);
    }

    #[test]
    fn other_services_and_games_are_not_interrupted() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tree_with(
            "nointerrupt",
            &[
                ("wx", "start=true\n", "[ $runs = 1 ] || { ask chumby-mode; exit 0; }\nask 'open game'"),
                ("game", "interrupt=false\n", "sleep 1\necho 'game done' >> \"$log\""),
                ("radio", "", "exit 0"),
            ],
            &[
                // On the game in front: refused, as it says interrupt=false.
                ("wx", &listen("    'front game') ask 'open wx' ;;")),
                // Refused: not the start app's service.
                ("radio", &listen("    'front game') ask 'open radio' ;;\n    'front wx') ask chumby-mode ;;")),
            ],
        );
        run(&root, &[("settings", "wx")], Arc::default());
        assert_eq!(apps_log(&root), ["wx", "game", "game done", "wx"]);
    }

    #[test]
    fn after_the_escape_hatch_services_wait_for_a_hand() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tree_with(
            "suppressed",
            &[
                (
                    "wx",
                    "start=true\n",
                    "[ $runs = 1 ] || { ask chumby-mode; exit 0; }\necho $$ > \"$NEOCHUMBY_ROOT/pid\"\nexec sleep 30",
                ),
                // Opened by the escape hatch: wx's service can't replace it,
                // until it opens the game by hand.
                ("home", "start=true\n", "sleep 1\necho 'home done' >> \"$log\"\nask 'open game'"),
                ("game", "", "exec sleep 30"),
            ],
            &[(
                "wx",
                &listen(
                    "    'front home') ask 'open wx' ;;\n    \
                     'front game') until grep -q '^game' \"$log\"; do sleep 0.05; done; ask 'open wx' ;;",
                ),
            )],
        );
        let fake = Fake::default();
        let flag = fake.escape.clone();
        let pid_file = root.join("pid");
        std::thread::spawn(move || {
            while !pid_file.exists() {
                neochumby::sleep_ms(10);
            }
            flag.store(true, Ordering::SeqCst);
        });
        run_on(&root, &[("settings", "wx")], fake);
        assert_eq!(apps_log(&root), ["wx", "home", "home done", "game", "wx"]);
    }

    #[test]
    fn services_restart_and_the_speaker_is_reset() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tree_with("restart", &[("wx", "start=true\n", "sleep 1\nask chumby-mode")], &[("wx", "exit 1")]);
        let fake = Fake::default();
        let resets = fake.resets.clone();
        run_on(&root, &[("settings", "wx")], fake);
        // Waits of 50, 100, 200, 200 ... ms within the second wx runs.
        let runs = log(&root).iter().filter(|l| l.starts_with("wx-svc")).count();
        assert!((4..=9).contains(&runs), "{runs} runs");
        // One for each exit, and one as the session ends.
        assert!(resets.load(Ordering::SeqCst) >= runs, "{runs} runs, {resets:?} resets");
    }

    #[test]
    fn services_stop_with_the_session() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let pid = "\"$NEOCHUMBY_ROOT/pid\"";
        let root = tree_with(
            "stop",
            &[("wx", "start=true\n", &format!("while [ ! -f {pid} ]; do sleep 0.05; done\nask chumby-mode"))],
            // It and what it started.
            &[("wx", &format!("sleep 30 &\necho $! > {pid}.new\nmv {pid}.new {pid}\nwait"))],
        );
        run(&root, &[("settings", "wx")], Arc::default());
        gone(&root.join("pid"));
    }
}
