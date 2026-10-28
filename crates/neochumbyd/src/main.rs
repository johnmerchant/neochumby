//! neochumbyd: what a Chumby pairs with. Serves the app tree over NFS,
//! pairs Chumbys, and runs the server half of their apps (companion.rs)
//! over neochumby-proto. It knows no app by name: an app brings its own
//! companion, in its directory of the tree.
//!
//!   neochumbyd serve [options]    the daemon
//!   neochumbyd pairing            Chumbys paired and asking to pair
//!   neochumbyd approve CODE       pair the Chumby showing CODE
//!   neochumbyd refuse CODE
//!   neochumbyd forget NAME
//!   neochumbyd set NAME KEY=VALUE  one of a Chumby's settings (`start=wx`),
//!                                  sent to it when it next connects;
//!                                  KEY= removes it
//!
//! Options (each command takes --state):
//!   --state DIR        default /var/lib/neochumbyd
//!   --tree DIR         the app tree to serve, default <state>/tree
//!   --nfs ADDR         default 0.0.0.0:2049
//!   --api ADDR         default 0.0.0.0:7340

mod api;
mod companion;
mod nfs;
mod state;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use nfsserve::tcp::{NFSTcp, NFSTcpListener};

use state::State;

struct Args {
    command: String,
    operand: Option<String>,
    setting: Option<String>,
    state: PathBuf,
    tree: Option<PathBuf>,
    nfs: String,
    api: String,
}

fn usage() -> ! {
    eprintln!(
        "usage: neochumbyd serve|pairing|approve CODE|refuse CODE|forget NAME|set NAME KEY=VALUE [--state DIR] [--tree DIR] [--nfs ADDR] [--api ADDR]"
    );
    std::process::exit(2)
}

fn args() -> Args {
    let mut a = Args {
        command: String::new(),
        operand: None,
        setting: None,
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
            _ if arg.starts_with("--") => usage(),
            _ if a.command.is_empty() => a.command = arg,
            _ if a.operand.is_none() => a.operand = Some(arg),
            _ if a.command == "set" && a.setting.is_none() => a.setting = Some(arg),
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
    match a.command.as_str() {
        "serve" => serve(&a, state),
        "pairing" => {
            for (name, key) in state.pairs().unwrap_or_else(|e| fail(e)) {
                let settings: Vec<String> = state.config(&name).iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!("paired   {name}  {key}  {}", settings.join(" "));
            }
            for p in state.pending().unwrap_or_else(|e| fail(e)) {
                println!("asking   {}  code {}  from {}  {}", p.name, p.code, p.addr, neochumby_proto::hex(&p.key));
            }
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
        }
        _ => usage(),
    }
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
    let api = Arc::new(api::Api { state, identity, nfs_port, tree: tree.clone(), state_dir: a.state.clone() });
    std::thread::spawn(move || api::serve(listener, api));

    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| fail(e));
    rt.block_on(async {
        let fs = nfs::TreeFs::new(tree);
        let listener = NFSTcpListener::bind(&a.nfs, fs).await.unwrap_or_else(|e| fail(format!("{}: {e}", a.nfs)));
        listener.handle_forever().await.unwrap_or_else(|e| fail(e));
    });
}
