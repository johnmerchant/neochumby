//! Apps' companions: the server half an app may have, named in its
//! manifest (`companion=<file in the app's directory>`). For each Chumby
//! that opens it, neochumbyd runs one, as its own process, with
//!
//!   NEOCHUMBY_CHUMBY   the Chumby's name, as it paired
//!   NEOCHUMBY_STATE    a directory of the app's own, kept across runs
//!                      (<state>/apps/<app>), which is also its working
//!                      directory
//!
//! added to neochumbyd's environment (anything an app needs from the host,
//! such as a port range, comes through it), and relays frames
//! (neochumby_proto::frame): what the Chumby sends goes to its stdin, what
//! it writes to stdout goes to the Chumby. Its stderr is neochumbyd's log.
//! When the Chumby goes, its stdin is closed; it should exit then, and is
//! killed if it has not within five seconds.

use std::io;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use neochumby_proto::{Msg, Session, frame, valid_name};

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

/// `key`'s value in the app's manifest.
fn manifest(dir: &Path, key: &str) -> Option<String> {
    let m = std::fs::read_to_string(dir.join("manifest")).ok()?;
    m.lines().filter_map(|l| l.split_once('=')).find(|(k, _)| k.trim() == key).map(|(_, v)| v.trim().to_string())
}

/// Runs `app`'s companion for `chumby` and relays between them until
/// either goes.
pub fn run(tree: &Path, state: &Path, chumby: &str, app: &str, mut s: Session) -> io::Result<()> {
    let dir = tree.join("apps").join(app);
    // Only a plain name in the app's own directory.
    let exe = valid_name(app)
        .then(|| manifest(&dir, "companion"))
        .flatten()
        .filter(|f| !f.is_empty() && !f.contains('/') && f != "." && f != "..");
    let Some(exe) = exe else {
        return s.send(&Msg::Closed);
    };
    let home = state.join("apps").join(app);
    std::fs::create_dir_all(&home)?;
    let mut child = Command::new(dir.join(&exe))
        .current_dir(&home)
        .env("NEOCHUMBY_CHUMBY", chumby)
        .env("NEOCHUMBY_STATE", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut out = child.stdout.take().expect("piped");
    let stdin = child.stdin.take();
    let mut run = Running { child, stdin };
    eprintln!("neochumbyd: {app} for {chumby}");

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
