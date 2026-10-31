//! Installing a catalog app, `crate:NAME@VERSION`, into the tree as
//! `apps/<app>/`, where the crate is `neochumby-<app>`:
//!
//! 1. Its entry in crates.io's sparse index: the version must be there and
//!    not yanked, and gives the `.crate`'s sha256 (`cksum`).
//! 2. The `.crate`, from static.crates.io, checked against it. Its
//!    Cargo.toml is the app's manifest (crates.io's API doesn't show
//!    `package.metadata`): [`Meta::parse`] checks it against the schema and
//!    the catalog's access policy. A crate that ships an `.aot`, or names
//!    one, is refused: an `.aot` is native code, and is only ever compiled
//!    here.
//! 3. Each `.wasm` its metadata pins, over HTTPS, checked against its
//!    sha256 and that it is bytecode, and kept: an asset that later
//!    changes or goes doesn't change an installed app.
//! 4. A label, not a requirement: "verified build" if one of GitHub's
//!    artifact attestations for the app's `.wasm` verifies here, as a
//!    Sigstore bundle, as built by a workflow of the crate's repository
//!    (sigstore.rs). Else "unverified", and why.
//! 5. [`Plan::summary`] for the owner, who approves it ([`Plan::approve`]):
//!    on a terminal y/N, else `--yes --access` naming the very access words
//!    and companion hosts it asks. An update that asks for nothing the
//!    installed version didn't have needs only `--yes`, and `serve` applies
//!    one by itself (update.rs).
//! 6. [`Plan::write`]: compiles the `.aot` (wamrc.rs), if there is a
//!    wamrc, and puts the app in the tree, in a directory beside it renamed
//!    into place, with a manifest only the installer writes
//!    (`source=catalog`).

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use neochumby_proto::valid_name;

use super::http::{Http, JSON_LIMIT};
use super::sigstore::{self, Expect, Verified};
use super::wamrc::Wamrc;
use super::{is_bytecode, sha256};

/// What a catalog crate's name starts with: `neochumby-<app>`.
pub const PREFIX: &str = "neochumby-";
/// The keyword every catalog crate carries.
pub const KEYWORD: &str = "neochumby-app";
/// The access words a catalog app may be granted.
pub const ALLOWED: &[&str] = &["fb", "touch", "speaker", "accel"];
/// The words it never is (neochumby_wasm::grant::NEVER_FOR_CATALOG).
pub const REFUSED: &[&str] = &["root", "system", "terminal", "logs", "network"];
/// The largest `.crate` taken (crates.io's own limit is 10 MB).
pub const CRATE_LIMIT: u64 = 16 << 20;
/// The largest `.wasm` taken.
pub const WASM_LIMIT: u64 = 32 << 20;
/// The most a `.crate` may unpack to.
const UNPACKED_LIMIT: u64 = 256 << 20;

/// A pinned catalog app: `crate:NAME@VERSION`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    /// The crate, `neochumby-<app>`.
    pub name: String,
    pub version: String,
    /// Its directory in the tree's apps/.
    pub app: String,
}

impl Spec {
    /// `crate:NAME@VERSION`; the version is required.
    pub fn parse(s: &str) -> Result<Spec, String> {
        let rest = s.strip_prefix("crate:").ok_or_else(|| format!("{s}: a catalog app is named crate:NAME@VERSION"))?;
        let (name, version) = rest
            .split_once('@')
            .ok_or_else(|| format!("{s}: which version? crate:{rest}@VERSION (none is chosen for you)"))?;
        let app = name.strip_prefix(PREFIX).unwrap_or_default();
        if !valid_name(app) || app.contains('_') {
            return Err(format!("{name}: a catalog app's crate is {PREFIX}<name>, its name a-z, 0-9 and -"));
        }
        let ok_version = !version.is_empty()
            && version.len() <= 64
            && version.bytes().all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
            && version.starts_with(|c: char| c.is_ascii_digit());
        if !ok_version {
            return Err(format!("{version}: not a version"));
        }
        Ok(Spec { name: name.into(), version: version.into(), app: app.into() })
    }

    /// Its file's path in the sparse index (after its root): `1/a`, `2/ab`,
    /// `3/a/abc`, or `ab/cd/abcd...`.
    pub fn index_path(&self) -> String {
        let n = self.name.to_ascii_lowercase();
        match n.len() {
            1 => format!("1/{n}"),
            2 => format!("2/{n}"),
            3 => format!("3/{}/{n}", &n[..1]),
            _ => format!("{}/{}/{n}", &n[..2], &n[2..4]),
        }
    }
}

impl std::fmt::Display for Spec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "crate:{}@{}", self.name, self.version)
    }
}

/// A prebuilt module the metadata pins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub url: String,
    pub sha256: String,
}

/// What a catalog crate's Cargo.toml says of its app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub title: String,
    pub color: Option<String>,
    /// Its access words, each once, in its order.
    pub access: Vec<String>,
    /// Whether it needs neochumbyd: `required` (always, with a companion)
    /// or `optional`.
    pub neochumbyd: String,
    /// Its companion's hosts.
    pub network: Vec<String>,
    pub repository: String,
    pub app: Module,
    pub companion: Option<Module>,
}

impl Meta {
    /// Reads and checks the Cargo.toml of `spec`'s `.crate`.
    pub fn parse(cargo_toml: &str, spec: &Spec) -> Result<Meta, String> {
        let t: toml::Table = toml::from_str(cargo_toml).map_err(|e| format!("its Cargo.toml: {e}"))?;
        let pkg = t.get("package").and_then(|p| p.as_table()).ok_or("its Cargo.toml has no [package]")?;
        let s = |k: &str| pkg.get(k).and_then(|v| v.as_str());
        if s("name") != Some(&spec.name) || s("version") != Some(&spec.version) {
            return Err(format!("its Cargo.toml is for another crate or version than {spec}"));
        }
        let keywords = pkg.get("keywords").and_then(|k| k.as_array());
        if !keywords.is_some_and(|k| k.iter().any(|w| w.as_str() == Some(KEYWORD))) {
            return Err(format!("it isn't a catalog app: it has no \"{KEYWORD}\" keyword"));
        }
        let repository = s("repository").ok_or("it names no repository")?;
        if !repository.starts_with("https://") {
            return Err(format!("its repository, {repository}, isn't an HTTPS URL"));
        }
        let m = pkg
            .get("metadata")
            .and_then(|m| m.get("neochumby"))
            .and_then(|m| m.as_table())
            .ok_or("it has no [package.metadata.neochumby]")?;
        if names_aot(m) {
            return Err("its metadata names an .aot: compiled code is never taken from an author".into());
        }
        let text = |k: &str| -> Result<Option<String>, String> {
            match m.get(k) {
                None => Ok(None),
                Some(v) => match v.as_str() {
                    Some(v) if value_ok(v) => Ok(Some(v.to_string())),
                    _ => Err(format!("its {k} isn't a line of text without =")),
                },
            }
        };
        let words = |k: &str| -> Result<Vec<String>, String> {
            let Some(v) = m.get(k) else { return Ok(Vec::new()) };
            let list = v.as_array().ok_or(format!("its {k} isn't a list"))?;
            let mut out: Vec<String> = Vec::new();
            for w in list {
                let w =
                    w.as_str().filter(|w| value_ok(w) && !w.contains(',')).ok_or(format!("its {k} has a bad entry"))?;
                if !out.iter().any(|o| o == w) {
                    out.push(w.to_string());
                }
            }
            Ok(out)
        };

        let title = text("title")?.filter(|t| !t.trim().is_empty()).ok_or("it has no title")?;
        if title.chars().count() > 32 {
            return Err("its title is longer than 32 characters".into());
        }
        let color = text("color")?;
        if color.as_ref().is_some_and(|c| c.len() != 6 || !c.bytes().all(|b| b.is_ascii_hexdigit())) {
            return Err("its color isn't six hex digits (336699)".into());
        }
        let access = words("access")?;
        for w in &access {
            if REFUSED.contains(&w.as_str()) {
                let why = if w == "network" {
                    "; a catalog app's network is its companion's, host by host (network = [...])"
                } else {
                    ""
                };
                return Err(format!("it asks for \"{w}\", which a catalog app is never granted{why}"));
            }
            if !ALLOWED.contains(&w.as_str()) {
                return Err(format!("it asks for \"{w}\", which isn't an access word a catalog app may have"));
            }
        }
        let network = words("network")?;
        if let Some(h) = network.iter().find(|h| !valid_host(h)) {
            return Err(format!("its network names {h}, which isn't a host name"));
        }

        let wasm = m.get("wasm").and_then(|w| w.as_table()).ok_or("it has no [package.metadata.neochumby.wasm]")?;
        if let Some(k) = wasm.keys().find(|k| *k != "app" && *k != "companion") {
            return Err(format!("its wasm table has {k}: only app and companion"));
        }
        let module = |k: &str| -> Result<Option<Module>, String> {
            let Some(v) = wasm.get(k) else { return Ok(None) };
            let t = v.as_table().ok_or(format!("its wasm.{k} isn't {{ url, sha256 }}"))?;
            let url = t.get("url").and_then(|u| u.as_str()).ok_or(format!("its wasm.{k} has no url"))?;
            let sum = t.get("sha256").and_then(|u| u.as_str()).ok_or(format!("its wasm.{k} has no sha256"))?;
            // HTTPS only: Http::check_url, before anything is fetched.
            if !value_ok(url) {
                return Err(format!("its wasm.{k} isn't a URL: {url}"));
            }
            if sum.len() != 64 || !sum.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("its wasm.{k}'s sha256 isn't 64 hex digits"));
            }
            Ok(Some(Module { url: url.into(), sha256: sum.to_ascii_lowercase() }))
        };
        let app = module("app")?.ok_or("its wasm table has no app")?;
        let companion = module("companion")?;

        let neochumbyd =
            text("neochumbyd")?.unwrap_or_else(|| if companion.is_some() { "required" } else { "optional" }.into());
        match neochumbyd.as_str() {
            "required" => {}
            "optional" if companion.is_none() => {}
            "optional" => return Err("it has a companion, so neochumbyd = \"required\"".into()),
            n => return Err(format!("its neochumbyd is {n}: required or optional")),
        }
        if !network.is_empty() && companion.is_none() {
            return Err("it names network hosts but has no companion to use them".into());
        }
        Ok(Meta { title, color, access, neochumbyd, network, repository: repository.into(), app, companion })
    }

    /// GitHub's owner and repository, if the repository is on GitHub.
    pub fn github(&self) -> Option<(String, String)> {
        let rest = self.repository.strip_prefix("https://github.com/")?;
        let mut parts = rest.trim_end_matches('/').split('/');
        let owner = parts.next().filter(|o| !o.is_empty())?;
        let repo = parts.next().map(|r| r.trim_end_matches(".git")).filter(|r| !r.is_empty())?;
        let ok = |s: &str| s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        (ok(owner) && ok(repo)).then(|| (owner.to_string(), repo.to_string()))
    }
}

/// Whether any key at or below `t` is `aot`.
fn names_aot(t: &toml::Table) -> bool {
    t.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("aot")
            || v.as_table().is_some_and(names_aot)
            || v.as_array().is_some_and(|a| a.iter().any(|e| e.as_table().is_some_and(names_aot)))
    })
}

/// Whether `v` may be a manifest value: one line, no `=`.
fn value_ok(v: &str) -> bool {
    !v.is_empty() && v.len() <= 1024 && !v.contains(['=', '\n', '\r', '\0'])
}

/// Whether `h` is a DNS name a companion may be allowed to reach.
fn valid_host(h: &str) -> bool {
    let labels: Vec<&str> = h.split('.').collect();
    h.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            (1..=63).contains(&l.len())
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && !labels.last().is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
}

/// What an installed catalog app's manifest says, for an update.
#[derive(Clone, Debug, Default)]
pub struct Installed {
    /// `crate=` (`neochumby-weather@1.2.0`).
    pub krate: String,
    pub access: Vec<String>,
    pub network: Vec<String>,
}

/// A manifest's key=value lines.
fn fields(manifest: &str) -> Vec<(&str, &str)> {
    manifest.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim(), v.trim())).collect()
}

fn list(v: Option<&str>) -> Vec<String> {
    v.unwrap_or_default().split(',').map(str::trim).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// The catalog app `apps/<app>/` in `tree`: None if there is nothing
/// there, an error if there is something that isn't a catalog app.
pub fn installed(tree: &Path, app: &str) -> Result<Option<Installed>, String> {
    let dir = tree.join("apps").join(app);
    if !dir.exists() {
        return Ok(None);
    }
    let m = std::fs::read_to_string(dir.join("manifest")).unwrap_or_default();
    let f = fields(&m);
    let get = |k: &str| f.iter().find(|(key, _)| *key == k).map(|(_, v)| *v);
    if get("source") != Some("catalog") {
        return Err(format!("apps/{app} is in the tree and isn't a catalog app: left as it is"));
    }
    Ok(Some(Installed {
        krate: get("crate").unwrap_or_default().into(),
        access: list(get("access")),
        network: list(get("network")),
    }))
}

/// The catalog apps in `tree`, by directory, in order.
pub fn catalog_apps(tree: &Path) -> Vec<(String, Installed)> {
    let mut out: Vec<(String, Installed)> = std::fs::read_dir(tree.join("apps"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let app = e.file_name().to_string_lossy().into_owned();
            let i = valid_name(&app).then(|| installed(tree, &app).ok().flatten()).flatten()?;
            Some((app, i))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// An app checked and ready to install, waiting on the owner.
pub struct Plan {
    pub spec: Spec,
    pub meta: Meta,
    pub wasm: Vec<u8>,
    pub companion: Option<Vec<u8>>,
    /// The attestation of the app's `.wasm` that verified, or why none
    /// did: a label.
    pub attestation: Result<Verified, String>,
    /// Its owners on crates.io (their logins), if crates.io said.
    pub owners: Option<Vec<String>>,
    /// Its downloads, all and recent, if crates.io said.
    pub downloads: Option<(u64, u64)>,
    /// The wamrc that will compile it, if any: else it runs interpreted.
    pub wamrc: Option<Wamrc>,
    /// What is installed now, for an update.
    pub previous: Option<Installed>,
}

/// What the owner said on the command line.
#[derive(Clone, Debug, Default)]
pub struct Approval {
    pub yes: bool,
    /// `--access WORDS`: its access words and its companion's hosts,
    /// comma-separated (`none` for nothing).
    pub access: Option<String>,
}

/// Fetches and checks `spec`, for `tree`: everything but the owner's
/// approval and the writing. Nothing is written.
pub fn prepare(http: &Http, spec: &Spec, tree: &Path, wamrc: Option<Wamrc>) -> Result<Plan, String> {
    let previous = installed(tree, &spec.app)?;
    let cargo_toml = crate_toml(http, spec)?;
    let meta = Meta::parse(&cargo_toml, spec).map_err(|e| format!("{spec}: {e}"))?;
    for (what, m) in [("app", Some(&meta.app)), ("companion", meta.companion.as_ref())] {
        if let Some(m) = m {
            http.check_url(&m.url).map_err(|e| format!("{spec}: its wasm.{what}: {e}"))?;
        }
    }

    // Its modules, each checked and kept.
    let fetch = |what: &str, m: &Module| -> Result<Vec<u8>, String> {
        let bytes = http.get_ok(&m.url, WASM_LIMIT).map_err(|e| format!("{spec}: its {what}: {e}"))?;
        if sha256(&bytes) != m.sha256 {
            return Err(format!("{spec}: its {what} isn't the one its Cargo.toml pins (sha256): refused"));
        }
        if !is_bytecode(&bytes) {
            return Err(format!("{spec}: its {what} isn't WebAssembly bytecode: refused"));
        }
        Ok(bytes)
    };
    let wasm = fetch("app .wasm", &meta.app)?;
    let companion = meta.companion.as_ref().map(|c| fetch("companion .wasm", c)).transpose()?;

    // The label, and what crates.io says of it: shown, never required.
    let attestation = attestation(http, &meta);
    let (owners, downloads) = (owners(http, spec), downloads(http, spec));

    Ok(Plan { spec: spec.clone(), meta, wasm, companion, attestation, owners, downloads, wamrc, previous })
}

/// Its owners on crates.io (their logins), if crates.io says.
pub fn owners(http: &Http, spec: &Spec) -> Option<Vec<String>> {
    let v = http.get_json(&format!("{}/v1/crates/{}/owners", http.sources.crates_api, spec.name)).ok()?;
    Some(v["users"].as_array().into_iter().flatten().filter_map(|u| u["login"].as_str().map(str::to_string)).collect())
}

/// Its downloads, all and recent, if crates.io says.
pub fn downloads(http: &Http, spec: &Spec) -> Option<(u64, u64)> {
    let v = http.get_json(&format!("{}/v1/crates/{}", http.sources.crates_api, spec.name)).ok()?;
    Some((v["crate"]["downloads"].as_u64()?, v["crate"]["recent_downloads"].as_u64().unwrap_or(0)))
}

/// Whether one of GitHub's attestations for `meta`'s app `.wasm` verifies,
/// as built by a workflow of its repository; else why not.
pub fn attestation(http: &Http, meta: &Meta) -> Result<Verified, String> {
    let (owner, repo) = meta.github().ok_or("its repository isn't on GitHub")?;
    let url = format!("{}/repos/{owner}/{repo}/attestations/sha256:{}", http.sources.github_api, meta.app.sha256);
    let r = http.get(&url, JSON_LIMIT, None)?;
    if r.status == 404 {
        return Err(format!("no attestation of its .wasm in {}", meta.repository));
    }
    if r.status != 200 {
        return Err(format!("GitHub didn't say (HTTP {})", r.status));
    }
    let v: serde_json::Value = serde_json::from_slice(&r.body).map_err(|e| format!("GitHub's answer: {e}"))?;
    let expect = Expect { owner: &owner, repo: &repo, sha256: &meta.app.sha256 };
    let mut why = format!("no attestation of its .wasm in {}", meta.repository);
    for a in v["attestations"].as_array().into_iter().flatten().take(16) {
        match sigstore::verify(&http.trust, &a["bundle"], &expect) {
            Ok(v) => return Ok(v),
            Err(e) => why = format!("its attestation doesn't verify: {e}"),
        }
    }
    Err(why)
}

/// The Cargo.toml of `spec`'s `.crate`: the version must be in crates.io's
/// index and not yanked, and the `.crate` must be the one the index's
/// checksum names. Nothing in it is checked yet ([`Meta::parse`]).
pub fn crate_toml(http: &Http, spec: &Spec) -> Result<String, String> {
    let src = &http.sources;
    // The index: is this version there, and what is its checksum?
    let index_url = format!("{}{}", src.index, spec.index_path());
    let index = http.get_ok(&index_url, CRATE_LIMIT).map_err(|e| format!("{spec}: crates.io's index: {e}"))?;
    let entry = String::from_utf8_lossy(&index)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["vers"].as_str() == Some(&spec.version) && v["name"].as_str() == Some(&spec.name))
        .ok_or_else(|| format!("{spec}: no such version in crates.io's index"))?;
    if entry["yanked"].as_bool() == Some(true) {
        return Err(format!("{spec}: that version is yanked"));
    }
    let cksum =
        entry["cksum"].as_str().filter(|c| c.len() == 64).ok_or_else(|| format!("{spec}: no checksum in the index"))?;

    // The crate, checked against it, and its Cargo.toml.
    let crate_url = format!("{}/{n}/{n}-{v}.crate", src.static_crates, n = spec.name, v = spec.version);
    let krate = http.get_ok(&crate_url, CRATE_LIMIT)?;
    if !sha256(&krate).eq_ignore_ascii_case(cksum) {
        return Err(format!("{spec}: the .crate isn't the one crates.io's index records: refused"));
    }
    unpack_cargo_toml(&krate, spec)
}

/// The Cargo.toml in a `.crate` (`NAME-VERSION/Cargo.toml`), refusing one
/// that ships an `.aot`.
fn unpack_cargo_toml(krate: &[u8], spec: &Spec) -> Result<String, String> {
    let want = format!("{}-{}/Cargo.toml", spec.name, spec.version);
    let gz = flate2::read::GzDecoder::new(krate).take(UNPACKED_LIMIT);
    let mut tar = tar::Archive::new(gz);
    let mut found = None;
    let bad = |e: std::io::Error| format!("{spec}: the .crate: {e}");
    for entry in tar.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        let path = entry.path().map_err(bad)?.to_string_lossy().into_owned();
        if path.to_ascii_lowercase().ends_with(".aot") {
            return Err(format!("{spec}: it ships {path}: compiled code is never taken from an author"));
        }
        if path == want {
            let mut s = String::new();
            entry.by_ref().take(1 << 20).read_to_string(&mut s).map_err(bad)?;
            found = Some(s);
        }
    }
    found.ok_or_else(|| format!("{spec}: the .crate has no {want}"))
}

/// Comma-separated words as a set; `none` is none.
pub fn set(s: &str) -> BTreeSet<String> {
    s.split(',').map(str::trim).filter(|w| !w.is_empty() && *w != "none").map(str::to_string).collect()
}

/// `w` as one word, comma-separated: `none` if it is empty.
pub fn words(w: &[String]) -> String {
    if w.is_empty() { "none".into() } else { w.join(",") }
}

impl Plan {
    /// The access words and hosts it asks for that the installed version
    /// wasn't granted: all of them for a new install.
    pub fn new_asks(&self) -> (Vec<String>, Vec<String>) {
        let had = self.previous.clone().unwrap_or_default();
        let new = |asks: &[String], had: &[String]| asks.iter().filter(|w| !had.contains(w)).cloned().collect();
        (new(&self.meta.access, &had.access), new(&self.meta.network, &had.network))
    }

    /// What it asks for: its access words, then its companion's hosts.
    pub fn asks(&self) -> Vec<String> {
        [self.meta.access.clone(), self.meta.network.clone()].concat()
    }

    /// Whether it is a verified build.
    pub fn verified(&self) -> bool {
        self.attestation.is_ok()
    }

    /// The command that installs it without asking.
    pub fn command(&self) -> String {
        format!("neochumbyd install {} --yes --access {}", self.spec, words(&self.asks()))
    }

    /// What the owner is shown before anything is written.
    pub fn summary(&self) -> String {
        let m = &self.meta;
        let mut s = format!("{}  {} {}\n", m.title, self.spec.name, self.spec.version);
        let publisher = match &self.owners {
            Some(o) if !o.is_empty() => o.join(", "),
            Some(_) => "no one listed".into(),
            None => "unknown (crates.io didn't say)".into(),
        };
        s += &format!("  published by  {publisher}, on crates.io\n");
        if let Some((all, recent)) = self.downloads {
            s += &format!("  downloads     {all} ({recent} in the last 90 days)\n");
        }
        s += &format!("  repository    {}\n", m.repository);
        s += &match &self.attestation {
            Ok(v) => format!(
                "  build         verified build: its .wasm was built by {} in that repository, signed and logged with Sigstore\n",
                v.workflow
            ),
            Err(why) => format!("  build         UNVERIFIED: {why}\n"),
        };
        let none = |v: &[String]| if v.is_empty() { "none".to_string() } else { v.join(", ") };
        s += &format!("  access        {}\n", none(&m.access));
        if m.companion.is_some() {
            s += &format!("  companion     yes, on this server; network: {}\n", none(&m.network));
        }
        s += &format!("  neochumbyd    {}\n", m.neochumbyd);
        s += &match &self.wamrc {
            Some(w) => format!("  runs          compiled (wamrc, WAMR {})\n", w.id),
            None => {
                "  runs          interpreted, slowly: no wamrc here to compile it (--wamrc or NEOCHUMBY_WAMRC)\n".into()
            }
        };
        if let Some(p) = &self.previous {
            let (access, hosts) = self.new_asks();
            s += &format!(
                "  updates       {} (installed now)\n",
                if p.krate.is_empty() { "an unknown version" } else { &p.krate }
            );
            if access.is_empty() && hosts.is_empty() {
                s += "                it asks for nothing more\n";
            } else {
                s += &format!("                it asks for more: {}\n", [access, hosts].concat().join(", "));
            }
        }
        s
    }

    /// Whether it asks for anything the installed version wasn't granted:
    /// always, for a new install.
    pub fn asks_more(&self) -> bool {
        let (access, hosts) = self.new_asks();
        self.previous.is_none() || !access.is_empty() || !hosts.is_empty()
    }

    /// Whether `a`, from the command line, approves it: `--yes`, and for a
    /// new install, or an update asking for more, `--access` naming exactly
    /// what it asks, its access words and its companion's hosts. Given,
    /// it must match even when not needed.
    pub fn approve(&self, a: &Approval) -> Result<(), String> {
        let refuse = |why: String| Err(format!("{why}; to approve it: {}", self.command()));
        if !a.yes {
            return refuse("not approved".into());
        }
        let asks: BTreeSet<String> = self.asks().into_iter().collect();
        match &a.access {
            Some(given) if set(given) != asks => {
                return refuse(format!("--access {given} isn't what it asks for ({})", words(&self.asks())));
            }
            None if self.asks_more() => {
                return refuse("--yes needs --access, naming the access and hosts it asks for".into());
            }
            _ => {}
        }
        Ok(())
    }

    /// The tree manifest it gets.
    pub fn manifest(&self, aot: Option<(&str, &str)>) -> String {
        let m = &self.meta;
        let mut lines = vec![format!("title={}", m.title)];
        if let Some(c) = &m.color {
            lines.push(format!("color={c}"));
        }
        lines.push("wasm=app.wasm".into());
        if let Some((key, sum)) = aot {
            lines.push("aot=app.aot".into());
            lines.push(format!("aot-key={key}"));
            lines.push(format!("aot-sha256={sum}"));
        }
        if !m.access.is_empty() {
            lines.push(format!("access={}", m.access.join(",")));
        }
        lines.push(format!("neochumbyd={}", m.neochumbyd));
        if self.companion.is_some() {
            lines.push("companion=companion.wasm".into());
        }
        if !m.network.is_empty() {
            lines.push(format!("network={}", m.network.join(",")));
        }
        lines.push("source=catalog".into());
        lines.push(format!("crate={}@{}", self.spec.name, self.spec.version));
        lines.push(format!("verified={}", if self.verified() { "yes" } else { "no" }));
        lines.iter().map(|l| format!("{l}\n")).collect()
    }

    /// Compiles it, if there is a wamrc (in `scratch`), and puts it in
    /// `tree` as apps/<app>/, in place of the version there: the
    /// directory. A wamrc that fails leaves it to the interpreter, with a
    /// line on stderr saying so.
    pub fn write(&self, tree: &Path, scratch: &Path) -> Result<PathBuf, String> {
        let aot = self.wamrc.as_ref().and_then(|w| match w.compile(&self.wasm, scratch) {
            Ok((aot, isolated)) => {
                if isolated == Some(false) {
                    eprintln!("neochumbyd: wamrc ran with the network (this kernel allows no user namespace)");
                }
                Some((aot, w.key(&self.wasm)))
            }
            Err(e) => {
                eprintln!("neochumbyd: {}: {e}; it will run interpreted, slowly", self.spec);
                None
            }
        });
        let apps = tree.join("apps");
        std::fs::create_dir_all(&apps).map_err(|e| format!("{}: {e}", apps.display()))?;
        let dest = apps.join(&self.spec.app);
        let tmp = apps.join(format!(".{}.new-{}-{}", self.spec.app, std::process::id(), super::unique()));
        let io = |e: std::io::Error| format!("{}: {e}", tmp.display());
        let r = (|| {
            std::fs::create_dir(&tmp).map_err(io)?;
            std::fs::write(tmp.join("app.wasm"), &self.wasm).map_err(io)?;
            if let Some(c) = &self.companion {
                std::fs::write(tmp.join("companion.wasm"), c).map_err(io)?;
            }
            let aot_lines = match &aot {
                Some((bytes, key)) => {
                    std::fs::write(tmp.join("app.aot"), bytes).map_err(io)?;
                    Some((key.clone(), sha256(bytes)))
                }
                None => None,
            };
            let manifest = self.manifest(aot_lines.as_ref().map(|(k, s)| (k.as_str(), s.as_str())));
            std::fs::write(tmp.join("manifest"), manifest).map_err(io)?;
            replace_dir(&tmp, &dest)
        })();
        if r.is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
        }
        r.map(|()| dest)
    }
}

/// Puts the directory `new` at `dest`, by rename: the one there before, if
/// any, is moved aside first and removed after.
fn replace_dir(new: &Path, dest: &Path) -> Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", dest.display());
    let old = dest.with_file_name(format!(
        ".{}.old-{}-{}",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        super::unique()
    ));
    let had = dest.exists();
    if had {
        std::fs::rename(dest, &old).map_err(io)?;
    }
    if let Err(e) = std::fs::rename(new, dest) {
        if had {
            let _ = std::fs::rename(&old, dest);
        }
        return Err(io(e));
    }
    if had {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(())
}

/// Removes the catalog app `app` from `tree`; any other is refused.
pub fn uninstall(tree: &Path, app: &str) -> Result<(), String> {
    if !valid_name(app) {
        return Err(format!("{app}: not an app's name"));
    }
    if installed(tree, app)?.is_none() {
        return Err(format!("no app {app} in the tree"));
    }
    let dir = tree.join("apps").join(app);
    let gone = tree.join("apps").join(format!(".{app}.gone-{}-{}", std::process::id(), super::unique()));
    std::fs::rename(&dir, &gone).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::remove_dir_all(&gone).map_err(|e| format!("{}: {e}", gone.display()))
}
