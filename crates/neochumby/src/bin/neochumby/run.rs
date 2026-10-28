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
//! - over the start app from any service, but over any other app only from
//!   the start app's service, so services don't fight over the screen;
//!   CHUMBY MODE only from the start app's service;
//! - when the app in front doesn't say `interrupt = false` (games);
//! - not after the escape hatch, until an app is opened by hand (asked for
//!   by the app in front), so a service can't trap the Chumby in an app
//!   gone wrong;
//! - not once the app in front has asked for something itself: it's
//!   leaving, and what it asked for wins.
//!
//! Services are looked after in the same loop, so while the session waits
//! on an app (ending it, up to `patience`, or the pause after the start
//! app failed), services wait to be restarted and their requests to be
//! read.
//!
//! The top button belongs to the session (button.rs). A press returns to
//! the start app, with NEOCHUMBY_REASON=button, unless the app in front
//! has the button (`button = true`).
//!
//! When a service exits, the session mutes the speaker and puts back the
//! volume the Chumby booted with (neochumby::audio), so a service that dies
//! while playing can't leave it on. It can't tell whether another service
//! is playing at that moment (the library keeps no record of who holds the
//! speaker), so that one is muted too, until it next unmutes.

use std::os::fd::RawFd;
use std::path::Path;
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

use neochumby::apps::App;
use neochumby::session::{Pipe, Request};

use crate::button::Edges;
use crate::child;
use crate::services::{Backoff, Services};
use crate::start;

/// What the session needs of the Chumby itself.
pub trait Device {
    /// Whether the escape hatch was used since the last call.
    fn escaped(&mut self) -> bool;
    /// Mutes the speaker and puts back the volume the Chumby booted with.
    fn reset_speaker(&mut self);
    /// Whether the top button is down now; None if the session hasn't got
    /// it.
    fn button(&mut self) -> Option<bool>;
    /// The top button's open file, to hand to an app with `button = true`.
    fn button_fd(&self) -> Option<RawFd>;
    /// Closes the top button, for the Chumby's own software.
    fn close_button(&mut self);
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
    /// The least time between opening one app and the next, so two apps
    /// that open each other and exit at once can't spin.
    pub min_gap: Duration,
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
            min_gap: Duration::from_millis(500),
        }
    }
}

/// Five failures of the start app within this long: it can't start.
const FAILURES: usize = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// How many lines an app may write that the session doesn't know before
/// they stop being logged.
const LOGGED_UNKNOWN: u32 = 3;

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
    /// Lines it wrote that the session doesn't know.
    unknown: u32,
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
    last_open: Option<Instant>,
    button: Edges,
}

/// Whether the service of the app `from` may replace `front` in front;
/// why not, if it may not.
pub fn may_interrupt(from: &str, start: &str, front: &App, suppressed: bool) -> Result<(), &'static str> {
    if front.name != start && from != start {
        Err("only the start app's service may, over an app other than the start app")
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
        Session {
            apps,
            root,
            start,
            device,
            timing,
            failures: Vec::new(),
            last_open: None,
            services,
            suppressed: false,
            button: Edges::default(),
        }
    }

    /// Runs apps, and the services, until CHUMBY MODE.
    pub fn run(mut self) {
        let mut next = Next::Open(self.start, None);
        while let Next::Open(app, reason) = next {
            if let Some(gap) = self.last_open.and_then(|t| self.timing.min_gap.checked_sub(t.elapsed())) {
                neochumby::sleep_ms(gap.as_millis() as u32);
            }
            self.last_open = Some(Instant::now());
            next = match self.open(app, reason) {
                Ok(front) => self.watch(front),
                Err(e) => {
                    eprintln!("neochumby: {}: {e}", app.name);
                    self.failed(app)
                }
            };
        }
        eprintln!("neochumby: CHUMBY MODE");
        self.device.close_button();
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
        let mut inherit = Vec::new();
        if app.button
            && let Some(fd) = self.device.button_fd()
        {
            cmd.env(neochumby::input::BUTTON_ENV, fd.to_string());
            inherit.push(fd);
        }
        let (child, pipe) = child::spawn(cmd, &inherit)?;
        self.services.front(&app.name);
        Ok(Front { app, child, pipe, asked: None, unknown: 0 })
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
            if f.app.button {
                self.button.forget();
            } else if let Some(down) = self.device.button()
                && self.button.press(down)
            {
                eprintln!("neochumby: the top button: ending {}, opening {}", f.app.name, self.start.name);
                child::end(&mut f.child, self.timing.patience);
                // A person chose this.
                self.suppressed = false;
                return Next::Open(self.start, Some("button"));
            }
            self.read(&mut f);
            let ev = self.services.poll(&f.app.name);
            if !ev.died.is_empty() {
                self.device.reset_speaker();
            }
            for (from, req) in ev.requests {
                if f.asked.is_some() {
                    eprintln!(
                        "neochumby: service {} asks: {}; dropped: {} has asked already",
                        from.name,
                        req.line().trim_end(),
                        f.app.name
                    );
                    continue;
                }
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
                Ok(Some(s)) => {
                    // Anything it started goes with it.
                    child::end(&mut f.child, self.timing.patience);
                    Ok(s)
                }
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
                    f.unknown += 1;
                    if f.unknown <= LOGGED_UNKNOWN {
                        eprintln!("neochumby: {}: ignoring {line:?}", f.app.name);
                    }
                    if f.unknown == LOGGED_UNKNOWN {
                        eprintln!("neochumby: {}: not logging more lines it doesn't know", f.app.name);
                    }
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
            Request::ChumbyMode if from.name != self.start.name => {
                eprintln!(
                    "neochumby: service {} asks for CHUMBY MODE; refused: only the start app's service may",
                    from.name
                );
                return None;
            }
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
        /// The button's level, and a file standing in for it.
        down: Arc<AtomicBool>,
        switch: Option<Arc<std::fs::File>>,
        closed: Arc<AtomicBool>,
    }

    impl Device for Fake {
        fn escaped(&mut self) -> bool {
            self.escape.swap(false, Ordering::SeqCst)
        }

        fn reset_speaker(&mut self) {
            self.resets.fetch_add(1, Ordering::SeqCst);
        }

        fn button(&mut self) -> Option<bool> {
            self.switch.as_ref().map(|_| self.down.load(Ordering::SeqCst))
        }

        fn button_fd(&self) -> Option<RawFd> {
            use std::os::fd::AsRawFd;
            self.switch.as_ref().map(|f| f.as_raw_fd())
        }

        fn close_button(&mut self) {
            self.switch = None;
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    /// A Fake with a button: /dev/null, close-on-exec, standing in for it.
    fn with_button() -> Fake {
        let f = std::fs::File::open("/dev/null").unwrap();
        neochumby_sys::fd::set_cloexec(std::os::fd::AsFd::as_fd(&f), true).unwrap();
        Fake { switch: Some(Arc::new(f)), ..Fake::default() }
    }

    /// Waits for a line starting with `start` in the log.
    fn wait_for(root: &Path, start: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !log(root).iter().any(|l| l.starts_with(start)) {
            assert!(Instant::now() < deadline, "no {start:?} in the log");
            neochumby::sleep_ms(10);
        }
    }

    /// Presses the button for 200 ms.
    fn press(down: &AtomicBool) {
        down.store(true, Ordering::SeqCst);
        neochumby::sleep_ms(200);
        down.store(false, Ordering::SeqCst);
    }

    fn timing() -> Timing {
        Timing {
            tick_ms: 10,
            grace: Duration::from_millis(300),
            patience: Duration::from_secs(1),
            retry: Duration::from_millis(10),
            service_backoff: Backoff::new(Duration::from_millis(50), Duration::from_millis(200)),
            service_patience: Duration::from_secs(1),
            min_gap: Duration::from_millis(20),
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
        run_timed(root, choices, fake, timing());
    }

    fn run_timed(root: &Path, choices: &[(&str, &str)], fake: Fake, timing: Timing) {
        let all = apps::all(root);
        let start = start::choose(&all, choices.first().copied()).expect("a start app");
        Session::new(&all, root, start, fake, timing).run();
    }

    /// Waits for the process whose pid is in `pid_file` to be gone.
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
    fn what_an_app_started_goes_when_it_exits() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let pid = "\"$NEOCHUMBY_ROOT/pid\"";
        let root = tree(
            "group",
            &[
                ("home", "start=true\n", "[ $runs = 1 ] || { ask chumby-mode; exit 0; }\nask 'open game'"),
                // A helper left behind as it exits.
                ("game", "", &format!("sleep 30 &\necho $! > {pid}.new\nmv {pid}.new {pid}")),
            ],
        );
        run(&root, &[], Arc::default());
        assert_eq!(log(&root), ["home", "game", "home"]);
        gone(&root.join("pid"));
    }

    #[test]
    fn apps_opening_each_other_cant_spin() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let stop = "\"$NEOCHUMBY_ROOT/stop\"";
        let root = tree(
            "spin",
            &[
                ("home", "start=true\n", &format!("[ -f {stop} ] && {{ ask chumby-mode; exit 0; }}\nask 'open game'")),
                ("game", "", "exit 0"),
            ],
        );
        let stop_file = root.join("stop");
        std::thread::spawn(move || {
            neochumby::sleep_ms(1000);
            std::fs::write(stop_file, "").unwrap();
        });
        run_timed(&root, &[], Fake::default(), Timing { min_gap: Duration::from_millis(200), ..timing() });
        // About one open every 200 ms for a second.
        let games = log(&root).iter().filter(|l| *l == "game").count();
        assert!((1..=4).contains(&games), "{games} games");
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
    fn a_press_returns_to_the_start_app() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tree(
            "button",
            &[
                ("wx", "start=true\n", "[ $runs = 1 ] || { ask chumby-mode; exit 0; }\nask 'open game'"),
                ("game", "", "exec sleep 30"),
            ],
        );
        let fake = with_button();
        // Held as the session starts: not a press.
        fake.down.store(true, Ordering::SeqCst);
        let (down, closed, r) = (fake.down.clone(), fake.closed.clone(), root.clone());
        std::thread::spawn(move || {
            neochumby::sleep_ms(100);
            down.store(false, Ordering::SeqCst);
            wait_for(&r, "game");
            press(&down);
        });
        run_on(&root, &[("settings", "wx")], fake);
        assert_eq!(log(&root), ["wx", "game", "wx button"]);
        assert!(closed.load(Ordering::SeqCst), "the button wasn't closed at CHUMBY MODE");
    }

    #[test]
    fn an_app_with_button_true_gets_the_button() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let fake = with_button();
        let fd = fake.button_fd().unwrap();
        // Logs whether it was handed the button, and whether the file is
        // open in it.
        let check = format!(
            "if [ -e /dev/fd/{fd} ]; then o=open; else o=closed; fi\n\
             echo \"$(basename $0) button=$NEOCHUMBY_BUTTON $o\" >> \"$log\""
        );
        let root = tree(
            "handover",
            &[
                (
                    "wx",
                    "start=true\n",
                    &format!("{check}\n[ $runs = 1 ] || {{ ask chumby-mode; exit 0; }}\nask 'open game'"),
                ),
                ("game", "button=true\n", &format!("{check}\nsleep 1\necho 'game done' >> \"$log\"")),
            ],
        );
        let (down, r) = (fake.down.clone(), root.clone());
        std::thread::spawn(move || {
            wait_for(&r, "game");
            press(&down);
        });
        run_on(&root, &[("settings", "wx")], fake);
        assert_eq!(
            log(&root),
            [
                "wx".to_string(),
                "wx button= closed".into(),
                "game".into(),
                format!("game button={fd} open"),
                "game done".into(),
                "wx".into(),
                "wx button= closed".into(),
            ]
        );
    }

    #[test]
    fn rules_for_services() {
        let mut game = start::tests::app("doom", false);
        let wx = start::tests::app("wx", true);
        // Over the start app: any service. Over another: the start app's.
        assert_eq!(may_interrupt("wx", "wx", &wx, false), Ok(()));
        assert_eq!(may_interrupt("alarm", "wx", &wx, false), Ok(()));
        assert_eq!(may_interrupt("wx", "wx", &game, false), Ok(()));
        assert!(may_interrupt("alarm", "wx", &game, false).is_err());
        // Not after the escape hatch, nor over interrupt = false.
        assert!(may_interrupt("wx", "wx", &game, true).is_err());
        assert!(may_interrupt("alarm", "wx", &wx, true).is_err());
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
    fn any_service_may_open_over_the_start_app_only() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let once = "\"$NEOCHUMBY_ROOT/once\"";
        let root = tree_with(
            "anyservice",
            &[
                ("wx", "start=true\n", "[ $runs = 1 ] || { ask chumby-mode; exit 0; }\nexec sleep 30"),
                // Opened over wx by its own service; opens the game by hand.
                ("alarm", "", "ask 'open game'"),
                ("game", "", "sleep 1\necho 'game done' >> \"$log\""),
            ],
            &[(
                "alarm",
                &listen(&format!(
                    "    'front wx') [ -f {once} ] || {{ touch {once}; ask 'open alarm'; }} ;;\n    \
                     'front game') ask 'open alarm' ;;"
                )),
            )],
        );
        run(&root, &[("settings", "wx")], Arc::default());
        // Not over the game: alarm isn't the start app's.
        assert_eq!(apps_log(&root), ["wx", "alarm", "game", "game done", "wx"]);
    }

    #[test]
    fn what_the_front_app_asked_wins() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let root = tree_with(
            "frontwins",
            &[
                // Asks for CHUMBY MODE, then takes its time to exit.
                ("wx", "start=true\n", "ask chumby-mode\necho 'wx asked' >> \"$log\"\nexec sleep 30"),
                ("game", "", "exit 0"),
            ],
            &[(
                "wx",
                &listen(
                    "    'front wx') [ -f \"$NEOCHUMBY_ROOT/once\" ] && continue; touch \"$NEOCHUMBY_ROOT/once\"\n      \
                     until grep -q '^wx asked' \"$log\"; do sleep 0.02; done; ask 'open game' ;;",
                ),
            )],
        );
        run(&root, &[("settings", "wx")], Arc::default());
        assert_eq!(apps_log(&root), ["wx", "wx asked"]);
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
