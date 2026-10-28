//! The wx-feed companion: the renderer of a Chumby's wx pages and screens,
//! which neochumbyd runs for each Chumby that opens the app
//! (`Msg::Open { app: "wx-feed" }`). The rendering is Python and Pillow
//! (python/wxfeed.py); this only runs it. A render now, then one every
//! NEOCHUMBY_WX_EVERY seconds (default 600, at least 60), and one at once
//! whenever the web page asks for it (it touches refresh-request in the
//! state directory).
//!
//! Each Chumby's output goes to its own directory: NEOCHUMBY_WX_OUT with
//! {chumby} replaced by its name, else neochumbyd's overlay for it,
//! <neochumbyd state>/overlay/<chumby>/apps/wx-feed/out/. sync layers the
//! overlay over the tree for that Chumby alone, so its wx engine reads
//! apps/wx-feed/out/manifest.txt in its tree. NFS doesn't serve the overlay
//! yet: NFS Chumbys need a follow-up.
//! The renderer keeps the rendering contract (stable names, a file written
//! only when its bytes change, the manifest last), so a render that
//! changes nothing writes nothing.
//!
//! To the Chumby, one kind of frame: [0] followed by the manifest's
//! version (ASCII digits), after each render that changed it. Facts the
//! Chumby sends about itself, if it ever does, would go to
//! <state>/info/<chumby> for the Settings screens (wxfeed.py --info).
//! Exits when its stdin closes: the Chumby has gone.

mod py;

use std::io::{self, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use neochumby_proto::{frame, valid_name};

const RENDERED: u8 = 0;
/// A render that takes longer than this is stuck (a fetch hanging past its
/// own timeouts), and is killed.
const RENDER_TIMEOUT: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(500);

/// Seconds between renders, from NEOCHUMBY_WX_EVERY.
fn every(v: Option<&str>) -> Duration {
    Duration::from_secs(v.and_then(|s| s.trim().parse().ok()).unwrap_or(600u64).max(60))
}

/// Where `chumby`'s pictures go.
fn out_dir(out: Option<&str>, state: &Path, chumby: &str) -> PathBuf {
    py::out_template(out, state).replace(py::CHUMBY, chumby).into()
}

/// The manifest's version line, if it has one.
fn version(out: &Path) -> Option<String> {
    let m = std::fs::read_to_string(out.join("manifest.txt")).ok()?;
    let v = m.lines().find_map(|l| l.strip_prefix("version "))?.trim();
    (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())).then(|| v.to_string())
}

/// Waits up to `t` for the Chumby to go; true if it has.
fn went(gone: &Receiver<()>, t: Duration) -> bool {
    !matches!(gone.recv_timeout(t), Err(RecvTimeoutError::Timeout))
}

fn modified(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// One run of wxfeed.py. Its output goes to our stderr (neochumbyd's log):
/// our stdout carries frames. Returns false if the Chumby went meanwhile.
fn render(chumby: &str, state: &Path, out: &Path, gone: &Receiver<()>) -> io::Result<bool> {
    std::fs::create_dir_all(out)?;
    let mut cmd = py::command("wxfeed.py")?;
    cmd.arg(out).arg("--config").arg(state.join("config.json"));
    let info = state.join("info").join(chumby);
    if info.is_file() {
        cmd.arg("--info").arg(info);
    }
    let log = io::stderr().as_fd().try_clone_to_owned()?;
    let mut child = cmd.env("NEOCHUMBY_CHUMBY", chumby).stdin(Stdio::null()).stdout(Stdio::from(log)).spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                eprintln!("neochumby-wx-feed: render for {chumby} failed ({status})");
            }
            return Ok(true);
        }
        let stuck = start.elapsed() > RENDER_TIMEOUT;
        if stuck || went(gone, POLL) {
            if stuck {
                eprintln!("neochumby-wx-feed: render for {chumby} took over {RENDER_TIMEOUT:?}; stopped");
            }
            let _ = child.kill();
            let _ = child.wait();
            return Ok(stuck);
        }
    }
}

fn run() -> io::Result<()> {
    let chumby = std::env::var("NEOCHUMBY_CHUMBY")
        .ok()
        .filter(|n| valid_name(n))
        .ok_or_else(|| io::Error::other("no NEOCHUMBY_CHUMBY"))?;
    let state = py::state_dir();
    let out = out_dir(std::env::var("NEOCHUMBY_WX_OUT").ok().as_deref(), &state, &chumby);
    let interval = every(std::env::var("NEOCHUMBY_WX_EVERY").ok().as_deref());
    let refresh = state.join("refresh-request");
    eprintln!("neochumby-wx-feed: rendering for {chumby} into {} every {}s", out.display(), interval.as_secs());

    // The Chumby going (neochumbyd closes our stdin) ends the run. It sends
    // nothing we use yet.
    let (gone_tx, gone) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        while let Ok(Some(_)) = frame::read(&mut stdin) {}
        let _ = gone_tx.send(());
    });

    let mut sent = None;
    let mut stdout = io::stdout().lock();
    loop {
        let asked = modified(&refresh);
        if !render(&chumby, &state, &out, &gone)? {
            return Ok(());
        }
        let v = version(&out);
        if v.is_some() && v != sent {
            let mut f = vec![RENDERED];
            f.extend_from_slice(v.as_deref().unwrap_or_default().as_bytes());
            frame::write(&mut stdout, &f)?;
            stdout.flush()?;
            sent = v;
        }
        // Until the next render is due, or the web page asks for one.
        let due = Instant::now() + interval;
        while Instant::now() < due {
            if went(&gone, Duration::from_secs(2)) {
                return Ok(());
            }
            if modified(&refresh) > asked {
                break;
            }
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("neochumby-wx-feed: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_has_a_floor_and_a_default() {
        assert_eq!(every(None), Duration::from_secs(600));
        assert_eq!(every(Some("300")), Duration::from_secs(300));
        assert_eq!(every(Some("5")), Duration::from_secs(60));
        assert_eq!(every(Some("soon")), Duration::from_secs(600));
    }

    #[test]
    fn each_chumby_has_its_own_directory() {
        let state = Path::new("/var/lib/neochumbyd/apps/wx-feed");
        let overlay = Path::new("/var/lib/neochumbyd/overlay/kitchen/apps/wx-feed/out");
        assert_eq!(out_dir(None, state, "kitchen"), overlay);
        assert_eq!(out_dir(Some(""), state, "kitchen"), overlay);
        assert_eq!(out_dir(Some("/srv/wx/"), state, "kitchen"), Path::new("/srv/wx/kitchen"));
        let overlay = "/var/lib/neochumbyd/overlay/{chumby}/apps/wx-feed/out";
        assert_eq!(
            out_dir(Some(overlay), state, "kitchen"),
            Path::new("/var/lib/neochumbyd/overlay/kitchen/apps/wx-feed/out")
        );
    }

    #[test]
    fn reads_the_manifest_version() {
        let dir = std::env::temp_dir().join(format!("wx-feed-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(version(&dir), None);
        std::fs::write(dir.join("manifest.txt"), "# wxfeed 0123abcd\nversion 1793178737\nsun 1 2\n").unwrap();
        assert_eq!(version(&dir).as_deref(), Some("1793178737"));
        std::fs::write(dir.join("manifest.txt"), "version 12x\n").unwrap();
        assert_eq!(version(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
