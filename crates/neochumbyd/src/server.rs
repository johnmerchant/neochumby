//! Apps' server programs: the half an app may have that runs once on the
//! server for all its Chumbys, rather than once per Chumby as a companion
//! does, named in its manifest (`server=<file in the app's directory>`).
//! The household web page is one. neochumbyd starts each app's server
//! program when it starts, sandboxed (confine.rs), as its own process, with
//!
//!   NEOCHUMBY_STATE    the app's own directory (<state>/apps/<app>), kept
//!                      across runs and shared with its companions; also
//!                      its working directory and HOME, and the only place
//!                      it may write (TMPDIR is its `tmp`)
//!   NEOCHUMBY_TREE     the app tree, to read the apps' manifests
//!   NEOCHUMBY_ADMIN    only for an app its owner has allowed `pairing`
//!                      (`neochumbyd allow <app> pairing`): 3, the
//!                      inherited descriptor of a connected Unix socket to
//!                      neochumbyd's admin protocol (admin.rs). It has no
//!                      path. The program should set it close-on-exec as
//!                      it starts, so that what it runs doesn't get it.
//!
//! and the environment confine.rs passes on. Its stdin is /dev/null; its
//! stdout and stderr go to neochumbyd's log, a line at a time. It may bind
//! only the TCP ports its manifest lists (`listen`), where Landlock can
//! say (ABI 4 on). If it exits it is started again, after
//! 1 s, then 2, 4 and so on up to a minute; a run of a minute or more
//! starts the wait again from 1 s. When neochumbyd stops, each gets
//! SIGTERM, and SIGKILL if it has not exited within five seconds.
//!
//! Each is in a session of its own, so a Ctrl-C at neochumbyd's terminal
//! reaches neochumbyd alone, which then stops it as above.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use neochumby_proto::valid_name;
use neochumby_sys::process::{self, Signal};

use crate::companion::{app_binary, manifest_value};
use crate::confine::{self, Confine, Plan};
use crate::state::{PAIRING, State};

const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_secs(60);

/// The server programs running, each watched by a thread of its own.
pub struct Servers {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

/// What a server program is run with.
struct Server {
    app: String,
    exe: PathBuf,
    home: PathBuf,
    state_dir: PathBuf,
    tree: PathBuf,
    /// neochumbyd's state: whether the owner allows it `pairing`, and the
    /// admin socket's answers if so.
    state: State,
    /// The TCP ports it may bind.
    listen: Vec<u16>,
    plan: Plan,
}

/// Starts the server program of every app in `tree` that has one.
pub fn start(tree: &Path, state_dir: &Path, state: &State, confine: &Confine) -> Servers {
    let stop = Arc::new(AtomicBool::new(false));
    let mut apps: Vec<String> = std::fs::read_dir(tree.join("apps"))
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    apps.sort();
    let mut threads = Vec::new();
    for app in apps.into_iter().filter(|a| valid_name(a)) {
        let Some(exe) = app_binary(tree, &app, "server") else { continue };
        let plan = match confine.plan(&app) {
            Ok(plan) => plan,
            Err(why) => {
                eprintln!("neochumbyd: server {app} not started: {why}");
                continue;
            }
        };
        let pairing = if state.allowed(&app).iter().any(|w| w == PAIRING) { ", allowed pairing" } else { "" };
        eprintln!("neochumbyd: server {app}: {plan}{pairing}");
        let listen = manifest_value(&tree.join("apps").join(&app), "listen").unwrap_or_default();
        let s = Server {
            listen: listen.split(',').filter_map(|p| p.trim().parse().ok()).collect(),
            home: state_dir.join("apps").join(&app),
            state: state.clone(),
            app,
            exe,
            state_dir: state_dir.to_path_buf(),
            tree: tree.to_path_buf(),
            plan,
        };
        let stop = stop.clone();
        threads.push(std::thread::spawn(move || supervise(&s, &stop)));
    }
    Servers { stop, threads }
}

impl Servers {
    /// Stops every server program, and returns once they have all exited.
    pub fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

fn supervise(s: &Server, stop: &AtomicBool) {
    let mut wait = FIRST_WAIT;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        match spawn(s) {
            Ok(mut child) => {
                eprintln!("neochumbyd: server {} started", s.app);
                loop {
                    if stop.load(Ordering::SeqCst) {
                        terminate(&mut child);
                        eprintln!("neochumbyd: server {} stopped", s.app);
                        return;
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            eprintln!("neochumbyd: server {} exited ({status})", s.app);
                            break;
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                        Err(e) => {
                            eprintln!("neochumbyd: server {}: {e}", s.app);
                            terminate(&mut child);
                            break;
                        }
                    }
                }
            }
            Err(e) => eprintln!("neochumbyd: server {}: {}: {e}", s.app, s.exe.display()),
        }
        if started.elapsed() >= LONGEST_WAIT {
            wait = FIRST_WAIT;
        }
        eprintln!("neochumbyd: server {} starts again in {} s", s.app, wait.as_secs());
        pause(stop, wait);
        wait = (wait * 2).min(LONGEST_WAIT);
    }
}

fn spawn(s: &Server) -> std::io::Result<Child> {
    std::fs::create_dir_all(&s.home)?;
    let mut cmd = s.plan.command(&s.exe, &s.state_dir, &s.tree, &s.home, Some(&s.listen))?;
    // The admin socket, if the owner allows it: a connected pair, made for
    // this run. The program gets its end as descriptor 3, and neochumbyd
    // answers on the other until it closes.
    let admin = if s.state.allowed(&s.app).iter().any(|w| w == PAIRING) { Some(UnixStream::pair()?) } else { None };
    let home = s.plan.prepare(&mut cmd, &s.home, admin.as_ref().map(|(_, theirs)| theirs.as_raw_fd()))?;
    cmd.env("NEOCHUMBY_STATE", home)
        .env("NEOCHUMBY_TREE", s.tree.canonicalize()?)
        .env_remove("NEOCHUMBY_ADMIN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if admin.is_some() {
        cmd.env("NEOCHUMBY_ADMIN", "3");
    }
    // On Linux, the kernel sends it SIGTERM if neochumbyd dies without
    // stopping it (SIGKILL, a crash). The signal follows this thread, which
    // lives as long as the server program.
    process::term_with_parent(&mut cmd);
    let mut child = cmd.spawn()?;
    if let Some(out) = child.stdout.take() {
        confine::log(s.app.clone(), out);
    }
    if let Some(err) = child.stderr.take() {
        confine::log(s.app.clone(), err);
    }
    if let Some((ours, theirs)) = admin {
        drop(theirs);
        let (app, state) = (s.app.clone(), s.state.clone());
        std::thread::spawn(move || {
            if let Err(e) = crate::admin::serve(&state, ours, &app) {
                eprintln!("neochumbyd: admin socket of {app}: {e}");
            }
        });
    }
    Ok(child)
}

/// SIGTERM, then SIGKILL after five seconds, to the program's process
/// group: the program and whatever it started.
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

    /// A server program that exits is started again; stopping ends the one
    /// running. Sandboxed where this system can.
    #[test]
    fn restarts_and_stops() {
        let dir = std::env::temp_dir().join(format!("neochumbyd-server-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let app = dir.join("tree/apps/clock");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("manifest"), "server=clockd\n").unwrap();
        // Counts its runs; the first exits at once, the second stays.
        let script =
            "#!/bin/sh\necho x >> runs\n[ \"$(wc -l < runs)\" -gt 1 ] && echo $$ > pid && exec sleep 600\nexit 3\n";
        std::fs::write(app.join("clockd"), script).unwrap();
        std::fs::set_permissions(app.join("clockd"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        // Not run: a path out of the app's directory, and an app with none.
        std::fs::create_dir_all(dir.join("tree/apps/sneaky")).unwrap();
        std::fs::write(dir.join("tree/apps/sneaky/manifest"), "server=../clock/clockd\n").unwrap();
        std::fs::create_dir_all(dir.join("tree/apps/doom")).unwrap();
        std::fs::write(dir.join("tree/apps/doom/manifest"), "exec=neochumby-doom\n").unwrap();

        let state = State::open(dir.join("state")).unwrap();
        let confine = Confine::for_tests();
        let d = start(&dir.join("tree"), &dir.join("state"), &state, &confine);
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

        // Without a sandbox, and without --unsandboxed, none runs.
        let d = start(&dir.join("tree"), &dir.join("state"), &state, &Confine::Refused("test".into()));
        assert_eq!(d.threads.len(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
