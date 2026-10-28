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

use std::path::Path;
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

use neochumby::apps::App;
use neochumby::session::{Pipe, Request};

use crate::child;
use crate::start;

/// What the session needs of the Chumby itself.
pub trait Device {
    /// Whether the escape hatch was used since the last call.
    fn escaped(&mut self) -> bool;
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
    last_open: Option<Instant>,
}

impl<'a, D: Device> Session<'a, D> {
    pub fn new(apps: &'a [App], root: &'a Path, start: &'a App, device: D, timing: Timing) -> Session<'a, D> {
        Session { apps, root, start, device, timing, failures: Vec::new(), last_open: None }
    }

    /// Runs apps until CHUMBY MODE.
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
    }

    fn open(&self, app: &'a App, reason: Option<&'static str>) -> std::io::Result<Front<'a>> {
        let exe = app.exec.as_ref().ok_or_else(|| std::io::Error::other("no exec"))?;
        let mut cmd = child::command(exe, &app.dir, self.root);
        if let Some(r) = reason {
            cmd.env(neochumby::session::REASON_ENV, r);
        }
        eprintln!("neochumby: opening {}{}", app.name, reason.map(|r| format!(" ({r})")).unwrap_or_default());
        let (child, pipe) = child::spawn(cmd, &[])?;
        Ok(Front { app, child, pipe, asked: None, unknown: 0 })
    }

    /// Watches the app in front until it has gone; what to do next.
    fn watch(&mut self, mut f: Front<'a>) -> Next<'a> {
        loop {
            neochumby::sleep_ms(self.timing.tick_ms);
            if self.device.escaped() {
                eprintln!("neochumby: escape: ending {} and what it started", f.app.name);
                child::end(&mut f.child, self.timing.patience);
                return Next::Open(start::home(self.apps).unwrap_or(self.start), None);
            }
            self.read(&mut f);
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
    fn read(&self, f: &mut Front<'a>) {
        for line in f.pipe.receive().lines {
            let next = match Request::parse(&line) {
                Some(Request::ChumbyMode) => Next::ChumbyMode,
                Some(Request::Open(name)) => match self.openable(&name) {
                    Ok(app) => Next::Open(app, None),
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
    //! The session driven for real, with shell scripts as apps. Each writes
    //! a line to `log` in the tree as it starts, and its requests to the
    //! pipe as an app would.

    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use neochumby::apps;

    use super::*;

    /// One test at a time: a script being written while another test forks
    /// can fail to run ("text file busy").
    static ONE: Mutex<()> = Mutex::new(());

    struct Fake {
        escape: Arc<AtomicBool>,
    }

    impl Device for Fake {
        fn escaped(&mut self) -> bool {
            self.escape.swap(false, Ordering::SeqCst)
        }
    }

    fn timing() -> Timing {
        Timing {
            tick_ms: 10,
            grace: Duration::from_millis(300),
            patience: Duration::from_secs(1),
            retry: Duration::from_millis(10),
            min_gap: Duration::from_millis(20),
        }
    }

    /// A tree of apps: (name, manifest lines after exec, script body).
    fn tree(test: &str, apps: &[(&str, &str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("neochumby-session-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, manifest, body) in apps {
            let dir = root.join("apps").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("manifest"), format!("exec={name}\n{manifest}")).unwrap();
            // $runs: how many times this app has started, this one included.
            let script = format!(
                "#!/bin/sh\nlog=\"$NEOCHUMBY_ROOT/log\"\necho \"{name} $NEOCHUMBY_REASON\" >> \"$log\"\n\
                 runs=$(grep -c '^{name} ' \"$log\")\nask() {{ echo \"$1\" >&\"$NEOCHUMBY_SESSION\"; }}\n{body}\n"
            );
            std::fs::write(dir.join(name), script).unwrap();
            std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(root.join("log"), "").unwrap();
        root
    }

    fn log(root: &Path) -> Vec<String> {
        std::fs::read_to_string(root.join("log")).unwrap().lines().map(|l| l.trim_end().to_string()).collect()
    }

    fn run(root: &Path, choices: &[(&str, &str)], escape: Arc<AtomicBool>) {
        run_timed(root, choices, escape, timing());
    }

    fn run_timed(root: &Path, choices: &[(&str, &str)], escape: Arc<AtomicBool>, timing: Timing) {
        let all = apps::all(root);
        let start = start::choose(&all, choices.first().copied()).expect("a start app");
        Session::new(&all, root, start, Fake { escape }, timing).run();
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
        run_timed(&root, &[], Arc::default(), Timing { min_gap: Duration::from_millis(200), ..timing() });
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
}
