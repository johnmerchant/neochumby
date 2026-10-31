//! The catalog (GET APPS on a Chumby's screen): other people's WebAssembly
//! apps, published as crates on crates.io, found once a day and installed
//! by the owner, pinned and checked (docs/proposals/0003-wasm-apps.md).
//! crates.io is where they are published, not who vouches for them: nothing
//! here says the Rust project, or crates.io, endorses an app.
//!
//!   neochumbyd catalog [--json]   what was found, from the cache only
//!   neochumbyd catalog refresh    asks crates.io and GitHub now (`serve`
//!                                 does daily, unless started with
//!                                 --no-catalog-refresh) (discover.rs),
//!                                 then updates what it can (update.rs)
//!   neochumbyd catalog log        every install, update and removal
//!   neochumbyd catalog auto-update [on|off]
//!                                 whether an update asking for nothing
//!                                 new applies by itself: on, unless the
//!                                 owner turns it off
//!   neochumbyd install crate:NAME@VERSION [--yes --access WORDS]
//!                      [--wamrc PATH]
//!                                 fetches, checks and shows it, and on
//!                                 the owner's yes puts it in the tree as
//!                                 apps/<name>/ (install.rs), compiled by
//!                                 wamrc if there is one (wamrc.rs)
//!   neochumbyd uninstall NAME     removes a catalog app from the tree;
//!                                 no other
//!
//! Each also takes --state DIR and --tree DIR, as the other commands do.
//! Installing, removing and the setting are the owner's: they go through
//! the admin socket's protocol (admin.rs: `install`, `uninstall`,
//! `auto-update`), with the owner's token, which the command line reads
//! from the state directory as the server's admin. A page given the admin
//! socket (`neochumbyd allow APP pairing`) does the same with the token
//! its owner types.
//!
//! For any page: [`Catalog`] and [`load`] read the cache; [`prepare`],
//! [`Plan::summary`], [`Plan::approve`] and [`Plan::write`] are an install,
//! a step at a time. A Chumby's GET APPS is [`chumby`]: it asks, and the
//! owner approves the code it shows (`neochumbyd approve CODE`, through
//! the admin socket's protocol).

pub mod chumby;
mod discover;
mod http;
mod install;
mod sigstore;
mod update;
mod wamrc;

use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[allow(unused_imports)]
pub use discover::{Catalog, GitHub, Listing, Repo, load};
#[allow(unused_imports)]
pub use install::{Approval, Meta, Plan, Spec, prepare, uninstall};
pub use update::{auto_updates, log, set_auto_updates};
pub use wamrc::Wamrc;

use http::{Http, Sources};

/// The commands this module runs.
pub const COMMANDS: &[&str] = &["catalog", "install", "uninstall"];

/// The hex sha256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `bytes` are WebAssembly bytecode (its magic number, version 1).
pub fn is_bytecode(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\0asm\x01\0\0\0")
}

/// A number no other call in this process gets, for scratch names.
fn unique() -> u64 {
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// What the admin socket's installs for a state directory use: `serve`'s
/// tree, wamrc and client, or the command line's.
#[derive(Clone)]
struct Context {
    tree: PathBuf,
    wamrc: Option<PathBuf>,
    http: Arc<Http>,
}

/// Each state directory's [`Context`] (one, but for tests).
static CONTEXTS: Mutex<Option<HashMap<PathBuf, Context>>> = Mutex::new(None);

/// Sets the tree and wamrc that installs for `state` go to.
pub fn configure(state: &Path, tree: PathBuf, wamrc: Option<PathBuf>) {
    configure_with(state, tree, wamrc, http());
}

fn configure_with(state: &Path, tree: PathBuf, wamrc: Option<PathBuf>, http: Http) {
    let mut c = CONTEXTS.lock().unwrap_or_else(|e| e.into_inner());
    c.get_or_insert_default().insert(state.to_path_buf(), Context { tree, wamrc, http: Arc::new(http) });
}

fn context(state: &Path) -> Result<Context, String> {
    let c = CONTEXTS.lock().unwrap_or_else(|e| e.into_inner());
    c.as_ref().and_then(|c| c.get(state)).cloned().ok_or_else(|| "this neochumbyd has no tree for the catalog".into())
}

/// The client every catalog request goes through: crates.io and GitHub,
/// or in a debug build a local stand-in (Sources::configured), whose
/// attestations are checked against NEOCHUMBY_CATALOG_LOCAL_TRUST if set.
pub fn http() -> Http {
    let sources = Sources::configured();
    #[cfg(debug_assertions)]
    if sources.is_local()
        && let Some(t) = std::env::var_os("NEOCHUMBY_CATALOG_LOCAL_TRUST")
    {
        match std::fs::read_to_string(&t).map_err(|e| e.to_string()).and_then(|j| sigstore::Trust::from_json(&j)) {
            Ok(trust) => return Http::new(sources).trusting(trust),
            Err(e) => eprintln!("neochumbyd: NEOCHUMBY_CATALOG_LOCAL_TRUST: {e}"),
        }
    }
    Http::new(sources)
}

fn usage() -> ! {
    eprintln!(
        "usage: neochumbyd catalog [refresh|log] [--json] | catalog auto-update [on|off] | install crate:NAME@VERSION [--yes --access WORDS] [--wamrc PATH] | uninstall NAME   [--state DIR] [--tree DIR]"
    );
    std::process::exit(2)
}

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("neochumbyd: {e}");
    std::process::exit(1)
}

/// Runs `command` (one of [`COMMANDS`]) with the arguments after it.
pub fn main(command: &str, args: Vec<String>) -> ! {
    let mut state = PathBuf::from("/var/lib/neochumbyd");
    let (mut tree, mut wamrc, mut operands) = (None, None, Vec::new());
    let (mut json, mut approval) = (false, Approval::default());
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--state" => state = value().into(),
            "--tree" => tree = Some(PathBuf::from(value())),
            "--wamrc" => wamrc = Some(PathBuf::from(value())),
            "--json" if command == "catalog" => json = true,
            "--yes" | "-y" if command == "install" => approval.yes = true,
            "--access" if command == "install" => approval.access = Some(value()),
            _ if arg.starts_with('-') => usage(),
            _ if operands.len() < 2 => operands.push(arg),
            _ => usage(),
        }
    }
    let tree = tree.unwrap_or_else(|| state.join("tree"));
    configure(&state, tree.clone(), wamrc.clone());
    let ops: Vec<&str> = operands.iter().map(String::as_str).collect();
    match (command, ops.as_slice()) {
        ("catalog", []) => list(&state, json, wamrc),
        ("catalog", ["refresh"]) if !json => {
            std::fs::create_dir_all(&state).unwrap_or_else(|e| fail(format!("{}: {e}", state.display())));
            let http = http();
            let c = discover::refresh(&http, &state).unwrap_or_else(|e| fail(e));
            println!("{} apps in the catalog", c.apps.len());
            let w = Wamrc::find(wamrc).unwrap_or_else(|e| fail(e));
            for o in update::auto_update(&http, &state, &tree, w.as_ref()) {
                println!("{o:?}");
            }
        }
        ("catalog", ["log"]) if !json => {
            for f in [format!("{}.1", update::LOG), update::LOG.to_string()] {
                if let Ok(s) = std::fs::read_to_string(state.join(f)) {
                    print!("{s}");
                }
            }
        }
        ("catalog", ["auto-update"]) if !json => {
            let a = admin(&state, "auto-update", false);
            println!("automatic updates are {}", a.last().map_or("on", String::as_str));
        }
        ("catalog", ["auto-update", on @ ("on" | "off")]) if !json => {
            admin(&state, &format!("auto-update {on}"), true);
            println!("automatic updates are {on}");
        }
        ("install", [spec]) => {
            let parsed = Spec::parse(spec).unwrap_or_else(|e| fail(e));
            let w = Wamrc::find(wamrc).unwrap_or_else(|e| fail(e));
            let plan = prepare(&http(), &parsed, &tree, w).unwrap_or_else(|e| fail(e));
            print!("{}", plan.summary());
            if !approval.yes && std::io::stdin().is_terminal() {
                print!("Install it? [y/N] ");
                let _ = std::io::stdout().flush();
                let mut line = String::new();
                let _ = std::io::stdin().lock().read_line(&mut line);
                if !matches!(line.trim(), "y" | "Y" | "yes") {
                    fail("not installed");
                }
            } else {
                plan.approve(&approval).unwrap_or_else(|e| fail(e));
            }
            // What it asks, as approved: the socket checks it again, against
            // the crate as it fetches it.
            let asks = install::words(&plan.asks());
            let a = admin(&state, &format!("install {spec} {asks}"), true);
            println!(
                "installed {} as {}",
                plan.spec,
                tree.join("apps").join(a.last().unwrap_or(&plan.spec.app)).display()
            );
        }
        ("uninstall", [app]) => {
            admin(&state, &format!("uninstall {app}"), true);
            println!("uninstalled {app}");
        }
        _ => usage(),
    }
    std::process::exit(0)
}

/// Sends `line` (with the owner's token after it, if `token`) through the
/// admin socket's protocol, answered here: its data lines, then the word
/// after `ok`, if any.
fn admin(state: &Path, line: &str, token: bool) -> Vec<String> {
    let st = crate::state::State::open(state.into()).unwrap_or_else(|e| fail(format!("{}: {e}", state.display())));
    let line = if token {
        let t = st.owner_token().unwrap_or_else(|e| fail(format!("the owner's token: {e}")));
        format!("{line} {t}")
    } else {
        line.to_string()
    };
    crate::admin::local(&st, &line).unwrap_or_else(|e| fail(e))
}

/// `neochumbyd catalog`: the cache, as it is.
fn list(state: &Path, json: bool, wamrc: Option<PathBuf>) {
    let c = load(state).unwrap_or_else(|e| fail(format!("{}: {e}", state.join(discover::FILE).display())));
    let Some(c) = c else {
        if json {
            fail("no catalog yet: neochumbyd catalog refresh");
        }
        println!("no catalog yet: `neochumbyd serve` fetches it daily, or neochumbyd catalog refresh");
        return;
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&c).unwrap_or_else(|e| fail(e)));
        return;
    }
    for a in &c.apps {
        let stars = a.stars.map(|s| format!(", {s} stars")).unwrap_or_default();
        println!("{}  ({} recent downloads{stars})", a.spec(), a.recent_downloads);
        if !a.description.is_empty() {
            println!("    {}", a.description);
        }
    }
    println!("{} apps; fetched {}", c.apps.len(), update::utc(c.fetched_at));
    if !matches!(Wamrc::find(wamrc), Ok(Some(_))) {
        println!("no wamrc (--wamrc or NEOCHUMBY_WAMRC): apps installed now run interpreted, slowly");
    }
    if !auto_updates(state) {
        println!("automatic updates are off (neochumbyd catalog auto-update on)");
    }
}

/// The admin socket's `install SPEC ASKS`: installs (or updates) `spec`
/// in the tree if it asks for exactly `asks` (its access words and its
/// companion's hosts, comma-separated, or `none`). The owner's token was
/// checked. Its summary's lines, then the app's directory name.
pub fn admin_install(state: &Path, spec: &str, asks: &str) -> Result<Vec<String>, String> {
    let ctx = context(state)?;
    let spec = Spec::parse(spec)?;
    let wamrc = Wamrc::find(ctx.wamrc.clone())?;
    let plan = prepare(&ctx.http, &spec, &ctx.tree, wamrc)?;
    plan.approve(&Approval { yes: true, access: Some(asks.into()) })?;
    plan.write(&ctx.tree, &state.join("scratch"))?;
    let what = match &plan.previous {
        Some(p) => format!("updated from {} to {}", p.krate.rsplit_once('@').map_or("?", |(_, v)| v), spec.version),
        None => format!("installed {spec}"),
    };
    let label = if plan.verified() { "a verified build" } else { "unverified" };
    log(state, &format!("apps/{}: {what} ({}, {label}), approved by the owner", spec.app, plan.meta.title));
    let mut out: Vec<String> = plan.summary().lines().map(|l| format!("summary {}", l.trim_start())).collect();
    out.push(spec.app.clone());
    Ok(out)
}

/// The admin socket's `uninstall APP`: removes a catalog app. The owner's
/// token was checked.
pub fn admin_uninstall(state: &Path, app: &str) -> Result<(), String> {
    let ctx = context(state)?;
    let was = install::installed(&ctx.tree, app)?.map(|i| i.krate).unwrap_or_default();
    uninstall(&ctx.tree, app)?;
    log(state, &format!("apps/{app}: removed ({was}), approved by the owner"));
    Ok(())
}

/// What `serve` does for the catalog, on a thread of its own: compiles
/// any catalog app whose `.aot` isn't this wamrc's (none, or another
/// WAMR's), then, unless `refresh` is false (`--no-catalog-refresh`),
/// keeps the cache fresh and the apps up to date. Another thread looks up
/// what Chumbys' GET APPS shows of the best-ranked apps, once per version
/// (chumby.rs).
pub fn serve(state: PathBuf, tree: PathBuf, wamrc: Option<PathBuf>, refresh: bool) {
    configure(&state, tree.clone(), wamrc.clone());
    chumby::set_wamrc(wamrc.clone());
    let state_for_chumbys = state.clone();
    std::thread::spawn(move || keep(http(), state, tree, wamrc, refresh));
    std::thread::spawn(move || {
        let http = http();
        loop {
            std::thread::sleep(Duration::from_secs(60));
            chumby::look_up(&http, &state_for_chumbys);
            std::thread::sleep(Duration::from_secs(59 * 60));
        }
    });
}

/// [`serve`]'s thread, with `http`.
fn keep(http: Http, state: PathBuf, tree: PathBuf, wamrc: Option<PathBuf>, refresh: bool) {
    let w = match Wamrc::find(wamrc) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("neochumbyd: {e}; catalog apps run interpreted");
            None
        }
    };
    if let Some(w) = &w {
        recompile(&tree, w, &state.join("scratch"));
    }
    if !refresh {
        eprintln!(
            "neochumbyd: catalog: --no-catalog-refresh: crates.io and GitHub are asked only by `neochumbyd catalog refresh`"
        );
        return;
    }
    if !auto_updates(&state) {
        eprintln!("neochumbyd: catalog: automatic updates are off (neochumbyd catalog auto-update on)");
    }
    discover::daily(http, &state, |http| {
        update::auto_update(http, &state, &tree, w.as_ref());
    })
}

/// Compiles each catalog app in `tree` whose manifest records no `.aot`
/// from `w`'s WAMR for its `.wasm`, and records the new one.
pub fn recompile(tree: &Path, w: &Wamrc, scratch: &Path) {
    let Ok(dirs) = std::fs::read_dir(tree.join("apps")) else { return };
    for d in dirs.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        if !neochumby_proto::valid_name(&name) {
            continue;
        }
        if let Err(e) = recompile_one(&d.path(), w, scratch) {
            eprintln!("neochumbyd: apps/{name}: {e}; it runs interpreted");
        }
    }
}

fn recompile_one(dir: &Path, w: &Wamrc, scratch: &Path) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let Ok(m) = std::fs::read_to_string(dir.join("manifest")) else { return Ok(()) };
    let get = |k: &str| m.lines().filter_map(|l| l.split_once('=')).find(|(key, _)| *key == k).map(|(_, v)| v);
    if get("source") != Some("catalog") || get("wasm") != Some("app.wasm") {
        return Ok(());
    }
    let wasm = std::fs::read(dir.join("app.wasm")).map_err(io)?;
    let key = w.key(&wasm);
    let aot_ok = std::fs::read(dir.join("app.aot")).is_ok_and(|a| get("aot-sha256") == Some(&sha256(&a)));
    if get("aot-key") == Some(&key) && aot_ok {
        return Ok(());
    }
    let (aot, _) = w.compile(&wasm, scratch)?;
    let tmp = dir.join(format!(".app.aot.{}", unique()));
    std::fs::write(&tmp, &aot).map_err(io)?;
    std::fs::rename(&tmp, dir.join("app.aot")).map_err(io)?;
    let mut lines: Vec<String> = m
        .lines()
        .filter(|l| !["aot=", "aot-key=", "aot-sha256="].iter().any(|p| l.starts_with(p)))
        .map(str::to_string)
        .collect();
    let at = lines.iter().position(|l| l.starts_with("wasm=")).map_or(lines.len(), |i| i + 1);
    let new = ["aot=app.aot".to_string(), format!("aot-key={key}"), format!("aot-sha256={}", sha256(&aot))];
    lines.splice(at..at, new);
    let tmp = dir.join(format!(".manifest.{}", unique()));
    std::fs::write(&tmp, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).map_err(io)?;
    std::fs::rename(&tmp, dir.join("manifest")).map_err(io)?;
    eprintln!("neochumbyd: {}: compiled for WAMR {}", dir.display(), w.id);
    Ok(())
}

#[cfg(test)]
pub mod tests;
