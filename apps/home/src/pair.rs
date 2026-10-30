//! Pairing, from neochumby-home's PAIR button: this Chumby asks the
//! neochumbyd it was booted with to pair, and shows the code the two derive
//! from their connection: 32 hex digits, too many for anyone in the middle
//! to make both ends show alike. Someone runs `neochumbyd approve CODE` on
//! the server, typing the code from this screen, having seen the same code
//! listed there. Only then does anything get
//! written to /psp: this Chumby's key, the conf with the server's key
//! pinned, and the boot hook, so it boots from the server with no stick.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use neochumby::conf;
use neochumby_proto::{Identity, Msg, Unverified, hex};

use crate::term::{Paint, Rgb, Term, Touch, fit};

const WHITE: Rgb = (230, 230, 230);
const TITLE: Rgb = (255, 200, 120);
const GREEN: Rgb = (120, 220, 120);
const CANCEL: Rgb = (120, 120, 140);

/// A line of a pairing screen: its text, its colour, and whether it is
/// one of the big ones (bold).
type Line<'a> = (&'a str, Rgb, bool);

enum Progress {
    Code(String),
    Done(Result<(), String>),
}

/// Asks, then writes the pairing if approved and not cancelled.
fn ask(
    server: &str,
    api_port: u16,
    root: &Path,
    cancelled: &AtomicBool,
    tx: &mpsc::Sender<Progress>,
) -> Result<(), String> {
    let identity = match std::fs::read_to_string(conf::KEY) {
        Ok(s) => Identity::from_secret(&s).map_err(|e| e.to_string())?,
        Err(_) => Identity::generate().map_err(|e| e.to_string())?,
    };
    let addr = (server, api_port).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no address")?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).map_err(|e| e.to_string())?;
    // Pairing is how the server's key is learnt: none to check yet.
    let mut s = Unverified::connect(tcp, &identity).map_err(|e| e.to_string())?;
    s.send(&Msg::Pair { name: conf::name() }).map_err(|e| e.to_string())?;
    let _ = tx.send(Progress::Code(s.code()));
    let Msg::Paired { nfs_port, path } = s.recv().map_err(|e| e.to_string())? else {
        return Err("refused".into());
    };
    if cancelled.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }

    let c = conf::render(&[
        ("server", server.to_string()),
        ("api_port", api_port.to_string()),
        ("server_key", hex(s.peer())),
        ("nfs_port", nfs_port.to_string()),
        ("mount_port", nfs_port.to_string()),
        ("path", path),
    ])
    .map_err(|e| e.to_string())?;
    let hook = std::fs::read(root.join("userhook2")).map_err(|e| format!("userhook2: {e}"))?;
    let w = |p: &str, d: &[u8], m| conf::write_atomic(Path::new(p), d, m).map_err(|e| format!("{p}: {e}"));
    w(conf::KEY, identity.secret().as_bytes(), 0o600)?;
    w(conf::CONF, c.as_bytes(), 0o644)?;
    w("/psp/rfs1/userhook2", &hook, 0o755)?;
    // What checks the tree over NFS from the next boot on, from the tree
    // pairing runs from, as the hook is (neochumby::verified).
    if let Err(e) = neochumby::verified::install_boot(root) {
        eprintln!("neochumby-home: {}: {e}", neochumby::verified::BOOT);
    }
    Ok(())
}

/// How many rows CANCEL takes along the bottom of a terminal `rows` high.
fn cancel_rows(rows: usize) -> usize {
    if rows >= 12 { 3 } else { 1 }
}

/// A pairing screen: `lines`, each in the middle, a blank row between the
/// big ones and the rest, and CANCEL along the bottom if `cancel`.
fn screen(t: &mut Term, lines: &[Line], cancel: bool) {
    let mut p = Paint::cleared();
    let mut row = 1;
    for (text, colour, big) in lines {
        let text = fit(text, t.cols);
        p.at((t.cols - text.len()) / 2, row).text(&text, *colour, None, *big);
        row += if *big { 2 } else { 1 };
    }
    if cancel {
        let h = cancel_rows(t.rows);
        let top = t.rows - h;
        p.fill(1, top, t.cols.saturating_sub(2), h, CANCEL);
        p.at(t.cols.saturating_sub(6) / 2, top + (h - 1) / 2).text("CANCEL", (0, 0, 0), Some(CANCEL), true);
    }
    t.show(&p.0);
}

/// Runs the pairing screen until paired, refused or cancelled (CANCEL, or
/// `cancel` returning true: the top button); true if paired.
pub fn run(t: &mut Term, server: &str, api_port: u16, root: &Path, mut cancel: impl FnMut() -> bool) -> bool {
    screen(t, &[("PAIRING", TITLE, true), (server, WHITE, true), ("connecting...", WHITE, false)], true);

    let cancelled = Arc::new(AtomicBool::new(false));
    let (tx, rx): (_, Receiver<Progress>) = mpsc::channel();
    {
        let (server, root, cancelled) = (server.to_string(), root.to_path_buf(), cancelled.clone());
        std::thread::spawn(move || {
            let r = ask(&server, api_port, &root, &cancelled, &tx);
            let _ = tx.send(Progress::Done(r));
        });
    }

    let mut down_on_cancel = false;
    loop {
        while let Ok(p) = rx.try_recv() {
            match p {
                Progress::Code(code) => {
                    // In the log too, for dev/emulate-pairing to compare.
                    eprintln!("neochumby-home: pairing with {server}, showing {code}");
                    // Eight groups of four: four to a line.
                    let (first, second) = code.split_at(code.len().min(19));
                    let second = second.trim_start_matches('-');
                    screen(
                        t,
                        &[
                            ("PAIRING", TITLE, true),
                            ("on the server, run", WHITE, false),
                            ("neochumbyd approve CODE", WHITE, false),
                            ("typing this code:", WHITE, true),
                            (first, GREEN, true),
                            (second, GREEN, true),
                        ],
                        true,
                    );
                }
                Progress::Done(r) => {
                    match &r {
                        Ok(()) => eprintln!("neochumby-home: paired with {server}"),
                        Err(e) => eprintln!("neochumby-home: not paired: {e}"),
                    }
                    let ok = r.is_ok();
                    let msg = match &r {
                        Ok(()) => "remove any stick, restart".to_string(),
                        Err(e) => e.chars().take(36).collect(),
                    };
                    let title = if ok { "PAIRED" } else { "NOT PAIRED" };
                    screen(t, &[(title, TITLE, true), (&msg, WHITE, false)], false);
                    neochumby::sleep_ms(4000);
                    return ok;
                }
            }
        }
        for touch in t.poll(50) {
            match touch {
                Touch::Down { row, .. } => down_on_cancel = row >= t.rows - cancel_rows(t.rows),
                Touch::Up if down_on_cancel => {
                    cancelled.store(true, Ordering::SeqCst);
                    return false;
                }
                Touch::Up => {}
            }
        }
        if cancel() {
            cancelled.store(true, Ordering::SeqCst);
            return false;
        }
    }
}
