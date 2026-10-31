//! Apps' companions: the server half an app may have, named in its
//! manifest (`companion=<file in the app's directory>`). For each Chumby
//! that opens it, neochumbyd runs one, sandboxed (confine.rs), as its own
//! process (with `--app-uids`, under the app's own uid, as its server
//! program runs), with
//!
//!   NEOCHUMBY_CHUMBY   the Chumby's name, as it paired
//!   NEOCHUMBY_STATE    a directory of the app's own, kept across runs
//!                      (<state>/apps/<app>), which is also its working
//!                      directory and HOME, and the only place it may
//!                      write (TMPDIR is its `tmp`); the app's server
//!                      program, if it has one (server.rs), shares it
//!
//! and the environment confine.rs passes on (anything an app needs from
//! the host, such as a port range, comes in a NEOCHUMBY_ variable), and
//! relays frames (neochumby_proto::frame): what the Chumby sends goes to
//! its stdin, what it writes to stdout goes to the Chumby. Its stderr goes
//! to neochumbyd's log. Without a sandbox, and without `--unsandboxed`, it
//! isn't run, and the Chumby is told the app has none.
//! When the Chumby goes, its stdin is closed; it should exit then, and is
//! killed if it has not within five seconds.
//!
//! One companion is neochumbyd's own: `companion=neochumbyd:catalog`
//! (neochumby_proto::catalog::COMPANION) is the catalog, GET APPS, served
//! here (catalog/chumby.rs). An app installed from the catalog never gets
//! it.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use neochumby_proto::{Msg, Session, frame, valid_name};

use crate::confine::{self, Confine};

/// The companion, stopped as above when dropped.
struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Drop for Running {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `key`'s value in the manifest of the app in `dir`.
pub fn manifest_value(dir: &Path, key: &str) -> Option<String> {
    let m = std::fs::read_to_string(dir.join("manifest")).ok()?;
    m.lines().filter_map(|l| l.split_once('=')).find(|(k, _)| k.trim() == key).map(|(_, v)| v.trim().to_string())
}

/// The binary `app`'s manifest names by `key` (`companion`, `server`):
/// only a plain name, in the app's own directory.
pub fn app_binary(tree: &Path, app: &str, key: &str) -> Option<PathBuf> {
    let dir = tree.join("apps").join(app);
    let exe = valid_name(app)
        .then(|| manifest_value(&dir, key))
        .flatten()
        .filter(|f| !f.is_empty() && !f.contains('/') && f != "." && f != "..")?;
    Some(dir.join(exe))
}

/// Runs `app`'s companion for `chumby` and relays between them until
/// either goes.
pub fn run(tree: &Path, state: &Path, confine: &Confine, chumby: &str, app: &str, mut s: Session) -> io::Result<()> {
    let dir = tree.join("apps").join(app);
    if valid_name(app) && manifest_value(&dir, "companion").as_deref() == Some(neochumby_proto::catalog::COMPANION) {
        // Never an app from the catalog's.
        if manifest_value(&dir, "source").as_deref() == Some("catalog") {
            return s.send(&Msg::Closed);
        }
        return crate::catalog::chumby::run(tree, state, chumby, s);
    }
    let Some(exe) = app_binary(tree, app, "companion") else {
        return s.send(&Msg::Closed);
    };
    let home = state.join("apps").join(app);
    std::fs::create_dir_all(&home)?;
    let plan = match confine.plan(app) {
        Ok(plan) => plan,
        Err(why) => {
            eprintln!("neochumbyd: {app} for {chumby} not started: {why}");
            return s.send(&Msg::Closed);
        }
    };
    let mut cmd = plan.command(&exe, state, tree, &home, None)?;
    let home = plan.prepare(&mut cmd, &home, None)?;
    let mut child = cmd
        .env("NEOCHUMBY_CHUMBY", chumby)
        .env("NEOCHUMBY_STATE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(err) = child.stderr.take() {
        confine::log(format!("{app} for {chumby}"), err);
    }
    let mut out = child.stdout.take().expect("piped");
    let stdin = child.stdin.take();
    let mut run = Running { child, stdin };
    eprintln!("neochumbyd: {app} for {chumby}, {plan}");

    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(16);
    std::thread::spawn(move || {
        while let Ok(Some(f)) = frame::read(&mut out) {
            if tx.send(f).is_err() {
                break;
            }
        }
    });

    loop {
        // From the companion, as it comes...
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(f) => {
                s.send(&Msg::Data(f))?;
                for f in rx.try_iter() {
                    s.send(&Msg::Data(f))?;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("neochumbyd: {app} for {chumby} exited");
                return s.send(&Msg::Closed);
            }
        }
        // ...and from the Chumby, which sends nothing else once open (a
        // newer one's `Msg::Unknown` is skipped).
        if let Some(Msg::Data(d)) = s.recv_timeout(Duration::from_millis(10))?
            && let Some(stdin) = run.stdin.as_mut()
        {
            frame::write(stdin, &d)?;
        }
    }
}
