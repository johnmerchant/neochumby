//! neochumbyd: what a Chumby pairs with. Serves the app tree over NFS, or
//! keeps a copy of it on the Chumby's stick (`delivery=sync`, sync.rs),
//! pairs Chumbys, and runs the server half of their apps (companion.rs)
//! over neochumby-proto. It knows no app by name: an app brings its own
//! companion, in its directory of the tree.
//!
//!   neochumbyd serve [options]    the daemon
//!   neochumbyd pairing            Chumbys paired and asking to pair, and
//!                                 the start apps in the tree (tree.rs)
//!   neochumbyd approve CODE       pair the Chumby showing CODE
//!   neochumbyd refuse CODE
//!   neochumbyd forget NAME
//!   neochumbyd set NAME KEY=VALUE  one of a Chumby's settings (`start=wx`),
//!                                  sent to it when it next connects;
//!                                  KEY= removes it. For `start`, it says
//!                                  which apps in the tree may be named;
//!                                  `delivery=sync` keeps the tree on the
//!                                  Chumby's stick
//!   neochumbyd manifest            what a sync sends: the tree's files,
//!                                  and any a Chumby could not take
//!   neochumbyd log NAME [-f]       a paired Chumby's log, as its session
//!                                  sent it; -f follows it (dev.rs)
//!   neochumbyd restart NAME        its session ends its apps and starts
//!                                  again from the tree
//!   neochumbyd shot NAME OUT.png   its screen
//!   neochumbyd mcp                 log, restart, shot, set and pairing's
//!                                  list as an MCP server on stdio, for
//!                                  Claude Code and the like (mcp.rs)
//!
//! Options (each command takes --state):
//!   --state DIR        default /var/lib/neochumbyd
//!   --tree DIR         the app tree to serve, default <state>/tree
//!   --nfs ADDR         default 0.0.0.0:2049
//!   --api ADDR         default 0.0.0.0:7340

mod api;
mod companion;
mod dev;
mod mcp;
mod nfs;
mod png;
mod state;
mod sync;
mod tree;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use nfsserve::tcp::{NFSTcp, NFSTcpListener};

use state::State;

/// How long `restart` and `shot` wait for the Chumby's developer link: long
/// enough for it to come back after the daemon itself restarted (a new
/// binary deployed), as its session reconnects within a few seconds.
const LINK_WAIT: Duration = Duration::from_secs(15);

struct Args {
    command: String,
    operand: Option<String>,
    setting: Option<String>,
    follow: bool,
    state: PathBuf,
    tree: Option<PathBuf>,
    nfs: String,
    api: String,
}

fn usage() -> ! {
    eprintln!(
        "usage: neochumbyd serve|pairing|approve CODE|refuse CODE|forget NAME|set NAME KEY=VALUE|manifest|log NAME [-f]|restart NAME|shot NAME OUT.png|mcp [--state DIR] [--tree DIR] [--nfs ADDR] [--api ADDR]"
    );
    std::process::exit(2)
}

fn args() -> Args {
    let mut a = Args {
        command: String::new(),
        operand: None,
        setting: None,
        follow: false,
        state: "/var/lib/neochumbyd".into(),
        tree: None,
        nfs: "0.0.0.0:2049".into(),
        api: format!("0.0.0.0:{}", neochumby_proto::API_PORT),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--state" => a.state = value().into(),
            "--tree" => a.tree = Some(value().into()),
            "--nfs" => a.nfs = value(),
            "--api" => a.api = value(),
            "-f" | "--follow" => a.follow = true,
            _ if arg.starts_with("--") => usage(),
            _ if a.command.is_empty() => a.command = arg,
            _ if a.operand.is_none() => a.operand = Some(arg),
            _ if matches!(a.command.as_str(), "set" | "shot") && a.setting.is_none() => a.setting = Some(arg),
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
    let a = args();
    let state = State::open(a.state.clone()).unwrap_or_else(|e| fail(format!("{}: {e}", a.state.display())));
    let operand = || a.operand.clone().unwrap_or_else(|| usage());
    let tree_dir = a.tree.clone().unwrap_or_else(|| a.state.join("tree"));
    match a.command.as_str() {
        "serve" => serve(&a, state),
        "pairing" => {
            let starts = tree::start_apps(&tree_dir);
            for (name, key) in state.pairs().unwrap_or_else(|e| fail(e)) {
                let config = state.config(&name);
                let settings: Vec<String> = config.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("paired   {name}  {key}  {}", settings.join(" "));
                let start = config.iter().find(|(k, _)| k == "start").map(|(_, v)| v.as_str());
                if let Some(note) = tree::start_note(&name, start, &starts) {
                    println!("         {note}");
                }
            }
            for p in state.pending().unwrap_or_else(|e| fail(e)) {
                println!("asking   {}  code {}  from {}  {}", p.name, p.code, p.addr, neochumby_proto::hex(&p.key));
            }
            println!("{}", tree::hint(&starts));
        }
        "approve" => {
            let name = state.approve(&operand()).unwrap_or_else(|e| fail(e));
            println!("paired {name}");
        }
        "refuse" => state.refuse(&operand()).unwrap_or_else(|e| fail(e)),
        "forget" => state.forget(&operand()).unwrap_or_else(|e| fail(e)),
        "set" => {
            let setting = a.setting.clone().unwrap_or_else(|| usage());
            let (key, value) = setting.split_once('=').unwrap_or_else(|| usage());
            state.set(&operand(), key, value).unwrap_or_else(|e| fail(e));
            if key == "start" {
                let starts = tree::start_apps(&tree_dir);
                if !value.is_empty() && !starts.iter().any(|s| s == value) {
                    eprintln!("neochumbyd: {value} isn't a start app in the tree; the Chumby will open {}", tree::HOME);
                }
                println!("{}", tree::hint(&starts));
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

fn serve(a: &Args, state: State) {
    let identity = state.identity().unwrap_or_else(|e| fail(format!("identity: {e}")));
    let tree = a.tree.clone().unwrap_or_else(|| a.state.join("tree"));
    let nfs_port = a.nfs.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or_else(|| usage());
    eprintln!(
        "neochumbyd: serving {} on {}, the API on {}; key {}",
        tree.display(),
        a.nfs,
        a.api,
        neochumby_proto::hex(&identity.public())
    );

    let listener = TcpListener::bind(&a.api).unwrap_or_else(|e| fail(format!("{}: {e}", a.api)));
    let api = Arc::new(api::Api {
        state,
        identity,
        nfs_port,
        tree: tree.clone(),
        state_dir: a.state.clone(),
        hashes: neochumby_sync::serve::Hashes::default(),
        links: Default::default(),
    });
    std::thread::spawn(move || api::serve(listener, api));

    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| fail(e));
    rt.block_on(async {
        let fs = nfs::TreeFs::new(tree);
        let listener = NFSTcpListener::bind(&a.nfs, fs).await.unwrap_or_else(|e| fail(format!("{}: {e}", a.nfs)));
        listener.handle_forever().await.unwrap_or_else(|e| fail(e));
    });
}
