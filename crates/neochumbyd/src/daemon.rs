//! Apps' daemons: the server half an app may have that runs once per
//! server rather than once per Chumby, named in its manifest
//! (`daemon=<file in the app's directory>`). The household web page is
//! one. neochumbyd starts each daemon in the tree when it starts, as its
//! own process, with
//!
//!   NEOCHUMBY_STATE    the app's own directory (<state>/apps/<app>), kept
//!                      across runs and shared with its companions; also
//!                      its working directory
//!   NEOCHUMBY_ADMIN    the admin socket (admin.rs)
//!   NEOCHUMBY_TREE     the app tree, to read the apps' manifests
//!
//! added to neochumbyd's environment. Its stdin is /dev/null; its stdout
//! and stderr are neochumbyd's log. If it exits it is started again, after
//! 1 s, then 2, 4 and so on up to a minute; a run of a minute or more
//! starts the wait again from 1 s. When neochumbyd stops, each daemon gets
//! SIGTERM, and SIGKILL if it has not exited within five seconds.
//!
//! A daemon is in a process group of its own, so a Ctrl-C at neochumbyd's
//! terminal reaches neochumbyd alone, which then stops it as above.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use neochumby_proto::valid_name;
use neochumby_sys::process::{self, Signal};

use crate::companion::app_binary;

const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_secs(60);

/// The daemons running, each watched by a thread of its own.
pub struct Daemons {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

/// What a daemon is run with.
struct Daemon {
    app: String,
    exe: PathBuf,
    home: PathBuf,
    admin: PathBuf,
    tree: PathBuf,
}

/// Starts the daemon of every app in `tree` that has one.
pub fn start(tree: &Path, state: &Path, admin: &Path) -> Daemons {
    let stop = Arc::new(AtomicBool::new(false));
    let mut apps: Vec<String> = std::fs::read_dir(tree.join("apps"))
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    apps.sort();
    let mut threads = Vec::new();
    for app in apps.into_iter().filter(|a| valid_name(a)) {
        let Some(exe) = app_binary(tree, &app, "daemon") else { continue };
        let d = Daemon {
            home: state.join("apps").join(&app),
            app,
            exe,
            admin: admin.to_path_buf(),
            tree: tree.to_path_buf(),
        };
        let stop = stop.clone();
        threads.push(std::thread::spawn(move || supervise(&d, &stop)));
    }
    Daemons { stop, threads }
}

impl Daemons {
    /// Stops every daemon, and returns once they have all exited.
    pub fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

fn supervise(d: &Daemon, stop: &AtomicBool) {
    let mut wait = FIRST_WAIT;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        match spawn(d) {
            Ok(mut child) => {
                eprintln!("neochumbyd: daemon {} started", d.app);
                loop {
                    if stop.load(Ordering::SeqCst) {
                        terminate(&mut child);
                        eprintln!("neochumbyd: daemon {} stopped", d.app);
                        return;
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            eprintln!("neochumbyd: daemon {} exited ({status})", d.app);
                            break;
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                        Err(e) => {
                            eprintln!("neochumbyd: daemon {}: {e}", d.app);
                            terminate(&mut child);
                            break;
                        }
                    }
                }
            }
            Err(e) => eprintln!("neochumbyd: daemon {}: {}: {e}", d.app, d.exe.display()),
        }
        if started.elapsed() >= LONGEST_WAIT {
            wait = FIRST_WAIT;
        }
        eprintln!("neochumbyd: daemon {} starts again in {} s", d.app, wait.as_secs());
        pause(stop, wait);
        wait = (wait * 2).min(LONGEST_WAIT);
    }
}

fn spawn(d: &Daemon) -> std::io::Result<Child> {
    std::fs::create_dir_all(&d.home)?;
    let mut cmd = Command::new(&d.exe);
    cmd.current_dir(&d.home)
        .env("NEOCHUMBY_STATE", &d.home)
        .env("NEOCHUMBY_ADMIN", &d.admin)
        .env("NEOCHUMBY_TREE", &d.tree)
        .stdin(Stdio::null())
        .process_group(0);
    // On Linux, the kernel sends it SIGTERM if neochumbyd dies without
    // stopping it (SIGKILL, a crash). The signal follows this thread, which
    // lives as long as the daemon.
    process::term_with_parent(&mut cmd);
    cmd.spawn()
}

/// SIGTERM, then SIGKILL after five seconds, to the daemon's process
/// group: the daemon and whatever it started.
fn terminate(child: &mut Child) {
    let _ = process::kill_group(child.id(), Signal::Term);
    let deadline = Instant::now() + Duration::from_secs(5);
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = process::kill_group(child.id(), Signal::Kill);
    let _ = child.kill();
    let _ = child.wait();
}

/// Waits `d`, or until neochumbyd is stopping.
fn pause(stop: &AtomicBool, d: Duration) {
    let until = Instant::now() + d;
    while !stop.load(Ordering::SeqCst) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A daemon that exits is started again; stopping ends the one running.
    #[test]
    fn restarts_and_stops() {
        let dir = std::env::temp_dir().join(format!("neochumbyd-daemon-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let app = dir.join("tree/apps/clock");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("manifest"), "daemon=clockd\n").unwrap();
        // Counts its runs; the first exits at once, the second stays.
        let script =
            "#!/bin/sh\necho x >> runs\n[ \"$(wc -l < runs)\" -gt 1 ] && echo $$ > pid && exec sleep 600\nexit 3\n";
        std::fs::write(app.join("clockd"), script).unwrap();
        std::fs::set_permissions(app.join("clockd"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        // Not run: a path out of the app's directory, and an app with none.
        std::fs::create_dir_all(dir.join("tree/apps/sneaky")).unwrap();
        std::fs::write(dir.join("tree/apps/sneaky/manifest"), "daemon=../clock/clockd\n").unwrap();
        std::fs::create_dir_all(dir.join("tree/apps/doom")).unwrap();
        std::fs::write(dir.join("tree/apps/doom/manifest"), "exec=neochumby-doom\n").unwrap();

        let d = start(&dir.join("tree"), &dir.join("state"), &dir.join("state/admin.sock"));
        assert_eq!(d.threads.len(), 1);
        let home = dir.join("state/apps/clock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !home.join("pid").exists() {
            assert!(Instant::now() < deadline, "not started again");
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(100));
        let pid: u32 = std::fs::read_to_string(home.join("pid")).unwrap().trim().parse().unwrap();
        d.stop();
        assert!(!process::alive(pid), "still running");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
