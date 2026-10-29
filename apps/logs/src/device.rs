//! The viewer on the Chumby: the screen, the touchscreen, the Chumby's own
//! logs, and (when paired) the host's, from the companion on a thread of
//! their own so that a slow server never holds up the screen.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use neochumby::conf;
use neochumby::fb::Fb;
use neochumby::input::{Touch, Touchscreen};
use neochumby_proto::{Identity, Msg};
use neochumby_sys::chan::{self, Disconnected, Receiver, Sender};

use crate::frame as tag;
use crate::tail::{self, Tail};
use crate::text::Text;
use crate::view::{self, Action, Bar, View};

/// The session's own log, which the boot hook and every app write to.
const SESSION_LOG: &str = "/tmp/neochumby.log";
/// The screen's tick: touches read, logs looked at.
const TICK: u32 = 40;
/// How often a file, and the kernel's ring, are read for more.
const FILE_POLL: Duration = Duration::from_millis(500);
const RING_POLL: Duration = Duration::from_secs(3);
/// The log is redrawn at most this often while it pours in.
const REDRAW: Duration = Duration::from_millis(150);

enum Kind {
    File(PathBuf),
    /// dmesg: the kernel's ring.
    Kernel,
    /// The host's, by its number at the companion.
    Host(u8),
}

struct Source {
    name: String,
    kind: Kind,
}

/// What is being read for the log shown.
enum Feed {
    File(Tail),
    Kernel(Vec<u8>),
    /// The host's: live once the companion has started it.
    Host(bool),
    /// Nothing more to read (dmesg wouldn't run).
    Done,
}

/// The Chumby's logs: the session's, the kernel's, then the other logs in
/// /tmp (*.log) and /var/log.
fn local() -> Vec<Source> {
    let mut v = vec![
        Source { name: "neochumby".into(), kind: Kind::File(SESSION_LOG.into()) },
        Source { name: "kernel".into(), kind: Kind::Kernel },
    ];
    for (dir, only_logs) in [("/tmp", true), ("/var/log", false)] {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        let mut found: Vec<PathBuf> = rd
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .map(|e| e.path())
            .filter(|p| !only_logs || p.extension().is_some_and(|x| x == "log"))
            .filter(|p| p != Path::new(SESSION_LOG))
            .collect();
        found.sort();
        v.extend(found.into_iter().map(|p| Source { name: p.display().to_string(), kind: Kind::File(p) }));
    }
    v
}

/// From the companion's thread.
enum FromHost {
    Sources(Vec<String>),
    Start(u8),
    Text(Vec<u8>),
    Lost(String),
}

/// The companion's thread: connects, says which log to show (`show`), and
/// passes on what comes; when the connection goes, tries again, backing
/// off. Each connection starts with the companion's list of logs, after
/// which the screen asks again for the one it shows. Ends when the
/// screen's end of either channel goes.
fn host(p: conf::Paired, me: Identity, show: Receiver<u8>, to: Sender<FromHost>) {
    let mut backoff = 5;
    loop {
        let r: std::io::Result<()> = (|| {
            let mut s = neochumby::companion::open(neochumby::companion!(), &p, &me)?;
            backoff = 5;
            loop {
                loop {
                    match show.recv(0) {
                        Ok(Some(i)) => s.send(&Msg::Data(vec![tag::SHOW, i]))?,
                        Ok(None) => break,
                        Err(Disconnected) => return Ok(()),
                    }
                }
                let ev = match s.recv_timeout(Duration::from_millis(100))? {
                    Some(Msg::Data(f)) => match f.split_first() {
                        Some((&tag::SOURCES, names)) => FromHost::Sources(
                            String::from_utf8_lossy(names)
                                .split('\n')
                                .filter(|n| !n.is_empty())
                                .map(String::from)
                                .collect(),
                        ),
                        Some((&tag::START, &[i])) => FromHost::Start(i),
                        Some((&tag::TEXT, t)) => FromHost::Text(t.to_vec()),
                        _ => continue,
                    },
                    Some(Msg::Closed) => return Err(std::io::Error::other("the companion has gone")),
                    _ => continue,
                };
                if to.send(ev).is_err() {
                    return Ok(());
                }
            }
        })();
        match r {
            Ok(()) => return,
            Err(e) => {
                if to.send(FromHost::Lost(e.to_string())).is_err() {
                    return;
                }
            }
        }
        // Waiting; what is asked for meanwhile is asked again once the
        // list has come.
        let until = Instant::now() + Duration::from_secs(backoff);
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if show.recv(left.as_millis() as i32) == Err(Disconnected) {
                return;
            }
        }
        backoff = (backoff * 2).min(60);
    }
}

/// The kernel's ring, as dmesg prints it.
fn dmesg() -> Result<Vec<u8>, String> {
    match std::process::Command::new("dmesg").output() {
        Ok(o) => Ok(o.stdout),
        Err(e) => Err(format!("logs: dmesg: {e}")),
    }
}

pub fn run() -> i32 {
    let mut fb = match Fb::open() {
        Ok(fb) => fb,
        Err(e) => {
            eprintln!("neochumby-logs: the screen: {e}");
            return 1;
        }
    };
    let mut touch = Touchscreen::open();
    let mut sources = local();

    // The host's logs, when there is a server to ask.
    let mut to_host: Option<Sender<u8>> = None;
    let mut from_host: Option<Receiver<FromHost>> = None;
    if let Some(p) = conf::paired() {
        match (Identity::from_secret(&p.secret), chan::channel(), chan::channel()) {
            (Ok(me), Ok((show_tx, show_rx)), Ok((ev_tx, ev_rx))) => {
                std::thread::spawn(move || host(p, me, show_rx, ev_tx));
                to_host = Some(show_tx);
                from_host = Some(ev_rx);
            }
            (Err(e), ..) => eprintln!("neochumby-logs: {}: {e}", conf::KEY),
            _ => eprintln!("neochumby-logs: no pipe for the server's logs"),
        }
    }

    let mut current = 0;
    let mut text = Text::new();
    let mut view = View::new();
    let mut feed = Feed::Done;
    let mut switch = true;
    let mut polled = Instant::now();
    let mut drawn = Instant::now();
    let (mut frame_dirty, mut log_dirty) = (true, true);
    loop {
        if std::mem::take(&mut switch) {
            text.clear();
            view = View::new();
            feed = match &sources[current].kind {
                Kind::File(p) => Feed::File(Tail::new(p)),
                Kind::Kernel => Feed::Kernel(Vec::new()),
                Kind::Host(i) => {
                    if let Some(tx) = &to_host {
                        let _ = tx.send(*i);
                    }
                    text.note("logs: asking the server...");
                    Feed::Host(false)
                }
            };
            // At once.
            polled = Instant::now() - RING_POLL;
            frame_dirty = true;
            log_dirty = true;
        }

        let following = view.follow;
        // Scrolled by hand: drawn at once, not when the next lines are.
        let mut touched = false;
        for t in touch.poll() {
            let t = match t {
                Touch::Down { x, y, .. } => view::Touch::Down(x, y),
                Touch::Up => view::Touch::Up,
            };
            let (scrolled, action) = view.touch(t, &text);
            log_dirty |= scrolled;
            touched |= scrolled;
            match action {
                Some(Action::Previous) => {
                    current = (current + sources.len() - 1) % sources.len();
                    switch = true;
                }
                Some(Action::Next) => {
                    current = (current + 1) % sources.len();
                    switch = true;
                }
                Some(a) => {
                    view.act(a, &text);
                    log_dirty = true;
                    touched = true;
                }
                None => {}
            }
        }
        frame_dirty |= view.follow != following;

        let mut more = false;
        match &mut feed {
            Feed::File(t) if polled.elapsed() >= FILE_POLL => {
                polled = Instant::now();
                let b = t.poll();
                more = !b.is_empty();
                text.push(&b);
            }
            Feed::Kernel(old) if polled.elapsed() >= RING_POLL => {
                polled = Instant::now();
                match dmesg() {
                    Ok(new) => {
                        match tail::after(old, &new) {
                            Some(b) => text.push(b),
                            None => {
                                text.clear();
                                text.push(&new);
                            }
                        }
                        more = new != *old;
                        *old = new;
                    }
                    Err(e) => {
                        text.note(&e);
                        feed = Feed::Done;
                        more = true;
                    }
                }
            }
            _ => {}
        }

        if let Some(rx) = &from_host {
            while let Ok(Some(ev)) = rx.recv(0) {
                let shown = match sources[current].kind {
                    Kind::Host(i) => Some(i),
                    _ => None,
                };
                match ev {
                    FromHost::Sources(names) => {
                        let name = sources[current].name.clone();
                        sources.retain(|s| !matches!(s.kind, Kind::Host(_)));
                        let locals = sources.len();
                        sources.extend(
                            names.into_iter().enumerate().map(|(i, name)| Source { name, kind: Kind::Host(i as u8) }),
                        );
                        // The same log, asked for again (by its new number), if
                        // it is still there; else the first.
                        if shown.is_some() {
                            current = (locals..sources.len()).find(|&i| sources[i].name == name).unwrap_or(0);
                            switch = true;
                        }
                        frame_dirty = true;
                    }
                    FromHost::Start(i) if shown == Some(i) => {
                        text.clear();
                        view = View::new();
                        feed = Feed::Host(true);
                        more = true;
                    }
                    FromHost::Text(b) if matches!(feed, Feed::Host(true)) && shown.is_some() => {
                        text.push(&b);
                        more = true;
                    }
                    FromHost::Lost(e) if shown.is_some() => {
                        text.note(&format!("logs: the server: {e}; trying again"));
                        feed = Feed::Host(false);
                        more = true;
                    }
                    _ => {}
                }
            }
        }
        if more {
            view.settle(&text);
            log_dirty = true;
        }

        if frame_dirty {
            let s = &sources[current];
            let side = if matches!(s.kind, Kind::Host(_)) { "HOST" } else { "CHUMBY" };
            let place = format!("{side}  {} OF {}", current + 1, sources.len());
            let Ok(()) = view::draw_frame(&mut fb, &Bar { name: &s.name, place: &place }, view.follow);
            frame_dirty = false;
        }
        if log_dirty && (touched || drawn.elapsed() >= REDRAW) {
            let Ok(()) = view::draw_log(&mut fb, &text, &view);
            drawn = Instant::now();
            log_dirty = false;
        }
        neochumby::sleep_ms(TICK);
    }
}
