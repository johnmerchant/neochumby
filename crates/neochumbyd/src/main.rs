//! neochumbyd: what a Chumby pairs with. Serves the app tree over NFS, or
//! keeps a copy of it on the Chumby's stick (`delivery=sync`, sync.rs),
//! pairs Chumbys, and runs the server half of their apps (companion.rs)
//! over neochumby-proto, and the apps' once-per-server server programs
//! (server.rs), which may reach it through an admin socket (admin.rs).
//! Both run sandboxed (confine.rs). It knows no app by name: an app brings
//! its own companion and server program, in its directory of the tree.
//!
//!   neochumbyd serve [options]    the daemon
//!   neochumbyd pairing            Chumbys paired and asking to pair, and
//!                                 the start and players in the tree
//!                                 (tree.rs)
//!   neochumbyd approve CODE [--replace]
//!                                 pair the Chumby showing CODE (its 32
//!                                 digits, typed from its screen); one
//!                                 already paired under its name with
//!                                 another key is unpaired only with
//!                                 --replace
//!   neochumbyd refuse CODE
//!                                 a code of eight letters (ABCD-EF23) is
//!                                 a Chumby's GET APPS asking to install
//!                                 or remove a catalog app: approve shows
//!                                 what it asks and lets it go ahead
//!                                 (catalog/chumby.rs)
//!   neochumbyd forget NAME
//!   neochumbyd set NAME KEY=VALUE  one of a Chumby's settings (`start=wx`),
//!                                  sent to it when it next connects;
//!                                  KEY= removes it. For `start` and
//!                                  `player` (the default player), it
//!                                  says which apps in the tree may be
//!                                  named;
//!                                  `delivery=sync` keeps the tree on the
//!                                  Chumby's stick
//!   neochumbyd manifest            what a sync sends: the tree's files,
//!                                  and any a Chumby could not take
//!   neochumbyd log NAME [-f]       a paired Chumby's log, as its session
//!                                  sent it; -f follows it (dev.rs)
//!   neochumbyd restart NAME        its session ends its apps and starts
//!                                  again from the tree
//!   neochumbyd shot NAME OUT.png   its screen
//!   neochumbyd tap NAME X Y [--hold MS]
//!                                  touches its screen at (X, Y), in
//!                                  pixels, for MS (default 100, at most
//!                                  10000), as a finger would
//!   neochumbyd button NAME press|release|tap
//!                                  its top button: down, up, or both.
//!                                  Both of these only once its owner has
//!                                  run `neochumbyd set NAME dev=on`
//!   neochumbyd mcp                 log, restart, shot, set and pairing's
//!                                  list as an MCP server on stdio, for
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
//!   neochumbyd allow APP pairing  lets APP's server program use the admin
//!                                  socket (admin.rs) as it next starts;
//!                                  `allow` alone lists what's allowed
//!   neochumbyd disallow APP pairing
//!   neochumbyd token [new]        the owner's token, which approving or
//!                                  every change through the admin socket
//!                                  needs; `new` replaces it
//!
//! Options (each command takes --state):
//!   --state DIR        default /var/lib/neochumbyd
//!   --tree DIR         the app tree to serve, default <state>/tree
//!   --nfs ADDR         default 0.0.0.0:2049
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
mod isolate;
mod line;
mod mcp;
mod nfs;
mod png;
mod server;
mod state;
mod sync;
mod tree;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use tokio::signal::unix::{SignalKind, signal};

use neochumby_proto::Msg;
use neochumby_proto::dev::{BUTTON_TAP_MS, MAX_HOLD_MS};
use neochumby_proto::settings::key;

use state::State;

/// How long `restart`, `shot`, `tap` and `button` wait for the Chumby's
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
    nfs: String,
    api: String,
    app_uids: Option<String>,
    unsandboxed: bool,
    listen_unix: Option<PathBuf>,
    /// `--no-catalog-refresh`.
    no_catalog_refresh: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: neochumbyd serve|pairing|approve CODE [--replace]|refuse CODE|forget NAME|set NAME KEY=VALUE|manifest|log NAME [-f]|restart NAME|shot NAME OUT.png|mcp|tap NAME X Y [--hold MS]|button NAME press|release|tap|token [new]|allow [APP pairing]|disallow APP pairing|catalog [refresh] [--json]|install crate:NAME@VERSION [--yes --access WORDS]|uninstall NAME [--state DIR] [--tree DIR] [--nfs ADDR] [--api ADDR] [--app-uids FIRST-LAST] [--unsandboxed] [--listen-unix PATH] [--wamrc PATH] [--no-catalog-refresh]"
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
        nfs: "0.0.0.0:2049".into(),
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
            "--nfs" => a.nfs = value(),
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
            _ if matches!(a.command.as_str(), "set" | "shot" | "allow" | "disallow") && a.setting.is_none() => {
                a.setting = Some(arg)
            }
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
    let a = args();
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
        "pairing" => {
            let (starts, players) = (tree::start_apps(&tree_dir), tree::players(&tree_dir));
            for (name, key) in state.pairs().unwrap_or_else(|e| fail(e)) {
                let pairs: Vec<String> = state.config(&name).iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("paired   {name}  {key}  {}", pairs.join(" "));
                let settings = state.settings(&name);
                let notes = [
                    tree::start_note(&name, settings.start.as_deref(), &starts),
                    tree::player_note(&name, settings.player.as_deref(), &players),
                ];
                for note in notes.into_iter().flatten() {
                    println!("         {note}");
                }
            }
            for p in state.pending().unwrap_or_else(|e| fail(e)) {
                println!("asking   {}  code {}  from {}  {}", p.name, p.code, p.addr, neochumby_proto::hex(&p.key));
            }
            for r in catalog::chumby::requests(&a.state) {
                println!("asking   {}  code {}  to {}", r.chumby, r.code, r.what);
            }
            println!("{}", tree::hint(&starts));
            println!("{}", tree::player_hint(&players));
        }
        // A Chumby's GET APPS asking: through the admin socket's protocol,
        // with the owner's token, as a page would.
        "approve" | "refuse" if catalog::chumby::parse_code(&operand()).is_some() => {
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
        "approve" => {
            let name = state.approve(&operand(), a.replace).unwrap_or_else(|e| fail(e));
            println!("paired {name}");
        }
        "refuse" => state.refuse(&operand()).unwrap_or_else(|e| fail(e)),
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
        "allow" | "disallow" if a.operand.is_none() => {
            for (app, words) in state.allowances() {
                println!("{app}  {}", words.join(" "));
            }
        }
        "allow" | "disallow" => {
            let what = a.setting.clone().unwrap_or_else(|| usage());
            let on = a.command == "allow";
            state.allow(&operand(), &what, on).unwrap_or_else(|e| fail(e));
            let app = operand();
            if on {
                println!("{app} allowed {what}: its server program gets the admin socket as it next starts");
            } else {
                println!("{app} no longer allowed {what}: its admin socket stops answering");
            }
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

/// `name`, if a Chumby is paired under it.
fn paired(state: &State, name: &str) -> String {
    if !state.pairs().unwrap_or_else(|e| fail(e)).iter().any(|(n, _)| n == name) {
        fail(format!("no Chumby paired as {name}"));
    }
    name.to_string()
}

fn serve(a: &Args, state: State, confine: confine::Confine) {
    // Nothing it makes is for other users: apps' programs may be other
    // users (--app-uids).
    neochumby_sys::privilege::add_to_umask(0o007);
    state.secure().unwrap_or_else(|e| fail(format!("closing the state directory to other users: {e}")));
    let identity = state.identity().unwrap_or_else(|e| fail(format!("identity: {e}")));
    // The same key again, to sign the tree's manifest for NFS (nfs.rs).
    let signer = state.identity().unwrap_or_else(|e| fail(format!("identity: {e}")));
    // Requests a daemon before this one kept: no one waits on them now.
    state.clear_pending().unwrap_or_else(|e| fail(format!("pending: {e}")));
    let tree = a.tree.clone().unwrap_or_else(|| a.state.join("tree"));
    let nfs_port = a.nfs.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or_else(|| usage());
    eprintln!(
        "neochumbyd: serving {} on {}, the API on {}; key {}",
        tree.display(),
        a.nfs,
        a.api,
        neochumby_proto::hex(&identity.public())
    );

    // Made now, so the owner can read it before the web page asks.
    state.owner_token().unwrap_or_else(|e| fail(format!("owner token: {e}")));
    // An admin socket left by an earlier neochumbyd (they had paths then).
    let _ = std::fs::remove_file(a.state.join("admin.sock"));
    let server_state = state.clone();

    let listener = TcpListener::bind(&a.api).unwrap_or_else(|e| fail(format!("{}: {e}", a.api)));
    let lines = a.listen_unix.as_ref().map(|p| {
        let l = line::listen(p).unwrap_or_else(|e| fail(format!("{}: {e}", p.display())));
        eprintln!("neochumbyd: serial lines on {}", p.display());
        l
    });
    let api = Arc::new(api::Api {
        state,
        identity,
        nfs_port,
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

    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| fail(e));
    let (servers, failed) = rt.block_on(async {
        let mut term = signal(SignalKind::terminate()).unwrap_or_else(|e| fail(e));
        let fs = nfs::TreeFs::new(tree.clone(), Box::new(move |m| signer.sign(m)));
        let listener = NFSTcpListener::bind(&a.nfs, fs).await.unwrap_or_else(|e| fail(format!("{}: {e}", a.nfs)));
        // Last, so nothing above can fail with server programs running.
        let servers = server::start(&tree, &a.state, &server_state, &confine);
        let failed = tokio::select! {
            r = listener.handle_forever() => r.err(),
            _ = term.recv() => None,
            _ = tokio::signal::ctrl_c() => None,
        };
        (servers, failed)
    });
    // Stopping, on SIGTERM or SIGINT: the server programs first.
    eprintln!("neochumbyd: stopping");
    servers.stop();
    if let Some(e) = failed {
        fail(format!("{}: {e}", a.nfs));
    }
}
