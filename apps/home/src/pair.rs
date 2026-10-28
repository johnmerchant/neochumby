//! Pairing, from the home screen's PAIR button: this Chumby asks the
//! neochumbyd it was booted with to pair, and shows the code the two derive
//! from their connection. Someone runs `neochumbyd approve <code>` on the
//! server, having seen the same code there. Only then does anything get
//! written to /psp: this Chumby's key, the conf with the server's key
//! pinned, and the boot hook, so it boots from the server with no stick.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use neochumby::conf;
use neochumby::fb::{self, Fb, rgb};
use neochumby::input::{Touch, Touchscreen};
use neochumby_proto::{Identity, Msg, Session, hex};

const BG: u16 = rgb(16, 16, 32);

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
    let mut s = Session::connect(tcp, &identity).map_err(|e| e.to_string())?;
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
    Ok(())
}

fn screen(fb: &mut Fb, lines: &[(&str, usize, u16)], cancel: bool) {
    fb.clear(BG);
    let mut y = 24;
    for (text, scale, colour) in lines {
        let x = fb::W.saturating_sub(Fb::text_width(text, *scale)) / 2;
        fb.text(x, y, text, *scale, *colour);
        y += 8 * scale + 12;
    }
    if cancel {
        fb.rect(8, fb::H - 48, fb::W - 16, 40, rgb(120, 120, 140));
        fb.text((fb::W - Fb::text_width("CANCEL", 2)) / 2, fb::H - 36, "CANCEL", 2, rgb(0, 0, 0));
    }
}

/// Runs the pairing screen until paired, refused or cancelled; true if
/// paired.
pub fn run(fb: &mut Fb, touch: &mut Touchscreen, server: &str, api_port: u16, root: &Path) -> bool {
    let white = rgb(230, 230, 230);
    screen(fb, &[("PAIRING", 3, rgb(255, 200, 120)), (server, 2, white), ("connecting...", 2, white)], true);

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
                    let cmd = format!("neochumbyd approve {code}");
                    screen(
                        fb,
                        &[
                            ("PAIRING", 3, rgb(255, 200, 120)),
                            ("on the server, run", 1, white),
                            (&cmd, 1, white),
                            (&code, 4, rgb(120, 220, 120)),
                        ],
                        true,
                    );
                }
                Progress::Done(r) => {
                    let ok = r.is_ok();
                    let msg = match &r {
                        Ok(()) => "remove any stick, restart".to_string(),
                        Err(e) => e.chars().take(36).collect(),
                    };
                    let title = if ok { "PAIRED" } else { "NOT PAIRED" };
                    screen(fb, &[(title, 3, rgb(255, 200, 120)), (&msg, 1, white)], false);
                    neochumby::sleep_ms(4000);
                    return ok;
                }
            }
        }
        for t in touch.poll() {
            match t {
                Touch::Down { y, .. } => down_on_cancel = y as usize >= fb::H - 48,
                Touch::Up if down_on_cancel => {
                    cancelled.store(true, Ordering::SeqCst);
                    return false;
                }
                Touch::Up => {}
            }
        }
        neochumby::sleep_ms(50);
    }
}
