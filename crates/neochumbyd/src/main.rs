//! neochumbyd: what a Chumby's stick is paired with. It keeps a copy of the
//! app tree on each Chumby's stick (sync.rs), and runs the server half of
//! their apps (companion.rs)
//! over neochumby-proto, and the apps' once-per-server server programs
//! (server.rs), which may reach it through an admin socket (admin.rs).
//! Both run sandboxed (confine.rs). It knows no app by name: an app brings
//! its own companion and server program, in its directory of the tree.
//!
//!   neochumbyd serve [options]    the daemon
//!   neochumbyd keygen FILE        a new Chumby key: its private half, in FILE
//!                                 alone (tools/stick has it write the stick
//!                                 directly); prints its public half. A FILE
//!                                 that is there already is left, and read
//!   neochumbyd add NAME KEY [--replace] [--address ADDR] [--api ADDR]
//!                                 registers a Chumby's public key under NAME
//!                                 and prints what its stick needs: server=,
//!                                 api_port= and server_key=. One already
//!                                 registered under NAME with another key is
//!                                 replaced only with --replace
//!   neochumbyd list               the Chumbys registered, GET APPS requests
//!                                 waiting, and the start and players in the
//!                                 tree (tree.rs)
//!   neochumbyd approve CODE
//!   neochumbyd refuse CODE        a code of eight letters (ABCD-EF23) is a
//!                                 Chumby's GET APPS asking to install or
//!                                 remove a catalog app: approve shows what
//!                                 it asks and lets it go ahead
//!                                 (catalog/chumby.rs)
//!   neochumbyd forget NAME        revokes that stick's key at once
//!   neochumbyd set NAME KEY=VALUE  one of a Chumby's settings (`start=wx`),
//!                                  sent to it when it next connects;
//!                                  KEY= removes it. For `start` and
//!                                  `player` (the default player), it
//!                                  says which apps in the tree may be
//!                                  named
//!   neochumbyd manifest            what a sync sends: the tree's files,
//!                                  and any a Chumby could not take
//!   neochumbyd log NAME [-f]       a paired Chumby's log, as its session
//!                                  sent it; -f follows it (dev.rs)
//!   neochumbyd sync NAME           asks it for a sync round now, and waits
//!                                  for the round to end. Exits 3 if nothing
//!                                  can be written until it restarts
//!   neochumbyd restart NAME        its session ends its apps and starts
//!                                  again from its stick's live set
//!   neochumbyd shot NAME OUT.png   its screen
//!   neochumbyd tap NAME X Y [--hold MS]
//!                                  touches its screen at (X, Y), in
//!                                  pixels, for MS (default 100, at most
//!                                  10000), as a finger would
//!   neochumbyd button NAME press|release|tap
//!                                  its top button: down, up, or both.
//!                                  Both of these only once its owner has
//!                                  run `neochumbyd set NAME dev=on`
//!   neochumbyd mcp                 log, sync, restart, shot, set and list
//!                                  as an MCP server on stdio, for
//!                                  Claude Code and the like (mcp.rs)
//!   neochumbyd catalog [refresh|log] [--json]
//!                                  the catalog's apps, from its cache
//!                                  (which `serve` refreshes daily), or
//!                                  asked of crates.io and GitHub now; or
//!                                  its log of installs and updates
//!   neochumbyd catalog auto-update [on|off]
//!                                  whether catalog apps update by
//!                                  themselves when the new version asks
//!                                  for nothing more (on unless turned off)
//!   neochumbyd install crate:NAME@VERSION [--yes --access WORDS]
//!                      [--wamrc PATH]
//!                                  a catalog app into the tree, checked,
//!                                  shown and approved first, through the
//!                                  admin socket's protocol with the
//!                                  owner's token (catalog/, admin.rs)
//!   neochumbyd uninstall NAME      a catalog app out of it, likewise
//!   neochumbyd token [new]        the owner's token, which the admin
//!                                  protocol needs; `new` replaces it
//!
//! and, not for typing, `neochumbyd wasm-companion`: the process a wasm
//! companion runs in, under Wasmtime, sandboxed (wasm.rs). Only a
//! neochumbyd built with the `wasm-companions` cargo feature has it, as
//! release builds are; without, it runs no wasm companion, and says so.
//!
//! Options (each command takes --state):
//!   --state DIR        default /var/lib/neochumbyd
//!   --tree DIR         the app tree to serve, default <state>/tree
//!   --api ADDR         default 0.0.0.0:7340
//!   --wamrc PATH       a wamrc built by tools/wamrc (else NEOCHUMBY_WAMRC):
//!                      catalog apps are compiled for the Chumby on
//!                      install, and again when it compiles for another
//!                      WAMR; without one they run interpreted
//!   --no-catalog-refresh
//!                      `serve` never asks crates.io or GitHub for the
//!                      catalog by itself (`neochumbyd catalog refresh`
//!                      still does), so nor does it update catalog apps
//!   --app-uids FIRST-LAST
//!                      started as root, neochumbyd becomes the user that
//!                      owns DIR, and runs each app's companions and server
//!                      program under a uid of the app's own from this
//!                      range (isolate.rs), in this system's sandbox too
//!                      where it has one
//!   --unsandboxed      run apps' server programs and companions without
//!                      a sandbox, where there is none (neither this
//!                      system's nor --app-uids; confine.rs); never as root
//!   --listen-unix PATH also takes Chumbys on serial lines at this Unix
//!                      socket: an emulated Chumby's, which has no
//!                      network (line.rs)

mod admin;
mod api;
mod catalog;
mod companion;
mod confine;
mod dev;
#[cfg(feature = "wasm-companions")]
mod egress;
mod isolate;
mod line;
mod mcp;
mod png;
mod server;
mod state;
mod sync;
mod tree;
#[cfg(feature = "wasm-companions")]
mod wasm;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use neochumby_proto::Msg;
use neochumby_proto::dev::{BUTTON_TAP_MS, MAX_HOLD_MS};
use neochumby_proto::settings::key;

use state::State;

/// How `neochumbyd sync` says that a restart must come first.
const NEEDS_RESTART: i32 = 3;
/// How long `restart`, `sync`, `shot`, `tap` and `button` wait for the Chumby's
/// developer link: long enough for it to come back after the daemon itself
/// restarted (a new binary deployed), as its session reconnects within a
/// few seconds.
const LINK_WAIT: Duration = Duration::from_secs(15);
/// How long `tap` holds without --hold.
const TAP_MS: u16 = 100;

struct Args {
    command: String,
    operand: Option<String>,
    setting: Option<String>,
    follow: bool,
    replace: bool,
    /// The operands after NAME, for `tap` and `button`.
    rest: Vec<String>,
    hold: Option<String>,
    wamrc: Option<PathBuf>,
    state: PathBuf,
    tree: Option<PathBuf>,
    address: Option<String>,
    api: String,
    app_uids: Option<String>,
    unsandboxed: bool,
    listen_unix: Option<PathBuf>,
    /// `--no-catalog-refresh`.
    no_catalog_refresh: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: neochumbyd serve|keygen FILE|add NAME KEY [--replace] [--address ADDR]|list|approve CODE|refuse CODE|forget NAME|set NAME KEY=VALUE|manifest|log NAME [-f]|sync NAME|restart NAME|shot NAME OUT.png|mcp|tap NAME X Y [--hold MS]|button NAME press|release|tap|token [new]|catalog [refresh] [--json]|install crate:NAME@VERSION [--yes --access WORDS]|uninstall NAME [--state DIR] [--tree DIR] [--api ADDR] [--app-uids FIRST-LAST] [--unsandboxed] [--listen-unix PATH] [--wamrc PATH] [--no-catalog-refresh]"
    );
    std::process::exit(2)
}

fn args() -> Args {
    let mut a = Args {
        command: String::new(),
        operand: None,
        setting: None,
        follow: false,
        replace: false,
        rest: Vec::new(),
        hold: None,
        wamrc: None,
        state: "/var/lib/neochumbyd".into(),
        tree: None,
        address: None,
        api: format!("0.0.0.0:{}", neochumby_proto::API_PORT),
        app_uids: None,
        unsandboxed: false,
        listen_unix: None,
        no_catalog_refresh: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--state" => a.state = value().into(),
            "--tree" => a.tree = Some(value().into()),
            "--address" => a.address = Some(value()),
            "--api" => a.api = value(),
            "--listen-unix" => a.listen_unix = Some(value().into()),
            "-f" | "--follow" => a.follow = true,
            "--replace" => a.replace = true,
            "--hold" => a.hold = Some(value()),
            "--wamrc" => a.wamrc = Some(value().into()),
            "--app-uids" => a.app_uids = Some(value()),
            "--unsandboxed" => a.unsandboxed = true,
            "--no-catalog-refresh" => a.no_catalog_refresh = true,
            _ if arg.starts_with("--") => usage(),
            _ if a.command.is_empty() => a.command = arg,
            _ if a.operand.is_none() => a.operand = Some(arg),
            _ if matches!(a.command.as_str(), "set" | "shot" | "add") && a.setting.is_none() => a.setting = Some(arg),
            _ if matches!(a.command.as_str(), "tap" | "button") => a.rest.push(arg),
            _ => usage(),
        }
    }
    a
}

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("neochumbyd: {e}");
    std::process::exit(1)
}

fn main() {
    // The catalog's commands take options of their own (catalog/).
    if let Some(c) = std::env::args().nth(1).filter(|c| catalog::COMMANDS.contains(&c.as_str())) {
        catalog::main(&c, std::env::args().skip(2).collect());
    }
    // Started by companion.rs, with arguments of its own.
    if std::env::args().nth(1).as_deref() == Some("wasm-companion") {
        #[cfg(feature = "wasm-companions")]
        wasm::main(std::env::args().skip(2));
        #[cfg(not(feature = "wasm-companions"))]
        fail(companion::no_wasm_companions());
    }
    let a = args();
    // A key for a stick: on the machine making it, which has no state.
    if a.command == "keygen" {
        let file = a.operand.clone().unwrap_or_else(|| usage());
        match keygen(file.as_ref()) {
            Ok(key) => println!("{}", neochumby_proto::hex(&key)),
            Err(e) => fail(format!("{file}: {e}")),
        }
        return;
    }
    // Before anything is written or any thread started: with --app-uids,
    // neochumbyd started as root becomes its own user here.
    let confine = (a.command == "serve")
        .then(|| confine::Confine::choose(&a.state, a.app_uids.as_deref(), a.unsandboxed).unwrap_or_else(|e| fail(e)));
    if (a.app_uids.is_some() || a.unsandboxed) && confine.is_none() {
        usage();
    }
    let state = State::open(a.state.clone()).unwrap_or_else(|e| fail(format!("{}: {e}", a.state.display())));
    let operand = || a.operand.clone().unwrap_or_else(|| usage());
    let tree_dir = a.tree.clone().unwrap_or_else(|| a.state.join("tree"));
    match a.command.as_str() {
        "serve" => serve(&a, state, confine.expect("chosen for serve")),
        "add" => {
            let name = operand();
            let key = a.setting.clone().unwrap_or_else(|| usage());
            let key = neochumby_proto::unhex_key(&key).unwrap_or_else(|e| fail(format!("{key}: {e}")));
            state.add(&name, &key, a.replace).unwrap_or_else(|e| fail(e));
            let server = state.public_key().unwrap_or_else(|e| fail(format!("identity: {e}")));
            if let Some(address) = &a.address {
                println!("server={address}");
            }
            println!("api_port={}", a.api.rsplit(':').next().unwrap_or_default());
            println!("server_key={}", neochumby_proto::hex(&server));
        }
        "list" => {
            let (starts, players) = (tree::start_apps(&tree_dir), tree::players(&tree_dir));
            for (name, key) in state.pairs().unwrap_or_else(|e| fail(e)) {
                let pairs: Vec<String> = state.config(&name).iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("chumby   {name}  {key}  {}", pairs.join(" "));
                let settings = state.settings(&name);
                let notes = [
                    tree::start_note(&name, settings.start.as_deref(), &starts),
                    tree::player_note(&name, settings.player.as_deref(), &players),
                ];
                for note in notes.into_iter().flatten() {
                    println!("         {note}");
                }
            }
            for r in catalog::chumby::requests(&a.state) {
                println!("asking   {}  code {}  to {}", r.chumby, r.code, r.what);
            }
            println!("{}", tree::hint(&starts));
            println!("{}", tree::player_hint(&players));
        }
        // A Chumby's GET APPS asking: through the admin protocol, with the
        // owner's token.
        "approve" | "refuse" => {
            if catalog::chumby::parse_code(&operand()).is_none() {
                fail("not a request's code: the eight letters the Chumby shows, ABCD-EF23");
            }
            let token = state.owner_token().unwrap_or_else(|e| fail(format!("the owner's token: {e}")));
            let line = format!("{} {} {token}", a.command, operand());
            let answer = admin::local(&state, &line).unwrap_or_else(|e| fail(e));
            let (chumby, summary) = answer.split_last().unwrap_or_else(|| fail("no answer"));
            for l in summary {
                println!("{}", l.strip_prefix("summary ").unwrap_or(l));
            }
            println!(
                "{} {}'s request {}",
                if a.command == "approve" { "approved" } else { "refused" },
                chumby,
                operand()
            );
        }
        "forget" => state.forget(&operand()).unwrap_or_else(|e| fail(e)),
        "set" => {
            let setting = a.setting.clone().unwrap_or_else(|| usage());
            let (key, value) = setting.split_once('=').unwrap_or_else(|| usage());
            state.set(&operand(), key, value).unwrap_or_else(|e| fail(e));
            if key == key::START {
                let starts = tree::start_apps(&tree_dir);
                if !value.is_empty() && !starts.iter().any(|s| s == value) {
                    eprintln!("neochumbyd: {value} isn't a start app in the tree; the Chumby will open {}", tree::HOME);
                }
                println!("{}", tree::hint(&starts));
            }
            if key == key::PLAYER {
                let players = tree::players(&tree_dir);
                if let Some(note) = tree::player_note(&operand(), Some(value).filter(|v| !v.is_empty()), &players) {
                    eprintln!("neochumbyd: {note}");
                }
                println!("{}", tree::player_hint(&players));
            }
            if key == key::DEV {
                let name = operand();
                if state.settings(&name).dev {
                    println!("{name} now takes taps and button presses from here (`neochumbyd tap`, `button`)");
                } else {
                    println!("{name} takes no taps or button presses from here");
                }
            }
        }
        "manifest" => manifest(&a),
        "log" => {
            let name = paired(&state, &operand());
            let mut out = std::io::stdout().lock();
            let r = if a.follow {
                dev::follow(&a.state, &name, &mut out, || false)
            } else {
                dev::read_log(&a.state, &name).and_then(|b| std::io::Write::write_all(&mut out, &b))
            };
            r.unwrap_or_else(|e| fail(format!("{name}: {e}")));
        }
        "sync" => {
            let name = paired(&state, &operand());
            match dev::sync(&a.state, &name, LINK_WAIT).unwrap_or_else(|e| fail(format!("{name}: {e}"))) {
                dev::Synced::Done(what) => println!("{name}: {what}"),
                dev::Synced::NeedsRestart(why) => {
                    eprintln!("neochumbyd: {name} needs a restart before it can sync: {why}");
                    std::process::exit(NEEDS_RESTART);
                }
            }
        }
        "restart" => {
            let name = paired(&state, &operand());
            dev::restart(&a.state, &name, LINK_WAIT).unwrap_or_else(|e| fail(e));
            println!("{name}: restarting");
        }
        "shot" => {
            let name = paired(&state, &operand());
            let out = a.setting.clone().unwrap_or_else(|| usage());
            dev::shot(&a.state, &name, out.as_ref(), LINK_WAIT).unwrap_or_else(|e| fail(e));
            println!("{out}");
        }
        "mcp" => {
            let server = mcp::Server { state, state_dir: a.state.clone(), tree: tree_dir, wait: LINK_WAIT };
            mcp::run(&server, std::io::stdin().lock(), std::io::stdout().lock()).unwrap_or_else(|e| fail(e));
        }
        "tap" | "button" => {
            let name = paired(&state, &operand());
            let msgs = input(&a).unwrap_or_else(|e| fail(e));
            if !state.settings(&name).dev {
                fail(dev::input_off(&name));
            }
            for (i, m) in msgs.iter().enumerate() {
                if i > 0 {
                    std::thread::sleep(Duration::from_millis(BUTTON_TAP_MS));
                }
                dev::input(&a.state, &name, m, LINK_WAIT).unwrap_or_else(|e| fail(e));
            }
            println!("{name}: {}", a.rest.join(" "));
        }
        "token" => {
            let token = match a.operand.as_deref() {
                None => state.owner_token(),
                Some("new") => state.new_owner_token(),
                Some(_) => usage(),
            };
            println!("{}", token.unwrap_or_else(|e| fail(e)));
        }
        _ => usage(),
    }
}

/// What `tap` or `button` asks of the Chumby, in order.
fn input(a: &Args) -> Result<Vec<Msg>, String> {
    let n = |s: &str, most: u16, what: &str| match s.parse::<u16>() {
        Ok(v) if v <= most => Ok(v),
        _ => Err(format!("{what} is 0 to {most}, not {s}")),
    };
    // The Chumby Classic's screen; a Chumby refuses what's off its own.
    let (w, h) = (320, 240);
    match (a.command.as_str(), &a.rest[..]) {
        ("tap", [x, y]) => {
            let hold_ms = match &a.hold {
                Some(ms) => n(ms, MAX_HOLD_MS, "--hold")?,
                None => TAP_MS,
            };
            Ok(vec![Msg::Tap { x: n(x, w - 1, "X")?, y: n(y, h - 1, "Y")?, hold_ms }])
        }
        ("button", [b]) if a.hold.is_none() => match b.as_str() {
            "press" => Ok(vec![Msg::Button { down: true }]),
            "release" => Ok(vec![Msg::Button { down: false }]),
            "tap" => Ok(vec![Msg::Button { down: true }, Msg::Button { down: false }]),
            _ => usage(),
        },
        _ => usage(),
    }
}

/// What a sync sends, as a set's manifest, then what is left out of it and
/// the manifest's digest.
fn manifest(a: &Args) {
    let tree = a.tree.clone().unwrap_or_else(|| a.state.join("tree"));
    let hashes = neochumby_sync::serve::Hashes::default();
    let tree = neochumby_sync::serve::scan(&tree, &hashes).unwrap_or_else(|e| fail(e));
    let m = tree.manifest();
    for e in &m {
        print!("{}", neochumby_sync::manifest::line(e));
    }
    for l in &tree.left_out {
        eprintln!("left out: {l}");
    }
    if let Err(e) = neochumby_sync::manifest::validate(&m, &neochumby_sync::LIMITS) {
        eprintln!("a Chumby would refuse this tree: {e}");
    }
    eprintln!("digest {}", neochumby_proto::hex(&neochumby_sync::manifest::digest(&m)));
}

/// A new key in `file`, written there and nowhere else, and synced: its
/// public half. If `file` is there, its key's public half, and nothing is
/// written: a stick made again keeps its identity.
fn keygen(file: &std::path::Path) -> std::io::Result<neochumby_proto::PublicKey> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Ok(s) = std::fs::read_to_string(file) {
        return Ok(neochumby_proto::Identity::from_secret(&s)?.public());
    }
    let id = neochumby_proto::Identity::generate()?;
    let tmp = file.with_extension("key.new");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    let r = f.write_all(id.secret().as_bytes()).and_then(|()| f.sync_all()).and_then(|()| std::fs::rename(&tmp, file));
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    neochumby_sys::fs::sync();
    Ok(id.public())
}

/// `name`, if a Chumby is registered under it.
fn paired(state: &State, name: &str) -> String {
    if !state.pairs().unwrap_or_else(|e| fail(e)).iter().any(|(n, _)| n == name) {
        fail(format!("no Chumby registered as {name}"));
    }
    name.to_string()
}

fn serve(a: &Args, state: State, confine: confine::Confine) {
    // Nothing it makes is for other users: apps' programs may be other
    // users (--app-uids).
    neochumby_sys::privilege::add_to_umask(0o007);
    state.secure().unwrap_or_else(|e| fail(format!("closing the state directory to other users: {e}")));
    let identity = state.identity().unwrap_or_else(|e| fail(format!("identity: {e}")));
    state.publish_key(&identity.public()).unwrap_or_else(|e| fail(format!("identity.pub: {e}")));
    let tree = a.tree.clone().unwrap_or_else(|| a.state.join("tree"));
    eprintln!(
        "neochumbyd: syncing {}, the API on {}; key {}",
        tree.display(),
        a.api,
        neochumby_proto::hex(&identity.public())
    );

    // Made now, so the owner can read it before the web page asks.
    state.owner_token().unwrap_or_else(|e| fail(format!("owner token: {e}")));
    // An admin socket left by an earlier neochumbyd (they had paths then).
    let _ = std::fs::remove_file(a.state.join("admin.sock"));
    dev::no_rounds_run(&a.state);

    let listener = TcpListener::bind(&a.api).unwrap_or_else(|e| fail(format!("{}: {e}", a.api)));
    let lines = a.listen_unix.as_ref().map(|p| {
        let l = line::listen(p).unwrap_or_else(|e| fail(format!("{}: {e}", p.display())));
        eprintln!("neochumbyd: serial lines on {}", p.display());
        l
    });
    let api = Arc::new(api::Api {
        state,
        identity,
        tree: tree.clone(),
        state_dir: a.state.clone(),
        hashes: neochumby_sync::serve::Hashes::default(),
        links: Default::default(),
        confine: confine.clone(),
    });
    let limits = Arc::new(api::Limits::default());
    if let Some(l) = lines {
        let (api, limits) = (api.clone(), limits.clone());
        std::thread::spawn(move || line::serve(l, api, limits));
    }
    std::thread::spawn(move || api::serve_with(listener, api, limits));
    catalog::serve(a.state.clone(), tree.clone(), a.wamrc.clone(), !a.no_catalog_refresh);

    let term = neochumby_sys::signal::catch_term();
    // Last, so nothing above can fail with server programs running.
    let servers = server::start(&tree, &a.state, &confine);
    while !term.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }
    // Stopping, on SIGTERM or SIGINT: the server programs first.
    eprintln!("neochumbyd: stopping");
    servers.stop();
}
