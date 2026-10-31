//! GET APPS on a Chumby: neochumbyd's side of the catalog app
//! (apps/catalog), whose manifest names `neochumbyd:catalog` as its
//! companion. The two talk in neochumby_proto::catalog's messages.
//!
//! - **Listing** comes from the cache alone (discover.rs), with what has
//!   been found of each app since: its title, its publisher and whether it
//!   is a verified build.
//! - **Details** are found by fetching the app's crate and checking it as
//!   an install would, once per version, and kept in
//!   `<state>/catalog-details/`. A crate's version never changes, so
//!   neither do they. `serve` looks up the best-ranked apps' in the
//!   background ([`look_up`]); any other's the first time a Chumby asks.
//!   What a Chumby can make neochumbyd fetch is limited: only apps in the
//!   cache, in the version listed or installed; only from crates.io and
//!   GitHub (a module hosted elsewhere isn't fetched to show its size);
//!   kept once looked up; and at most [`LOOKUPS_PER_MINUTE`] new lookups a
//!   minute for each Chumby.
//! - **Installing and removing** are the owner's, on the server. The
//!   Chumby asks for exactly the access its owner approved on its screen;
//!   neochumbyd checks the app again and keeps the request in
//!   `<state>/catalog-requests/` under a short code, which the Chumby
//!   shows. The owner approves it through the admin socket's protocol, with
//!   the owner's token: `approve CODE TOKEN` (`neochumbyd approve CODE` on
//!   the command line, as for pairing) lets it go ahead, `refuse CODE
//!   TOKEN` refuses it, and ten minutes without either withdraws it.
//!   Nothing is installed on a Chumby's say alone, but an update that asks
//!   for nothing the installed version wasn't granted, as serve's own
//!   updates do (update.rs), unless the owner has turned those off.
//!   Every install, update and removal is logged (update.rs).

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use neochumby_proto::catalog::{self as wire, Answer, Ask, MAX_DESCRIPTION, PAGE, cut};
use neochumby_proto::{Msg, Session, channel, valid_name};

use super::discover::{self, Listing};
use super::http::Http;
use super::install::{self, ALLOWED, Plan, Spec, WASM_LIMIT, prepare};
use super::wamrc::Wamrc;
use super::{is_bytecode, sha256, update};

/// Where looked-up details are kept, one file per crate and version.
const DETAILS: &str = "catalog-details";
/// Where requests wait for the owner.
const REQUESTS: &str = "catalog-requests";
/// How long a request waits for the owner.
pub const REQUEST_TTL: Duration = Duration::from_secs(600);
/// The most requests waiting at once, from all Chumbys.
const MAX_REQUESTS: usize = 8;
/// The most catalog apps listed as installed.
const MAX_INSTALLED: usize = 64;
/// How many of the best-ranked apps [`look_up`] finds the details of.
const LOOK_UP: usize = 64;
/// The most details a Chumby may have looked up (not already kept) in a
/// minute.
pub const LOOKUPS_PER_MINUTE: usize = 6;
/// The letters a request's code is made of: none that looks like another.
const CODE_LETTERS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// `serve`'s wamrc, for the apps Chumbys install.
static WAMRC: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Keeps `serve`'s `--wamrc` for the installs Chumbys ask for.
pub fn set_wamrc(path: Option<PathBuf>) {
    let _ = WAMRC.set(path);
}

fn wamrc() -> Option<Wamrc> {
    Wamrc::find(WAMRC.get().cloned().flatten()).ok().flatten()
}

/// When each Chumby's last lookups were, by its name.
static LOOKUPS: Mutex<Option<HashMap<String, VecDeque<Instant>>>> = Mutex::new(None);

/// Whether `chumby` may have one more lookup now; if so, it is counted.
fn may_look_up(chumby: &str) -> bool {
    let mut all = LOOKUPS.lock().unwrap_or_else(|e| e.into_inner());
    let times = all.get_or_insert_default().entry(chumby.to_string()).or_default();
    while times.front().is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
        times.pop_front();
    }
    if times.len() >= LOOKUPS_PER_MINUTE {
        return false;
    }
    times.push_back(Instant::now());
    true
}

/// One Chumby's GET APPS.
pub struct Service {
    pub http: Http,
    pub state: PathBuf,
    pub tree: PathBuf,
    pub wamrc: Option<Wamrc>,
    /// The Chumby's name, as it paired.
    pub chumby: String,
    waiting: Option<Waiting>,
    /// The owner's yes, to act on at the next tick.
    approved: Option<Work>,
    /// Whether the Chumby is on `delivery=sync`: its stick gets the tree's
    /// changes at its next round, and its session isn't restarted.
    pub sync: bool,
    /// Whether the tree has gained or lost an app for it.
    pub changed: bool,
}

/// A request waiting for the owner.
struct Waiting {
    id: String,
    work: Work,
    since: Instant,
}

enum Work {
    Install(Box<Plan>),
    Remove { app: String },
}

impl Drop for Service {
    /// A Chumby that goes takes back what it asked.
    fn drop(&mut self) {
        self.withdraw();
    }
}

impl Service {
    pub fn new(http: Http, state: PathBuf, tree: PathBuf, wamrc: Option<Wamrc>, chumby: &str) -> Service {
        Service {
            http,
            state,
            tree,
            wamrc,
            chumby: chumby.into(),
            waiting: None,
            approved: None,
            sync: false,
            changed: false,
        }
    }

    /// Answers one of the app's messages.
    pub fn ask(&mut self, a: Ask) -> Vec<Answer> {
        match a {
            Ask::List { from } => vec![Answer::Page(self.page(from))],
            Ask::Details { krate, version } => {
                vec![match self.details(&krate, &version) {
                    Ok(d) => Answer::Details(Box::new(d)),
                    Err(e) => Answer::Failed(e),
                }]
            }
            Ask::Install { krate, version, access, network } => self.install(&krate, &version, &access, &network),
            Ask::Remove { app } => self.remove(&app),
            Ask::Cancel => {
                self.withdraw();
                Vec::new()
            }
        }
    }

    /// What has become of the request waiting, if anything.
    pub fn tick(&mut self) -> Vec<Answer> {
        if let Some(work) = self.approved.take() {
            return self.go(work);
        }
        let Some(w) = &self.waiting else { return Vec::new() };
        let dir = self.state.join(REQUESTS);
        if dir.join(format!("{}.approved", w.id)).exists() {
            let w = self.waiting.take().expect("waiting");
            let _ = std::fs::remove_file(dir.join(format!("{}.approved", w.id)));
            // Said now, done at the next tick: the Chumby shows it meanwhile.
            let mut out = vec![Answer::Progress("Approved on the server.".into())];
            if let Work::Install(plan) = &w.work
                && plan.wamrc.is_some()
            {
                out.push(Answer::Progress("Compiling it for the Chumby.".into()));
            }
            self.approved = Some(w.work);
            return out;
        }
        if !dir.join(&w.id).exists() {
            self.waiting = None;
            return vec![Answer::Failed("The owner refused it on the server.".into())];
        }
        if w.since.elapsed() > REQUEST_TTL {
            self.withdraw();
            return vec![Answer::Failed("No one approved it on the server in time.".into())];
        }
        Vec::new()
    }

    fn page(&self, from: u32) -> wire::Page {
        let cache = discover::load(&self.state).ok().flatten().unwrap_or_default();
        let apps = cache
            .apps
            .iter()
            .skip(from as usize)
            .take(PAGE as usize)
            .map(|l| {
                let known = Spec::parse(&l.spec()).ok().and_then(|s| self.known(&s));
                listing(l, known.as_ref())
            })
            .collect();
        wire::Page {
            fetched_at: cache.fetched_at,
            total: cache.apps.len() as u32,
            from,
            apps,
            installed: if from == 0 { installed_apps(&self.tree) } else { Vec::new() },
            compiles: self.wamrc.is_some(),
        }
    }

    /// The details of `krate`@`version`, as kept, or looked up now, and
    /// then as they stand against the tree.
    fn details(&self, krate: &str, version: &str) -> Result<wire::Details, String> {
        let spec = Spec::parse(&format!("crate:{krate}@{version}"))?;
        self.listed(&spec)?;
        let mut d = match self.known(&spec) {
            Some(d) => d,
            None if !may_look_up(&self.chumby) => {
                return Err("Too many apps looked up: try again in a minute.".into());
            }
            None => look_up_one(&self.http, &self.state, &spec)?,
        };
        if let Some(l) =
            discover::load(&self.state).ok().flatten().and_then(|c| c.apps.into_iter().find(|l| l.name == krate))
        {
            d.description = cut(&l.description, MAX_DESCRIPTION);
            d.downloads = d.downloads.max(l.downloads);
        }
        d.compiled = self.wamrc.is_some();
        match install::installed(&self.tree, &spec.app) {
            Ok(Some(i)) => {
                d.installed = Some(i.krate.rsplit_once('@').map(|(_, v)| v.to_string()).unwrap_or_default());
                let had: Vec<&String> = i.access.iter().chain(&i.network).collect();
                d.more = d.access.iter().chain(&d.network).filter(|w| !had.contains(w)).cloned().collect();
            }
            Ok(None) => {}
            Err(e) if d.problem.is_empty() => d.problem = e,
            Err(_) => {}
        }
        Ok(d)
    }

    /// Whether `spec` is in the cache, in the version listed, or is the
    /// version installed: all a Chumby may have looked up or installed.
    fn listed(&self, spec: &Spec) -> Result<(), String> {
        let cache = discover::load(&self.state).ok().flatten().unwrap_or_default();
        let listed = cache.apps.iter().any(|l| l.name == spec.name && l.version == spec.version);
        let installed = install::installed(&self.tree, &spec.app)
            .ok()
            .flatten()
            .is_some_and(|i| i.krate == format!("{}@{}", spec.name, spec.version));
        if listed || installed { Ok(()) } else { Err("That isn't an app in the catalog.".into()) }
    }

    fn known(&self, spec: &Spec) -> Option<wire::Details> {
        let b = std::fs::read(details_file(&self.state, spec)).ok()?;
        serde_json::from_slice(&b).ok()
    }

    fn install(&mut self, krate: &str, version: &str, access: &[String], network: &[String]) -> Vec<Answer> {
        self.withdraw();
        let r = (|| {
            let spec = Spec::parse(&format!("crate:{krate}@{version}"))?;
            self.listed(&spec)?;
            let plan = prepare(&self.http, &spec, &self.tree, self.wamrc.clone())?;
            let set = |v: &[String]| v.iter().cloned().collect::<BTreeSet<_>>();
            if set(access) != set(&plan.meta.access) || set(network) != set(&plan.meta.network) {
                return Err(format!(
                    "{} asks for {} now, not what was approved: look at it again.",
                    plan.meta.title,
                    words(&[plan.meta.access.clone(), plan.meta.network.clone()].concat())
                ));
            }
            // An update asking for nothing new needs only this Chumby's
            // owner, as serve's own updates do, unless the server's owner
            // turned those off.
            if plan.previous.is_some() && !plan.asks_more() && update::auto_updates(&self.state) {
                self.approved = Some(Work::Install(Box::new(plan)));
                return Ok(vec![Answer::Progress("It asks for nothing new: updating it.".into())]);
            }
            let what = format!("install {} ({}) for {}", plan.spec, plan.meta.title, self.chumby);
            self.request(&what, &plan.summary(), Work::Install(Box::new(plan)))
        })();
        r.unwrap_or_else(|e| vec![Answer::Failed(e)])
    }

    fn remove(&mut self, app: &str) -> Vec<Answer> {
        self.withdraw();
        let r = (|| {
            if !valid_name(app) {
                return Err(format!("{app}: not an app's name"));
            }
            let i = install::installed(&self.tree, app)?.ok_or_else(|| format!("{app} isn't installed"))?;
            let what = format!("remove apps/{app} ({}) for {}", i.krate, self.chumby);
            self.request(&what, &format!("{what}\n"), Work::Remove { app: app.into() })
        })();
        r.unwrap_or_else(|e| vec![Answer::Failed(e)])
    }

    /// Keeps a request for the owner, and says so in the log.
    fn request(&mut self, what: &str, summary: &str, work: Work) -> Result<Vec<Answer>, String> {
        let dir = self.state.join(REQUESTS);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let waiting = requests(&self.state);
        if waiting.len() >= MAX_REQUESTS {
            return Err("Too many requests are waiting on the server.".into());
        }
        let (id, code) = (random_id().map_err(|e| e.to_string())?, random_code().map_err(|e| e.to_string())?);
        std::fs::write(dir.join(&id), format!("{code} {}\n{what}\n{summary}", self.chumby))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        eprintln!(
            "neochumbyd: {} asks to {what}, showing {code}: `neochumbyd approve {code}` lets it, `neochumbyd refuse {code}` doesn't (or the admin socket's approve and refuse, with the owner's token)",
            self.chumby
        );
        self.waiting = Some(Waiting { id, work, since: Instant::now() });
        Ok(vec![Answer::Waiting { code, minutes: (REQUEST_TTL.as_secs() / 60) as u16 }])
    }

    /// The owner said yes: does it.
    fn go(&mut self, work: Work) -> Vec<Answer> {
        let mut out = Vec::new();
        match work {
            Work::Install(plan) => match plan.write(&self.tree, &self.state.join("scratch")) {
                Ok(_) => {
                    let label = if plan.verified() { "a verified build" } else { "unverified" };
                    let what = match &plan.previous {
                        Some(p) if !plan.asks_more() => format!(
                            "updated from {} to {} ({label}; it asks for nothing new), asked by {}",
                            p.krate.rsplit_once('@').map_or("?", |(_, v)| v),
                            plan.spec.version,
                            self.chumby
                        ),
                        Some(_) => format!(
                            "updated to {} ({label}), asked by {}, approved by the owner",
                            plan.spec, self.chumby
                        ),
                        None => format!(
                            "installed {} ({label}), asked by {}, approved by the owner",
                            plan.spec, self.chumby
                        ),
                    };
                    update::log(&self.state, &format!("apps/{}: {what}", plan.spec.app));
                    self.changed = true;
                    out.push(Answer::Installed {
                        app: plan.spec.app.clone(),
                        title: plan.meta.title.clone(),
                        home: home_order(&self.tree),
                        restarts: !self.sync,
                    });
                }
                Err(e) => out.push(Answer::Failed(e)),
            },
            Work::Remove { app } => match install::uninstall(&self.tree, &app) {
                Ok(()) => {
                    update::log(
                        &self.state,
                        &format!("apps/{app}: removed, asked by {}, approved by the owner", self.chumby),
                    );
                    self.changed = true;
                    out.push(Answer::Removed { app });
                }
                Err(e) => out.push(Answer::Failed(e)),
            },
        }
        out
    }

    /// Takes back the request waiting, if any.
    fn withdraw(&mut self) {
        if let Some(w) = self.waiting.take() {
            let dir = self.state.join(REQUESTS);
            let _ = std::fs::remove_file(dir.join(&w.id));
            let _ = std::fs::remove_file(dir.join(format!("{}.approved", w.id)));
        }
    }
}

/// Runs GET APPS for `chumby` until it goes: neochumbyd's built-in
/// companion (companion.rs).
///
/// A session knows the apps of the tree it started with, so once GET APPS
/// has installed or removed one and the Chumby has left it, its session
/// is started again (as `neochumbyd restart` does), unless it is on
/// `delivery=sync`, whose stick has yet to get the change.
pub fn run(tree: &Path, state: &Path, chumby: &str, mut s: Session) -> io::Result<()> {
    eprintln!("neochumbyd: GET APPS for {chumby}");
    // As every catalog request goes (a debug build's local stand-in and its
    // trusted root included).
    let http = super::http();
    let mut svc = Service::new(http, state.into(), tree.into(), wamrc(), chumby);
    svc.sync = crate::state::State::open(state.into())
        .is_ok_and(|st| st.settings(chumby).delivery == Some(neochumby_proto::Delivery::Sync));
    let r = serve(&mut svc, &mut s);
    if svc.changed && !svc.sync {
        eprintln!("neochumbyd: {chumby}'s apps have changed: restarting its session");
        if let Err(e) = crate::dev::restart(state, chumby, Duration::from_secs(15)) {
            eprintln!("neochumbyd: {chumby}: {e}");
        }
    }
    r
}

/// Answers the app until it goes.
fn serve(svc: &mut Service, s: &mut Session) -> io::Result<()> {
    loop {
        let mut out = svc.tick();
        // Anything but the app's frames (a newer Chumby's `Msg::Unknown`)
        // is skipped.
        if let Some(Msg::Data(f)) = s.recv_timeout(Duration::from_millis(250))? {
            match channel::decode::<Ask>(&f) {
                Ok(a) => out.extend(svc.ask(a)),
                Err(e) => out.push(Answer::Failed(format!("not understood: {e}"))),
            }
        }
        for a in out {
            s.send(&Msg::Data(channel::encode(&a)?))?;
        }
    }
}

/// The listing the Chumby is sent for `l`, with what is known of it.
fn listing(l: &Listing, known: Option<&wire::Details>) -> wire::Listing {
    wire::Listing {
        krate: l.name.clone(),
        version: l.version.clone(),
        app: l.app.clone(),
        title: known.map(|d| d.title.clone()).unwrap_or_default(),
        publisher: known.map(|d| cut(&d.publisher, 64)).unwrap_or_default(),
        description: cut(&l.description, MAX_DESCRIPTION),
        downloads: l.downloads,
        recent_downloads: l.recent_downloads,
        stars: l.stars,
        verified: known.filter(|d| d.problem.is_empty()).map(|d| d.verified),
        refused: known.is_some_and(|d| !d.problem.is_empty() || !d.refused.is_empty()),
    }
}

fn details_file(state: &Path, spec: &Spec) -> PathBuf {
    state.join(DETAILS).join(format!("{}@{}.json", spec.name, spec.version))
}

/// Finds the details of `spec` by fetching its crate and checking it as an
/// install does (nothing is written to the tree), from crates.io and
/// GitHub only, and keeps them unless what went wrong may not happen again
/// (a download failing, say).
pub fn look_up_one(http: &Http, state: &Path, spec: &Spec) -> Result<wire::Details, String> {
    let toml = install::crate_toml(http, spec)?;
    let (d, keep) = match install::Meta::parse(&toml, spec) {
        Ok(meta) => {
            let mut d = from_meta(http, spec, &meta);
            if let Err(e) = install::can_run(&meta) {
                d.problem = format!("{spec}: {e}");
            }
            let attestation = install::attestation(http, &meta);
            // GitHub not answering may not happen again; anything else is
            // this version's for good.
            let keep = attestation.as_ref().err().is_none_or(|e| !e.contains("GitHub didn't say"));
            match attestation {
                Ok(v) => (d.verified, d.build) = (true, format!("built by {} in {}", v.workflow, meta.repository)),
                Err(e) => d.build = e,
            }
            // Its size, if its module is on crates.io or GitHub: a module
            // hosted anywhere else is fetched at install, not to show it.
            if http.is_catalog_source(&meta.app.url) {
                match http.get_ok(&meta.app.url, WASM_LIMIT) {
                    Ok(b) if sha256(&b) == meta.app.sha256 && is_bytecode(&b) => d.size = b.len() as u64,
                    Ok(_) => d.problem = format!("{spec}: its app .wasm isn't the one its Cargo.toml pins: refused"),
                    Err(e) => return Err(format!("{spec}: its app .wasm: {e}")),
                }
            }
            (d, keep)
        }
        Err(e) => {
            let mut d = from_toml(&toml, spec);
            d.problem = format!("{spec}: {e}");
            (d, true)
        }
    };
    if keep {
        let dir = state.join(DETAILS);
        let file = details_file(state, spec);
        let tmp = dir.join(format!(".{}.{}", spec.app, super::unique()));
        let json = serde_json::to_vec(&d).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&tmp, json))
            .and_then(|()| std::fs::rename(&tmp, &file))
            .map_err(|e| format!("{}: {e}", file.display()))?;
    }
    Ok(d)
}

/// Looks up the details of the best-ranked apps that haven't been yet, a
/// few seconds apart: `serve` calls it after each refresh.
pub fn look_up(http: &Http, state: &Path) {
    let Some(c) = discover::load(state).ok().flatten() else { return };
    for l in c.apps.iter().take(LOOK_UP) {
        let Ok(spec) = Spec::parse(&l.spec()) else { continue };
        if details_file(state, &spec).exists() {
            continue;
        }
        if let Err(e) = look_up_one(http, state, &spec) {
            eprintln!("neochumbyd: catalog: {spec}: {e}");
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// What crates.io says of `spec`, with its checked metadata `m`.
fn from_meta(http: &Http, spec: &Spec, m: &install::Meta) -> wire::Details {
    wire::Details {
        krate: spec.name.clone(),
        version: spec.version.clone(),
        app: spec.app.clone(),
        title: m.title.clone(),
        publisher: install::owners(http, spec).map(|o| o.join(", ")).unwrap_or_default(),
        repository: m.repository.clone(),
        downloads: install::downloads(http, spec).map(|(all, _)| all).unwrap_or(0),
        access: m.access.clone(),
        network: m.network.clone(),
        companion: m.companion.is_some(),
        ..Default::default()
    }
}

/// What a crate's Cargo.toml says, read as plainly as it can be, for a
/// crate that can't be installed: its title, and its access words, each
/// refused or not.
fn from_toml(toml: &str, spec: &Spec) -> wire::Details {
    let t: toml::Table = toml::from_str(toml).unwrap_or_default();
    let pkg = t.get("package").and_then(|p| p.as_table());
    let m = pkg.and_then(|p| p.get("metadata")).and_then(|m| m.get("neochumby"));
    let text = |v: Option<&toml::Value>| v.and_then(|v| v.as_str()).map(|s| cut(s, 160)).unwrap_or_default();
    let list = |k: &str| -> Vec<String> {
        let v = m.and_then(|m| m.get(k)).and_then(|v| v.as_array());
        v.into_iter().flatten().filter_map(|w| w.as_str()).map(|w| cut(w, 32)).collect()
    };
    let (access, refused) = list("access").into_iter().partition(|w| ALLOWED.contains(&w.as_str()));
    wire::Details {
        krate: spec.name.clone(),
        version: spec.version.clone(),
        app: spec.app.clone(),
        title: cut(&text(m.and_then(|m| m.get("title"))), 32),
        repository: text(pkg.and_then(|p| p.get("repository"))),
        access,
        refused,
        network: list("network"),
        companion: m.and_then(|m| m.get("wasm")).and_then(|w| w.get("companion")).is_some(),
        ..Default::default()
    }
}

/// `words` as the Chumby says them: "fb, touch", or "nothing".
fn words(w: &[String]) -> String {
    if w.is_empty() { "nothing".into() } else { w.join(", ") }
}

/// A manifest's `key=value` lines.
fn manifest(dir: &Path) -> Vec<(String, String)> {
    let m = std::fs::read_to_string(dir.join("manifest")).unwrap_or_default();
    m.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().into(), v.trim().into())).collect()
}

fn get<'a>(m: &'a [(String, String)], key: &str) -> Option<&'a str> {
    m.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// The tree's app directories, in order.
fn app_dirs(tree: &Path) -> Vec<(String, PathBuf)> {
    let mut d: Vec<(String, PathBuf)> = std::fs::read_dir(tree.join("apps"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(n, _)| valid_name(n))
        .collect();
    d.sort();
    d
}

/// The catalog apps in the tree.
fn installed_apps(tree: &Path) -> Vec<wire::Installed> {
    app_dirs(tree)
        .into_iter()
        .filter_map(|(app, dir)| {
            let m = manifest(&dir);
            if get(&m, "source") != Some("catalog") {
                return None;
            }
            let (krate, version) = get(&m, "crate").and_then(|c| c.rsplit_once('@')).unwrap_or_default();
            Some(wire::Installed {
                title: get(&m, "title").unwrap_or(&app).into(),
                krate: krate.into(),
                version: version.into(),
                access: get(&m, "access")
                    .unwrap_or_default()
                    .split(',')
                    .filter(|w| !w.is_empty())
                    .map(Into::into)
                    .collect(),
                app,
            })
        })
        .take(MAX_INSTALLED)
        .collect()
}

/// The apps home lists, in its order: those with a title and something to
/// run, by directory.
fn home_order(tree: &Path) -> Vec<String> {
    app_dirs(tree)
        .into_iter()
        .filter(|(_, dir)| {
            let m = manifest(dir);
            get(&m, "title").is_some() && (get(&m, "exec").is_some() || get(&m, "wasm").is_some())
        })
        .map(|(n, _)| n)
        .take(256)
        .collect()
}

fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

fn random_id() -> io::Result<String> {
    Ok(neochumby_proto::hex(&random_bytes::<16>()?))
}

/// A request's code: eight letters in two groups (`ABCD-EF23`). A paired
/// Chumby's connection is already authenticated; the code only names the
/// request, for the owner to approve on the server.
fn random_code() -> io::Result<String> {
    let b = random_bytes::<8>()?;
    let l: String = b.iter().map(|b| CODE_LETTERS[*b as usize % CODE_LETTERS.len()] as char).collect();
    Ok(format!("{}-{}", &l[..4], &l[4..]))
}

/// `code` as typed, if it is a request's: case, dashes and spaces don't
/// matter. A pairing code (32 hex digits) never is one.
pub fn parse_code(code: &str) -> Option<String> {
    let l: String = code.chars().filter(|c| !matches!(c, '-' | ' ')).map(|c| c.to_ascii_uppercase()).collect();
    (l.len() == 8 && l.bytes().all(|b| CODE_LETTERS.contains(&b))).then(|| format!("{}-{}", &l[..4], &l[4..]))
}

/// A request waiting: its id, code, Chumby, what it asks, and the summary.
pub struct Request {
    pub id: String,
    pub code: String,
    pub chumby: String,
    pub what: String,
    pub summary: String,
}

/// The requests waiting, removing any older than [`REQUEST_TTL`].
pub fn requests(state: &Path) -> Vec<Request> {
    let dir = state.join(REQUESTS);
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let id = e.file_name().to_string_lossy().into_owned();
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        if age.is_some_and(|a| a > REQUEST_TTL) {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        let Ok(s) = std::fs::read_to_string(e.path()) else { continue };
        let mut lines = s.splitn(3, '\n');
        let (first, what, summary) =
            (lines.next().unwrap_or(""), lines.next().unwrap_or(""), lines.next().unwrap_or(""));
        let Some((code, chumby)) = first.split_once(' ') else { continue };
        out.push(Request { id, code: code.into(), chumby: chumby.into(), what: what.into(), summary: summary.into() });
    }
    out.sort_by(|a, b| a.chumby.cmp(&b.chumby));
    out
}

/// `neochumbyd approve CODE` (or `refuse`) for a request: the request
/// showing `code`, which is then let go ahead, or refused.
pub fn decide(state: &Path, code: &str, approve: bool) -> Result<Request, String> {
    let code = parse_code(code).ok_or("not a request's code: type the eight letters the Chumby shows")?;
    let r = requests(state).into_iter().find(|r| r.code == code).ok_or("no Chumby is asking with that code")?;
    let file = state.join(REQUESTS).join(&r.id);
    let io = |e: io::Error| format!("{}: {e}", file.display());
    if approve {
        std::fs::rename(&file, file.with_file_name(format!("{}.approved", r.id))).map_err(io)?;
    } else {
        std::fs::remove_file(&file).map_err(io)?;
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Fake, Publish, temp};
    use super::*;

    /// The one answer `v` holds.
    fn one(mut v: Vec<Answer>) -> Answer {
        assert_eq!(v.len(), 1, "{v:?}");
        v.remove(0)
    }

    fn service(fake: &Fake, d: &Path) -> Service {
        Service::new(fake.http(), d.join("state"), d.join("tree"), None, "den")
    }

    /// The cache the fake crates.io makes, as `catalog refresh` keeps it.
    fn refresh(fake: &Fake, state: &Path) {
        fake.serve(
            "/api/v1/crates?keyword=neochumby-app&sort=downloads&per_page=100&page=1",
            r#"{"crates":[{"name":"neochumby-weather","max_version":"1.2.0","max_stable_version":"1.2.0","description":"Forecasts, by touch","repository":"https://github.com/someone/neochumby-weather","downloads":1234,"recent_downloads":56}],"meta":{"next_page":null}}"#,
        );
        std::fs::create_dir_all(state).unwrap();
        discover::refresh(&fake.http(), state).unwrap();
    }

    #[test]
    fn codes() {
        let c = random_code().unwrap();
        assert_eq!(parse_code(&c.to_lowercase().replace('-', " ")), Some(c));
        assert_eq!(parse_code("abcd-ef01"), None, "0 and 1 aren't letters a code has");
        assert_eq!(parse_code(&"ab".repeat(16)), None, "a pairing code isn't one");
    }

    /// The listing, before and after the app's details are looked up; the
    /// details; an install the owner approves; and its removal.
    #[test]
    fn list_look_up_install_and_remove() {
        let d = temp("chumby-install");
        let fake = Fake::start();
        fake.publish(Publish::default());
        refresh(&fake, &d.join("state"));
        let mut svc = service(&fake, &d);

        let Answer::Page(p) = one(svc.ask(Ask::List { from: 0 })) else { panic!() };
        assert_eq!((p.total, p.apps.len(), p.installed.len()), (1, 1, 0));
        let l = &p.apps[0];
        assert_eq!(
            (l.krate.as_str(), l.version.as_str(), l.title.as_str(), l.verified),
            ("neochumby-weather", "1.2.0", "", None)
        );

        let ask = Ask::Details { krate: "neochumby-weather".into(), version: "1.2.0".into() };
        let Answer::Details(det) = one(svc.ask(ask)) else { panic!() };
        assert_eq!(det.title, "WEATHER");
        assert_eq!(det.publisher, "someone");
        assert_eq!(det.access, ["fb", "touch"]);
        assert!(det.refused.is_empty() && det.problem.is_empty() && !det.compiled && !det.verified);
        assert_eq!(det.size, 13);
        assert_eq!(det.description, "Forecasts, by touch");

        // Now listed with what was found.
        let Answer::Page(p) = one(svc.ask(Ask::List { from: 0 })) else { panic!() };
        assert_eq!((p.apps[0].title.as_str(), p.apps[0].publisher.as_str()), ("WEATHER", "someone"));
        assert_eq!(p.apps[0].verified, Some(false));

        // Access other than what it asks is refused before anything waits.
        let install = |access: &[&str]| Ask::Install {
            krate: "neochumby-weather".into(),
            version: "1.2.0".into(),
            access: access.iter().map(|w| w.to_string()).collect(),
            network: Vec::new(),
        };
        let Answer::Failed(e) = one(svc.ask(install(&["fb"]))) else { panic!() };
        assert!(e.contains("look at it again"), "{e}");
        assert!(requests(&svc.state).is_empty());

        let Answer::Waiting { code, minutes: 10 } = one(svc.ask(install(&["touch", "fb"]))) else { panic!() };
        assert!(svc.tick().is_empty(), "nothing happens until the owner says");
        assert!(!d.join("tree/apps/weather").exists());
        let r = decide(&svc.state, &code.to_lowercase(), true).unwrap();
        assert_eq!(r.chumby, "den");
        assert!(r.what.starts_with("install crate:neochumby-weather@1.2.0 (WEATHER)"), "{}", r.what);
        assert!(r.summary.contains("access        fb, touch"), "{}", r.summary);
        // Said first, then done at the next tick.
        assert_eq!(svc.tick(), [Answer::Progress("Approved on the server.".into())]);
        assert!(!d.join("tree/apps/weather").exists());
        let installed = Answer::Installed {
            app: "weather".into(),
            title: "WEATHER".into(),
            home: vec!["weather".into()],
            restarts: true,
        };
        assert_eq!(svc.tick(), [installed]);
        assert!(d.join("tree/apps/weather/app.wasm").exists());
        assert!(requests(&svc.state).is_empty());

        let Answer::Page(p) = one(svc.ask(Ask::List { from: 0 })) else { panic!() };
        assert_eq!(p.installed[0].krate, "neochumby-weather");
        assert_eq!(p.installed[0].access, ["fb", "touch"]);

        // The owner refuses a removal; then approves one.
        let Answer::Waiting { code, .. } = one(svc.ask(Ask::Remove { app: "weather".into() })) else { panic!() };
        decide(&svc.state, &code, false).unwrap();
        assert!(matches!(&svc.tick()[..], [Answer::Failed(_)]));
        let Answer::Waiting { code, .. } = one(svc.ask(Ask::Remove { app: "weather".into() })) else { panic!() };
        decide(&svc.state, &code, true).unwrap();
        svc.tick();
        assert_eq!(svc.tick(), [Answer::Removed { app: "weather".into() }]);
        assert!(!d.join("tree/apps/weather").exists());
    }

    /// A crate asking for what a catalog app never gets: its details say
    /// which, and it can't be installed.
    #[test]
    fn refused_access_is_shown_as_refused() {
        let d = temp("chumby-refused");
        let fake = Fake::start();
        let meta = "title = \"SNOOP\"\naccess = [\"touch\", \"root\"]\n";
        fake.publish(Publish { meta, ..Publish::default() });
        refresh(&fake, &d.join("state"));
        let mut svc = service(&fake, &d);
        let ask = Ask::Details { krate: "neochumby-weather".into(), version: "1.2.0".into() };
        let Answer::Details(det) = one(svc.ask(ask)) else { panic!() };
        assert_eq!(
            (det.title.as_str(), det.access.as_slice(), det.refused.as_slice()),
            ("SNOOP", &["touch".to_string()][..], &["root".to_string()][..])
        );
        assert!(det.problem.contains("never granted"), "{}", det.problem);
        // Kept: it won't change.
        assert!(details_file(&d.join("state"), &Spec::parse("crate:neochumby-weather@1.2.0").unwrap()).exists());
    }

    /// A Chumby that goes takes its request with it; and one that waits too
    /// long is told so.
    #[test]
    fn requests_go_with_the_chumby() {
        let d = temp("chumby-gone");
        let fake = Fake::start();
        fake.publish(Publish::default());
        refresh(&fake, &d.join("state"));
        let mut svc = service(&fake, &d);
        let ask = Ask::Install {
            krate: "neochumby-weather".into(),
            version: "1.2.0".into(),
            access: vec!["fb".into(), "touch".into()],
            network: Vec::new(),
        };
        assert!(matches!(&svc.ask(ask.clone())[..], [Answer::Waiting { .. }]));
        assert_eq!(requests(&svc.state).len(), 1);
        svc.ask(Ask::Cancel);
        assert!(requests(&svc.state).is_empty());
        assert!(matches!(&svc.ask(ask)[..], [Answer::Waiting { .. }]));
        svc.waiting.as_mut().unwrap().since -= REQUEST_TTL + Duration::from_secs(1);
        assert!(matches!(&svc.tick()[..], [Answer::Failed(e)] if e.contains("in time")));
        assert!(requests(&svc.state).is_empty());
        assert!(matches!(&svc.ask(Ask::Remove { app: "weather".into() })[..], [Answer::Failed(_)]));
        let state = svc.state.clone();
        drop(svc);
        assert!(requests(&state).is_empty());
    }

    fn ask_install(version: &str, access: &[&str]) -> Ask {
        Ask::Install {
            krate: "neochumby-weather".into(),
            version: version.into(),
            access: access.iter().map(|w| w.to_string()).collect(),
            network: Vec::new(),
        }
    }

    /// The owner approves or refuses a Chumby's request through the admin
    /// socket's protocol, and only with the owner's token.
    #[test]
    fn requests_are_approved_through_the_admin_socket() {
        use crate::admin::request;
        let d = temp("chumby-admin");
        let fake = Fake::start();
        fake.publish(Publish::default());
        refresh(&fake, &d.join("state"));
        let mut svc = service(&fake, &d);
        let st = crate::state::State::open(svc.state.clone()).unwrap();
        let token = st.owner_token().unwrap();

        let Answer::Waiting { code, .. } = one(svc.ask(ask_install("1.2.0", &["fb", "touch"]))) else { panic!() };
        let listed = request(&st, "requests");
        assert!(listed.starts_with(&format!("request {code} den install crate:neochumby-weather@1.2.0")), "{listed}");
        assert_eq!(request(&st, &format!("approve {code}")), "error approve: wrong number of words\n");
        assert_eq!(request(&st, &format!("approve {code} nope")), "error wrong token\n");
        assert_eq!(request(&st, &format!("refuse {code} nope")), "error wrong token\n");
        assert!(svc.tick().is_empty(), "still waiting");
        let a = request(&st, &format!("approve {code} {token}"));
        assert!(a.ends_with("ok den\n") && a.contains("summary access        fb, touch"), "{a}");
        assert_eq!(svc.tick(), [Answer::Progress("Approved on the server.".into())]);
        assert!(matches!(&svc.tick()[..], [Answer::Installed { .. }]));

        // Refused, with the token.
        let Answer::Waiting { code, .. } = one(svc.ask(Ask::Remove { app: "weather".into() })) else { panic!() };
        assert_eq!(
            request(&st, &format!("refuse {code} {token}")),
            "summary remove apps/weather (neochumby-weather@1.2.0) for den\nok den\n"
        );
        assert!(matches!(&svc.tick()[..], [Answer::Failed(e)] if e.contains("refused")));
        assert!(d.join("tree/apps/weather").exists());
        // A code no one is asking with.
        let e = request(&st, &format!("approve ABCD-EFGH {token}"));
        assert!(e.starts_with("error no Chumby is asking"), "{e}");
        let log = std::fs::read_to_string(svc.state.join(update::LOG)).unwrap();
        assert!(
            log.contains("apps/weather: installed crate:neochumby-weather@1.2.0 (unverified), asked by den"),
            "{log}"
        );
    }

    /// An update asking for nothing new needs only the Chumby; asking for
    /// more, or with automatic updates off, it waits for the owner.
    #[test]
    fn an_update_asking_nothing_new_needs_only_the_chumby() {
        let d = temp("chumby-update");
        let fake = Fake::start();
        fake.publish(Publish::default());
        refresh(&fake, &d.join("state"));
        let mut svc = service(&fake, &d);
        let Answer::Waiting { code, .. } = one(svc.ask(ask_install("1.2.0", &["fb", "touch"]))) else { panic!() };
        decide(&svc.state, &code, true).unwrap();
        svc.tick();
        svc.tick();

        fake.publish(Publish { version: "1.2.1", ..Publish::default() });
        fake.list(&svc.state, "1.2.1");
        assert_eq!(
            svc.ask(ask_install("1.2.1", &["fb", "touch"])),
            [Answer::Progress("It asks for nothing new: updating it.".into())]
        );
        assert!(matches!(&svc.tick()[..], [Answer::Installed { .. }]));
        let m = std::fs::read_to_string(d.join("tree/apps/weather/manifest")).unwrap();
        assert!(m.contains("crate=neochumby-weather@1.2.1"), "{m}");
        let log = std::fs::read_to_string(svc.state.join(update::LOG)).unwrap();
        assert!(
            log.contains("updated from 1.2.0 to 1.2.1 (unverified; it asks for nothing new), asked by den"),
            "{log}"
        );

        // Off: the same kind of update waits.
        update::set_auto_updates(&svc.state, false).unwrap();
        fake.publish(Publish { version: "1.2.2", ..Publish::default() });
        fake.list(&svc.state, "1.2.2");
        assert!(matches!(&svc.ask(ask_install("1.2.2", &["fb", "touch"]))[..], [Answer::Waiting { .. }]));
        svc.ask(Ask::Cancel);
        update::set_auto_updates(&svc.state, true).unwrap();
        // Asking for more waits too.
        let meta = "title = \"WEATHER\"\naccess = [\"fb\", \"touch\", \"speaker\"]\n";
        fake.publish(Publish { version: "1.3.0", meta, ..Publish::default() });
        fake.list(&svc.state, "1.3.0");
        let ask = ask_install("1.3.0", &["fb", "touch", "speaker"]);
        assert!(matches!(&svc.ask(ask)[..], [Answer::Waiting { .. }]));
    }

    /// What a Chumby can have looked up: only what the cache lists, or is
    /// installed, and only so many a minute.
    #[test]
    fn lookups_are_limited() {
        let d = temp("chumby-limits");
        let fake = Fake::start();
        fake.publish(Publish::default());
        fake.publish(Publish { version: "1.1.0", ..Publish::default() });
        refresh(&fake, &d.join("state"));
        let svc = Service::new(fake.http(), d.join("state"), d.join("tree"), None, "limited");
        let details = |svc: &Service, v: &str| svc.details("neochumby-weather", v);

        // A version the cache doesn't list, even one on crates.io.
        assert!(details(&svc, "1.1.0").unwrap_err().contains("isn't an app in the catalog"));
        assert!(svc.details("neochumby-other", "1.0.0").is_err());
        assert!(fake.requests("/static/neochumby-weather/neochumby-weather-1.1.0.crate").is_empty());
        let mut s2 = Service::new(fake.http(), d.join("state"), d.join("tree"), None, "limited");
        assert!(matches!(&s2.ask(ask_install("1.1.0", &["fb", "touch"]))[..], [Answer::Failed(_)]));

        // Kept once looked up: asked again, nothing is fetched.
        details(&svc, "1.2.0").unwrap();
        let crate_path = "/static/neochumby-weather/neochumby-weather-1.2.0.crate";
        let fetched = fake.requests(crate_path).len();
        details(&svc, "1.2.0").unwrap();
        assert_eq!(fake.requests(crate_path).len(), fetched);

        // So many new lookups a minute for each Chumby, then no more.
        for _ in 1..LOOKUPS_PER_MINUTE {
            assert!(may_look_up("limited"));
        }
        assert!(!may_look_up("limited"));
        std::fs::remove_dir_all(d.join("state").join(DETAILS)).unwrap();
        assert!(details(&svc, "1.2.0").unwrap_err().contains("Too many"));
        assert!(may_look_up("another"), "each Chumby has its own");
    }

    /// A module hosted anywhere but crates.io and GitHub isn't fetched to
    /// show a Chumby its details.
    #[test]
    fn details_fetch_only_from_crates_io_and_github() {
        let d = temp("chumby-elsewhere");
        let fake = Fake::start();
        let bytes = b"\0asm\x01\0\0\0elsewhere".to_vec();
        let app = format!("app = {{ url = \"https://example.com/app.wasm\", sha256 = \"{}\" }}", sha256(&bytes));
        fake.publish(Publish { app: Some(app), ..Publish::default() });
        refresh(&fake, &d.join("state"));
        let svc = service(&fake, &d);
        let det = svc.details("neochumby-weather", "1.2.0").unwrap();
        assert_eq!((det.title.as_str(), det.size), ("WEATHER", 0));
        assert!(!det.verified && det.build.contains("no attestation"), "{}", det.build);
    }
}
