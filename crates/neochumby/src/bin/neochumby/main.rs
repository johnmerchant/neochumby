//! neochumby: what the Chumby boots to, run by the boot hook from the tree.
//! The session: it runs the apps' background services, restarting them
//! when they exit (services.rs), and runs one app at a time in front,
//! full-screen: first the start app (start.rs), then whatever the apps ask
//! for over their request pipe (neochumby::session), going back to the
//! start app whenever one exits (run.rs).
//!
//! A Chumby whose server is `serial` (an emulated one, with no network)
//! reaches its neochumbyd over its serial line, which the session shares
//! out to everything that connects (line.rs).
//!
//! On a paired Chumby, its output also goes to its neochumbyd, which can
//! ask it to restart, or for its screen, and, once its owner allows it,
//! tap its screen and press its top button (devlink.rs). A restart stops
//! everything, then runs the tree's `neochumby` in
//! this process's place: the newest tree, deployed since boot.
//!
//! Holding the top-left corner of the screen for three seconds, whatever
//! is running, ends the app in front and whatever it started, and opens
//! neochumby-home: the way back from an app that has gone wrong. The top
//! button is the session's, to hand over or to return to the start app
//! (button.rs).
//!
//!   neochumby                 the session
//!   neochumby sensors         prints the touchscreen, squeeze and
//!                             accelerometer
//!   neochumby check-set DIR   for the boot hook: exits 0 if the synced set
//!                             in DIR is complete (neochumby_sync)
//!   neochumby terminal ...    the session's own: shows a text-mode app's
//!                             terminal (terminal.rs)
//!
//! A text-mode app (one whose access doesn't list `fb`) runs on a terminal
//! the session shows (terminal.rs), in the font and colours the settings
//! name (`font=`, `colors=`), from the server or else /psp/neochumby.conf.
//!
//! With `delivery=sync` in its settings, the session also runs
//! neochumby-sync from the tree as a service of its own (restarted like
//! the apps' services), which keeps a copy of the tree on the stick for the
//! boot hook to run. A tree made for a stick has no neochumby-sync, and
//! needs none. The session also keeps the boot hook in
//! /psp up to date with the tree's (conf::refresh_hook), and neochumby-boot
//! beside it, which checks a tree over NFS before anything in it runs
//! (neochumby::verified). Started by neochumby-boot, the session runs from
//! a checked copy, and checks each program it runs too.
//!
//! Apps live beside it, in apps/<name>/ (neochumby::apps); each is its own
//! binary, paged in over NFS only when run. What it runs is described in
//! child.rs. The default player is chosen as the session starts
//! (player.rs), and so are the frontends the owner
//! approved (frontend.rs).

mod button;
mod child;
mod companion;
mod devlink;
mod frontend;
mod jail;
mod line;
mod network;
mod player;
mod run;
mod services;
mod start;
mod storage;
mod terminal;

use std::fs::File;
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use neochumby::apps::{self, App, Needs};
use neochumby::audio;
use neochumby::backlight;
use neochumby::conf;
use neochumby::fb::{Fb, rgb};
use neochumby::input::{self, Touch};
use neochumby::settings;
use neochumby::verified;
use neochumby_proto::{Delivery, Settings};

/// The escape hatch: a touch held this long in this corner. The resistive
/// touchscreen reports brief releases during a steady hold; one shorter
/// than ESCAPE_GRACE does not end it.
const ESCAPE_HOLD: Duration = Duration::from_secs(3);
const ESCAPE_GRACE: Duration = Duration::from_millis(400);
const ESCAPE_CORNER: i32 = 60;

fn sensors() {
    let mut touch = input::Touchscreen::open();
    let mut squeeze = input::Squeeze::open();
    let mut accel = input::Accel::open();
    let mut tick = 0u32;
    loop {
        for t in touch.poll() {
            println!("{t:?}");
        }
        if tick.is_multiple_of(10) {
            println!("squeeze={} accel={:?}", squeeze.squeezed(), accel.read());
        }
        tick += 1;
        neochumby::sleep_ms(30);
    }
}

/// Sets the flag when a touch is held in the top-left corner for
/// [`ESCAPE_HOLD`]. Input devices give every reader every event, so this
/// takes nothing from whatever else is reading the touchscreen.
fn watch_escape(flag: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut touch = input::Touchscreen::open();
        let mut held_since: Option<Instant> = None;
        let mut released_at: Option<Instant> = None;
        loop {
            for t in touch.poll() {
                match t {
                    Touch::Down { x, y, .. } if x < ESCAPE_CORNER && y < ESCAPE_CORNER => {
                        if held_since.is_none() {
                            eprintln!("neochumby: escape: held at ({x}, {y})");
                            held_since = Some(Instant::now());
                        }
                        released_at = None;
                    }
                    Touch::Down { .. } => {
                        held_since = None;
                        released_at = None;
                    }
                    Touch::Up => {
                        if held_since.is_some() {
                            released_at.get_or_insert_with(Instant::now);
                        }
                    }
                }
            }
            if released_at.is_some_and(|t| t.elapsed() > ESCAPE_GRACE) {
                held_since = None;
                released_at = None;
            }
            if released_at.is_none() && held_since.is_some_and(|t| t.elapsed() >= ESCAPE_HOLD) {
                eprintln!("neochumby: escape");
                flag.store(true, Ordering::SeqCst);
                held_since = None;
            }
            neochumby::sleep_ms(50);
        }
    });
}

/// How often the session tries the top button again while it hasn't got it
/// (another process had it at boot, say).
const BUTTON_RETRY: Duration = Duration::from_secs(5);

/// The top button, opened once for the whole session (button.rs): its input
/// device (input::open_button). `again` when retrying, which logs only success.
fn open_button(again: bool) -> Option<File> {
    // Passed on only to an app with `button = true`.
    let open =
        input::open_button().and_then(|(f, path)| neochumby_sys::fd::set_cloexec(f.as_fd(), true).map(|()| (f, path)));
    match open {
        Ok((f, path)) => {
            eprintln!("neochumby: top button at {path}");
            Some(f)
        }
        Err(e) => {
            if !again {
                eprintln!("neochumby: the top button: {e}; trying again every {} s", BUTTON_RETRY.as_secs());
            }
            None
        }
    }
}

/// The speaker as the session sets it for the apps: a master volume
/// (None: leave it), unmuted or not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Speaker {
    volume: Option<u32>,
    unmuted: bool,
}

impl Speaker {
    /// As one word for an atomic: the volume in the low byte (0xff for
    /// none), unmuted in bit 8.
    fn word(self) -> u32 {
        self.volume.map_or(0xff, |v| v.min(100)) | u32::from(self.unmuted) << 8
    }

    fn from_word(w: u32) -> Speaker {
        Speaker { volume: Some(w & 0xff).filter(|v| *v <= 100), unmuted: w & 0x100 != 0 }
    }
}

/// What the volume thread is asked for and has done, as [`Speaker::word`]s.
/// audio::set_volume runs chumby_set_volume, which the loop
/// mustn't wait on, and one thread applying the latest keeps the order.
struct Volumes {
    want: AtomicU32,
    done: AtomicU32,
}

impl Volumes {
    /// Starts the thread; nothing is asked of it yet.
    fn start() -> Arc<Volumes> {
        let v = Arc::new(Volumes { want: AtomicU32::new(u32::MAX), done: AtomicU32::new(u32::MAX) });
        let t = v.clone();
        std::thread::spawn(move || {
            loop {
                let w = t.want.load(Ordering::SeqCst);
                if w == t.done.load(Ordering::SeqCst) {
                    neochumby::sleep_ms(20);
                    continue;
                }
                let s = Speaker::from_word(w);
                if let Some(v) = s.volume
                    && let Err(e) = audio::set_volume(v)
                {
                    eprintln!("neochumby: setting the volume: {e}");
                }
                // Unmuted only if that is still what's wanted: a reset that
                // came while the volume was being set has muted it.
                let mute = !(s.unmuted && t.want.load(Ordering::SeqCst) == w);
                match audio::mute(mute) {
                    Ok(()) => eprintln!(
                        "neochumby: the speaker: {}, volume {:?}",
                        if mute { "muted" } else { "unmuted" },
                        s.volume
                    ),
                    Err(e) => eprintln!("neochumby: the speaker's mute: {e}"),
                }
                t.done.store(w, Ordering::SeqCst);
            }
        });
        v
    }
}

/// A stream player, which hands a URL to its daemon (btplayd), where there
/// is one: the image has none yet, and the attempt is logged.
const BTPLAY: &str = "/usr/bin/btplay";

/// Runs the player with `args`: no shell, so a URL is one argument
/// and nothing else. It may not return soon, so a thread waits for it: the
/// loop never does. Its daemon outlives it, by design; it stays out of the
/// apps' process groups and holds none of the session's descriptors
/// (they're close-on-exec).
fn btplay(args: Vec<String>) {
    let mut cmd = Command::new(BTPLAY);
    cmd.args(&args).current_dir("/").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.process_group(0);
    std::thread::spawn(move || match cmd.status() {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("neochumby: {BTPLAY} {}: {s}", args.join(" ")),
        Err(e) => eprintln!("neochumby: {BTPLAY}: {e}"),
    });
}

/// The Chumby, as the session's loop sees it.
struct Chumby {
    escape: Arc<AtomicBool>,
    boot_volume: Option<u32>,
    speaker: Arc<Volumes>,
    button: Option<File>,
    /// When the session last tried to open the button, while it hasn't got
    /// it; None once it has closed it for good.
    button_tried: Option<Instant>,
    /// Set by the developer link.
    restart: Option<Arc<AtomicBool>>,
    /// The top button as neochumbyd holds it, through the developer link.
    remote: Option<Arc<devlink::Remote>>,
    /// The backlight, if the kernel has one.
    backlight: Option<backlight::Backlight>,
    /// The backlight as the Chumby booted with it.
    boot_brightness: Option<u32>,
    /// The tree, and the owner's font and colours, for terminals.
    root: std::path::PathBuf,
    look: terminal::Look,
}

impl run::Device for Chumby {
    fn escaped(&mut self) -> bool {
        self.escape.swap(false, Ordering::SeqCst)
    }

    /// Mutes at once; the volume thread puts the boot's
    /// volume back. As the session ends, it waits for that, up to `wait`.
    fn reset_speaker(&mut self, wait: Duration) {
        if let Err(e) = audio::mute(true) {
            eprintln!("neochumby: muting the speaker: {e}");
        }
        let want = Speaker { volume: self.boot_volume, unmuted: false }.word();
        self.speaker.want.store(want, Ordering::SeqCst);
        let deadline = Instant::now() + wait;
        while self.speaker.done.load(Ordering::SeqCst) != want && Instant::now() < deadline {
            neochumby::sleep_ms(20);
        }
    }

    fn unmute(&mut self, volume: u32) {
        self.speaker.want.store(Speaker { volume: Some(volume), unmuted: true }.word(), Ordering::SeqCst);
    }

    fn speaker_holder(&self) -> Option<u32> {
        audio::holder()
    }

    fn button(&mut self) -> Option<bool> {
        if self.button.is_none()
            && let Some(t) = self.button_tried
            && t.elapsed() >= BUTTON_RETRY
        {
            self.button = open_button(true);
            self.button_tried = Some(Instant::now());
        }
        let real = self.button.as_mut().and_then(input::button_level);
        // Read only while the app in front hasn't the button (run.rs), so
        // a press from neochumbyd can reach it.
        let remote = self.remote.as_ref().map(|r| {
            r.handed_over.store(false, Ordering::SeqCst);
            r.button.load(Ordering::SeqCst)
        });
        button::level(real, remote)
    }

    /// Asked for only as an app with `button = true` opens (run.rs): from
    /// then until the button is read again, neochumbyd can't press it.
    fn button_fd(&self) -> Option<RawFd> {
        if let Some(r) = &self.remote {
            r.handed_over.store(true, Ordering::SeqCst);
        }
        self.button.as_ref().map(|f| f.as_raw_fd())
    }

    fn close_button(&mut self) {
        self.button = None;
        self.button_tried = None;
    }

    fn restart(&mut self) -> bool {
        self.restart.as_ref().is_some_and(|r| r.swap(false, Ordering::SeqCst))
    }

    /// One write to /sys, so the loop does it.
    fn brightness(&mut self, level: Option<u32>) {
        let Some(v) = level.or(self.boot_brightness) else { return };
        let Some(b) = &self.backlight else {
            eprintln!("neochumby: no backlight to set to {v}");
            return;
        };
        match b.set(v) {
            Ok(()) => eprintln!("neochumby: the backlight: {v} ({})", b.path().display()),
            Err(e) => eprintln!("neochumby: {}: {e}", b.path().display()),
        }
    }

    fn play(&mut self, url: Option<&str>) {
        match url {
            Some(u) => btplay(vec!["--start-daemon".into(), u.into()]),
            None => btplay(vec!["--passthru=stop".into()]),
        }
    }

    fn speaker_group(&self) -> Option<u32> {
        audio::holder().and_then(neochumby_sys::process::group_of)
    }

    fn terminal(&mut self, app: &App) -> std::io::Result<Option<(terminal::Terminal, File)>> {
        terminal::open(app, &self.root, &self.look).map(Some)
    }
}

fn session() {
    // On a Chumby with no network, its serial line to neochumbyd: before
    // anything connects there.
    line::start();
    // First, so that it carries everything the session logs.
    let link = devlink::Link::start();
    let root = apps::root();
    let all = apps::all(&root);
    jail::init(&all);
    match conf::refresh_hook(&root) {
        Ok(true) => eprintln!("neochumby: {} brought up to date from the tree", conf::HOOK),
        Ok(false) => {}
        Err(e) => eprintln!("neochumby: {}: {e}", conf::HOOK),
    }
    // As the hook is, and from the same trees: what checks a tree over NFS
    // on the next boot.
    if conf::paired().is_some() && root != Path::new(conf::STICK_TREE) {
        match verified::install_boot(&root) {
            Ok(true) => eprintln!("neochumby: {} brought up to date from the tree", verified::BOOT),
            Ok(false) => {}
            Err(e) => eprintln!("neochumby: {}: {e}", verified::BOOT),
        }
    }
    if verified::on() {
        eprintln!("neochumby: the tree is checked against neochumbyd's signed manifest");
    }

    // The master volume as the boot set it from /psp/volume: nothing has
    // played yet.
    let boot_volume = audio::volume();

    // The server's `start` and `player`, else the local ones: names, trusted
    // only as far as the tree goes (start.rs, player.rs).
    let settings = settings::load();
    let core = core_services(&root, &settings);
    let (local, local_player) = (conf::get(conf::CONF, "start"), conf::get(conf::CONF, "player"));
    let choice = chosen(settings.start.as_deref(), local.as_deref());
    let player = player::choose(&all, chosen(settings.player.as_deref(), local_player.as_deref()));
    match player {
        Some(m) => eprintln!("neochumby: the player is {}", m.name),
        None => eprintln!("neochumby: no player here (player = true in a manifest)"),
    }
    // The owner's font and colours for text-mode apps, as `start` is
    // chosen: names, checked as each terminal opens.
    let setting = |key: &str| {
        let server = settings.other.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        server.or_else(|| conf::get(conf::CONF, key))
    };
    let look = terminal::Look { font: setting("font"), colors: setting("colors") };
    // The frontends the owner approved (`frontend_<app>=`), as `start` is
    // chosen: names, trusted only as far as the tree goes (frontend.rs).
    let approved = frontend::resolve(&all, |key| {
        let server = settings.other.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        let local = conf::get(conf::CONF, key);
        chosen(server, local.as_deref()).map(|(from, v)| (from, v.to_string()))
    });
    frontend::init(&all, approved);
    // The services' sockets, before any app runs (frontend.rs).
    frontend::bind(&all);
    // On a kernel with a console on the screen, its cursor would blink over
    // what apps draw.
    neochumby::term::vt::quiet();
    let escape = Arc::new(AtomicBool::new(false));
    watch_escape(escape.clone());

    let ended = match start::choose(&all, choice) {
        None => {
            eprintln!("neochumby: no start app (not even {})", start::HOME);
            run::Ended::GaveUp
        }
        Some(first) => {
            let backlight = backlight::Backlight::find();
            if backlight.is_none() {
                eprintln!("neochumby: no backlight (/sys/class/backlight)");
            }
            let chumby = Chumby {
                escape,
                boot_volume,
                speaker: Volumes::start(),
                button: open_button(false),
                button_tried: Some(Instant::now()),
                restart: link.as_ref().map(devlink::Link::restart_flag),
                remote: link.as_ref().map(devlink::Link::remote),
                boot_brightness: backlight.as_ref().and_then(backlight::Backlight::level),
                backlight,
                root: root.clone(),
                look,
            };
            // On either ending, the loop has closed the button, stopped the
            // services (neochumby-sync with them) and put the speaker back.
            run::Session::new(&all, &core, &root, first, player, chumby, run::Timing::default()).run()
        }
    };

    if ended == run::Ended::Restart {
        // Everything this process holds is close-on-exec (std opens files
        // and sockets so, and the request pipes are marked so), so the new
        // session starts with what the boot hook gave this one.
        if let Some(link) = link {
            link.release();
        }
        // Just after a deploy, the NFS client may still hold the replaced
        // file's handle: the first try fails as stale, and drops it. A
        // checked tree is checked again, from its new manifest.
        let exe = root.join("neochumby");
        for _ in 0..5 {
            let e = if verified::on() { verified::boot(&root) } else { Command::new(&exe).exec() };
            eprintln!("neochumby: restart: {}: {e}", exe.display());
            neochumby::sleep_ms(1000);
        }
        eprintln!("neochumby: restart failed");
    }
    if let Ok(mut fb) = Fb::open() {
        fb.clear(rgb(0, 0, 0));
    }
}

/// A setting from the server, else `local` (from /psp/neochumby.conf), with
/// where it came from.
fn chosen<'a>(server: Option<&'a str>, local: Option<&'a str>) -> Option<(&'static str, &'a str)> {
    match server {
        Some(s) => Some(("the server's settings", s)),
        None => local.map(|s| (conf::CONF, s)),
    }
}

/// The session's own services, supervised like the apps' (services.rs) but
/// no app's: neochumby-sync, when this Chumby's settings say
/// `delivery=sync`. It opens no app, so it is never told what is in front
/// and nothing it asks for is done.
fn core_services(root: &Path, settings: &Settings) -> Vec<App> {
    if settings.delivery != Some(Delivery::Sync) {
        return Vec::new();
    }
    vec![App {
        dir: root.to_path_buf(),
        name: "neochumby-sync".into(),
        title: None,
        exec: None,
        color: 0,
        service: Some(root.join("neochumby-sync")),
        needs: Needs::Required,
        start: false,
        interrupt: false,
        button: false,
        // It mounts the stick.
        companion: false,
        access: neochumby::apps::Access { root: true, ..Default::default() },
        player: false,
        font: None,
        tap: Vec::new(),
        frontend: None,
        wasm: None,
        catalog: false,
    }]
}

/// `neochumby check-set DIR`: whether the set in DIR (the stick's
/// neochumby/a or /b) is complete. The boot hook runs it from the set
/// itself, and runs the set only on exit 0.
fn check_set(dir: Option<String>) -> ! {
    let Some(dir) = dir else {
        eprintln!("usage: neochumby check-set DIR");
        std::process::exit(2)
    };
    match neochumby_sync::stick::check_set(Path::new(&dir), &neochumby_sync::LIMITS) {
        Ok(m) => {
            eprintln!("neochumby: {dir}: complete, {} files", m.len());
            std::process::exit(0)
        }
        Err(e) => {
            eprintln!("neochumby: {dir}: not complete: {e}");
            std::process::exit(1)
        }
    }
}

fn main() {
    neochumby::debug::install("neochumby");
    match std::env::args().nth(1).as_deref() {
        None => session(),
        Some("sensors") => sensors(),
        Some("check-set") => check_set(std::env::args().nth(2)),
        Some("terminal") => terminal::serve(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some(other) => {
            eprintln!("neochumby: unknown command {other}");
            std::process::exit(2);
        }
    }
}
