//! The catalog against a fake crates.io and GitHub on 127.0.0.1: none of
//! these reach the internet.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::discover::{self, rank, repo_key};
use super::http::{Http, Sources};
use super::install::{self, Approval, Plan, Spec, prepare};
use super::sigstore::tests::{Fake as Sigstore, Make};
use super::update::{self, Outcome};
use super::wamrc::Wamrc;
use super::{recompile, sha256};

/// A fresh, empty directory for one test.
pub fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("neochumbyd-catalog-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A wamrc in `dir` that runs `body` (sh), with a wamrc.id.
pub fn fake_wamrc(dir: &Path, body: &str) -> Wamrc {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("wamrc");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.join("wamrc.id"), "2.4.3-0123456789ab\n").unwrap();
    Wamrc::at(path).unwrap()
}

/// Each request the fake server had: its path and headers.
type Seen = Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>;

/// An answer the fake server gives.
#[derive(Clone)]
struct Route {
    status: u16,
    body: Vec<u8>,
    etag: Option<String>,
}

/// crates.io's and GitHub's stand-in: answers GETs by path (and query),
/// and keeps each request's path and headers. Its attestations are signed
/// by a Sigstore of its own.
pub struct Fake {
    pub base: String,
    routes: Arc<Mutex<HashMap<String, Route>>>,
    seen: Seen,
    sigstore: Arc<Sigstore>,
}

impl Fake {
    pub fn start() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Arc<Mutex<HashMap<String, Route>>> = Arc::default();
        let seen: Seen = Arc::default();
        let (r, s) = (routes.clone(), seen.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    continue;
                }
                let path = first.split_whitespace().nth(1).unwrap_or_default().to_string();
                let mut headers = Vec::new();
                loop {
                    let mut l = String::new();
                    if reader.read_line(&mut l).unwrap_or(0) == 0 || l.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = l.split_once(':') {
                        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
                    }
                }
                let inm = headers.iter().find(|(k, _)| k == "if-none-match").map(|(_, v)| v.clone());
                s.lock().unwrap().push((path.clone(), headers));
                let route = r.lock().unwrap().get(&path).cloned();
                let (status, body, etag) = match route {
                    Some(rt) if rt.etag.is_some() && rt.etag == inm => (304, Vec::new(), rt.etag),
                    Some(rt) => (rt.status, rt.body, rt.etag),
                    None => (404, b"{}".to_vec(), None),
                };
                let mut head =
                    format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
                if let Some(e) = etag {
                    head += &format!("ETag: {e}\r\n");
                }
                head += "\r\n";
                let _ = conn.write_all(head.as_bytes());
                let _ = conn.write_all(&body);
            }
        });
        Fake { base, routes, seen, sigstore: Arc::new(Sigstore::new()) }
    }

    fn route(&self, path: &str, status: u16, body: impl Into<Vec<u8>>, etag: Option<&str>) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.into(), Route { status, body: body.into(), etag: etag.map(str::to_string) });
    }

    pub fn serve(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.route(path, 200, body, None);
    }

    pub fn http(&self) -> Http {
        Http::with_token(Sources::local(&self.base), Some("t0ken".into())).trusting(self.sigstore.trust())
    }

    pub fn requests(&self, path: &str) -> Vec<Vec<(String, String)>> {
        self.seen.lock().unwrap().iter().filter(|(p, _)| p == path).map(|(_, h)| h.clone()).collect()
    }
}

/// A module that is bytecode.
fn wasm(tag: &str) -> Vec<u8> {
    [b"\0asm\x01\0\0\0".as_slice(), tag.as_bytes()].concat()
}

/// What a test publishes: a catalog crate on the fake crates.io.
pub struct Publish<'a> {
    pub version: &'a str,
    /// Lines under [package.metadata.neochumby], before its wasm table.
    pub meta: &'a str,
    /// The app's wasm table entry, if not the usual one.
    pub app: Option<String>,
    /// The app module's bytes, if not `wasm(version)`.
    pub bytes: Option<Vec<u8>>,
    /// The bytes the index's checksum is of, if not the .crate's.
    pub cksum_of: Option<&'a [u8]>,
    pub yanked: bool,
    pub extra_files: &'a [&'a str],
}

impl Default for Publish<'_> {
    fn default() -> Self {
        Publish {
            version: "1.2.0",
            meta: "title = \"WEATHER\"\ncolor = \"336699\"\naccess = [\"fb\", \"touch\"]\n",
            app: None,
            bytes: None,
            cksum_of: None,
            yanked: false,
            extra_files: &[],
        }
    }
}

const NAME: &str = "neochumby-weather";

/// Builds a `.crate` of `files` (path under NAME-VERSION/, contents).
fn dot_crate(version: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (path, body) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append_data(&mut h, format!("{NAME}-{version}/{path}"), *body).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

impl Fake {
    /// Publishes `p` (and keeps the versions published before it in the
    /// index); the app's bytes.
    pub fn publish(&self, p: Publish) -> Vec<u8> {
        let bytes = p.bytes.clone().unwrap_or_else(|| wasm(p.version));
        let dl = format!("/dl/app-{}.wasm", p.version);
        self.serve(&dl, bytes.clone());
        let app = p
            .app
            .unwrap_or_else(|| format!("app = {{ url = \"{}{dl}\", sha256 = \"{}\" }}", self.base, sha256(&bytes)));
        let toml = format!(
            "[package]\nname = \"{NAME}\"\nversion = \"{v}\"\nrepository = \"https://github.com/someone/neochumby-weather\"\nkeywords = [\"neochumby-app\"]\n\n[package.metadata.neochumby]\n{meta}\n[package.metadata.neochumby.wasm]\n{app}\n",
            v = p.version,
            meta = p.meta,
        );
        let mut files: Vec<(&str, &[u8])> = vec![("Cargo.toml", toml.as_bytes()), ("src/main.rs", b"fn main() {}\n")];
        files.extend(p.extra_files.iter().map(|f| (*f, b"x".as_slice())));
        let krate = dot_crate(p.version, &files);
        self.serve(&format!("/static/{NAME}/{NAME}-{}.crate", p.version), krate.clone());
        let line = serde_json::json!({
            "name": NAME, "vers": p.version, "deps": [], "features": {},
            "cksum": sha256(p.cksum_of.unwrap_or(&krate)), "yanked": p.yanked,
        });
        let path = "/index/ne/oc/neochumby-weather";
        let mut index = self.routes.lock().unwrap().get(path).map(|r| r.body.clone()).unwrap_or_default();
        index.extend(format!("{line}\n").as_bytes());
        self.serve(path, index);
        self.serve(&format!("/api/v1/crates/{NAME}/owners"), r#"{"users":[{"login":"someone","name":"Some One"}]}"#);
        self.serve(&format!("/api/v1/crates/{NAME}"), r#"{"crate":{"downloads":1234,"recent_downloads":56}}"#);
        bytes
    }

    /// GitHub's attestation of `bytes`, as its attest action makes one in
    /// the crate's repository.
    pub fn attest(&self, bytes: &[u8]) {
        self.attest_as(bytes, Make { sha256: sha256(bytes), ..Make::default() });
    }

    /// An attestation for `bytes`, as `m` makes it.
    fn attest_as(&self, bytes: &[u8], m: Make) {
        let path = format!("/github/repos/someone/neochumby-weather/attestations/sha256:{}", sha256(bytes));
        let body = serde_json::json!({"attestations": [{"bundle": self.sigstore.bundle(&m), "repository_id": 1}]});
        self.serve(&path, body.to_string());
    }

    /// The cache `catalog refresh` would keep, listing `version`.
    pub fn list(&self, state: &Path, version: &str) {
        self.serve(
            "/api/v1/crates?keyword=neochumby-app&sort=downloads&per_page=100&page=1",
            format!(
                r#"{{"crates":[{{"name":"{NAME}","max_version":"{version}","max_stable_version":"{version}","description":"Forecasts, by touch","repository":"https://github.com/someone/neochumby-weather","downloads":1234,"recent_downloads":56}}],"meta":{{"next_page":null}}}}"#
            ),
        );
        std::fs::create_dir_all(state).unwrap();
        discover::refresh(&self.http(), state).unwrap();
    }
}

fn spec(v: &str) -> Spec {
    Spec::parse(&format!("crate:{NAME}@{v}")).unwrap()
}

fn yes(access: &str) -> Approval {
    Approval { yes: true, access: Some(access.into()) }
}

fn install(fake: &Fake, tree: &Path, v: &str, a: &Approval, wamrc: Option<Wamrc>) -> Result<Plan, String> {
    let plan = prepare(&fake.http(), &spec(v), tree, wamrc)?;
    plan.approve(a)?;
    plan.write(tree, &tree.join("../scratch"))?;
    Ok(plan)
}

#[test]
fn names_are_pinned_and_indexed_by_crates_io_s_rules() {
    assert!(Spec::parse("crate:neochumby-weather").unwrap_err().contains("which version?"));
    assert!(Spec::parse("neochumby-weather@1.0.0").is_err());
    assert!(Spec::parse("crate:serde@1.0.0").is_err());
    assert!(Spec::parse("crate:neochumby-Weather@1.0.0").is_err());
    assert!(Spec::parse("crate:neochumby-weather@latest").is_err());
    assert!(Spec::parse("crate:neochumby-weather@1.0.0/../x").is_err());
    let s = spec("1.2.0");
    assert_eq!((s.app.as_str(), s.index_path()), ("weather", "ne/oc/neochumby-weather".into()));
    let path = |n: &str| Spec { name: n.into(), version: "1".into(), app: String::new() }.index_path();
    assert_eq!([path("a"), path("ab"), path("abc"), path("Abcd")], ["1/a", "2/ab", "3/a/abc", "ab/cd/abcd"]);
}

#[test]
fn installs_checked_and_pinned_without_a_wamrc() {
    let (fake, dir) = (Fake::start(), temp("happy"));
    let tree = dir.join("tree");
    let bytes = fake.publish(Publish::default());
    fake.attest(&bytes);

    let plan = prepare(&fake.http(), &spec("1.2.0"), &tree, None).unwrap();
    // Nothing is written before it is approved.
    assert!(!tree.exists());
    let s = plan.summary();
    for want in [
        "WEATHER  neochumby-weather 1.2.0",
        "published by  someone",
        "1234 (56",
        "verified build",
        "access        fb, touch",
        "interpreted, slowly",
    ] {
        assert!(s.contains(want), "{want}: {s}");
    }
    plan.approve(&yes("fb,touch")).unwrap();
    plan.write(&tree, &dir.join("scratch")).unwrap();

    let app = tree.join("apps/weather");
    assert_eq!(std::fs::read(app.join("app.wasm")).unwrap(), bytes);
    assert!(!app.join("app.aot").exists());
    assert_eq!(
        std::fs::read_to_string(app.join("manifest")).unwrap(),
        "title=WEATHER\ncolor=336699\nwasm=app.wasm\naccess=fb,touch\nneochumbyd=optional\nsource=catalog\ncrate=neochumby-weather@1.2.0\nverified=yes\n"
    );
    // Only the temporary directory's rename put it there: nothing else is left.
    assert_eq!(std::fs::read_dir(tree.join("apps")).unwrap().count(), 1);
    // GitHub was asked with the token; crates.io and GitHub with our User-Agent.
    let attest = format!("/github/repos/someone/neochumby-weather/attestations/sha256:{}", sha256(&bytes));
    let h = &fake.requests(&attest)[0];
    assert!(h.contains(&("authorization".into(), "Bearer t0ken".into())));
    let ua = &fake.requests(&format!("/api/v1/crates/{NAME}/owners"))[0];
    assert!(ua.iter().any(|(k, v)| k == "user-agent"
        && v.contains("neochumbyd")
        && v.contains("github.com/johnmerchant/neochumby")));

    // Uninstalled, as a catalog app; nothing else is.
    install::uninstall(&tree, "weather").unwrap();
    assert!(!app.exists());
    std::fs::create_dir_all(tree.join("apps/clock")).unwrap();
    std::fs::write(tree.join("apps/clock/manifest"), "exec=neochumby-clock\n").unwrap();
    assert!(install::uninstall(&tree, "clock").unwrap_err().contains("isn't a catalog app"));
    assert!(install::uninstall(&tree, "nothing").is_err());
}

#[test]
fn unverified_and_with_a_companion() {
    let (fake, dir) = (Fake::start(), temp("companion"));
    let tree = dir.join("tree");
    let comp = wasm("companion");
    fake.serve("/dl/companion.wasm", comp.clone());
    let meta =
        "title = \"WEATHER\"\naccess = [\"fb\"]\nneochumbyd = \"required\"\nnetwork = [\"api.open-meteo.com\"]\n";
    let bytes = wasm("1.2.0");
    let app = format!(
        "app = {{ url = \"{b}/dl/app-1.2.0.wasm\", sha256 = \"{}\" }}\ncompanion = {{ url = \"{b}/dl/companion.wasm\", sha256 = \"{}\" }}",
        sha256(&bytes),
        sha256(&comp),
        b = fake.base
    );
    fake.publish(Publish { meta, app: Some(app), ..Publish::default() });
    let plan = prepare(&fake.http(), &spec("1.2.0"), &tree, None).unwrap();
    assert!(plan.summary().contains("UNVERIFIED: no attestation"), "{}", plan.summary());
    assert!(plan.summary().contains("network: api.open-meteo.com"));
    // Its hosts are approved too, as what it asks for.
    assert!(plan.approve(&yes("fb")).unwrap_err().contains("--access fb,api.open-meteo.com"));
    assert!(plan.approve(&yes("fb,evil.example.com")).is_err());
    plan.approve(&yes("api.open-meteo.com,fb")).unwrap();
    plan.write(&tree, &dir.join("scratch")).unwrap();
    let app = tree.join("apps/weather");
    assert_eq!(std::fs::read(app.join("companion.wasm")).unwrap(), comp);
    assert_eq!(
        std::fs::read_to_string(app.join("manifest")).unwrap(),
        "title=WEATHER\nwasm=app.wasm\naccess=fb\nneochumbyd=required\ncompanion=companion.wasm\nnetwork=api.open-meteo.com\nsource=catalog\ncrate=neochumby-weather@1.2.0\nverified=no\n"
    );
}

#[test]
fn compiles_with_wamrc_and_records_the_key() {
    let (fake, dir) = (Fake::start(), temp("aot"));
    let tree = dir.join("tree");
    let bytes = fake.publish(Publish::default());
    let args = dir.join("args");
    let body = format!(
        "echo \"$@\" > '{}'\nwhile [ $# -gt 0 ]; do [ \"$1\" = -o ] && out=$2; shift; done\nprintf '\\000aot native' > \"$out\"",
        args.display()
    );
    let w = fake_wamrc(&dir, &body);
    let plan = install(&fake, &tree, "1.2.0", &yes("touch,fb"), Some(w)).unwrap();
    assert!(plan.summary().contains("compiled (wamrc, WAMR 2.4.3-0123456789ab)"));

    assert_eq!(
        std::fs::read_to_string(&args).unwrap(),
        "--target=armv5te --target-abi=eabi --cpu=arm926ej-s --bounds-checks=1 -o app.aot app.wasm\n"
    );
    let app = tree.join("apps/weather");
    let aot = std::fs::read(app.join("app.aot")).unwrap();
    assert_eq!(aot, b"\0aot native");
    let key = format!("sha256:{}/wamr:2.4.3-0123456789ab/armv5te-eabi-arm926ej-s-bounds-nonatives", sha256(&bytes));
    assert_eq!(
        std::fs::read_to_string(app.join("manifest")).unwrap(),
        format!(
            "title=WEATHER\ncolor=336699\nwasm=app.wasm\naot=app.aot\naot-key={key}\naot-sha256={}\naccess=fb,touch\nneochumbyd=optional\nsource=catalog\ncrate=neochumby-weather@1.2.0\nverified=no\n",
            sha256(&aot)
        )
    );
    // Its scratch directory is gone.
    assert_eq!(std::fs::read_dir(tree.join("../scratch")).unwrap().count(), 0);

    // Another WAMR's wamrc: serve's start compiles it again, and records it.
    std::fs::write(dir.join("wamrc.id"), "2.5.0-fedcba987654\n").unwrap();
    let newer = Wamrc::at(dir.join("wamrc")).unwrap();
    recompile(&tree, &newer, &dir.join("scratch"));
    let m = std::fs::read_to_string(app.join("manifest")).unwrap();
    assert!(m.contains("aot-key=sha256:") && m.contains("/wamr:2.5.0-fedcba987654/"), "{m}");
    assert_eq!(m.matches("aot=").count(), 1);
    assert!(m.ends_with("verified=no\n"));
}

#[test]
fn a_crate_not_the_index_s_is_refused() {
    let (fake, dir) = (Fake::start(), temp("cksum"));
    fake.publish(Publish { cksum_of: Some(b"another crate"), ..Publish::default() });
    let e = prepare(&fake.http(), &spec("1.2.0"), &dir, None).err().unwrap();
    assert!(e.contains("isn't the one crates.io's index records"), "{e}");
}

#[test]
fn a_wasm_not_the_pinned_one_is_refused() {
    let (fake, dir) = (Fake::start(), temp("wasm-sha"));
    let app =
        format!("app = {{ url = \"{}/dl/app-1.2.0.wasm\", sha256 = \"{}\" }}", fake.base, sha256(b"something else"));
    fake.publish(Publish { app: Some(app), ..Publish::default() });
    let e = prepare(&fake.http(), &spec("1.2.0"), &dir, None).err().unwrap();
    assert!(e.contains("isn't the one its Cargo.toml pins"), "{e}");
}

#[test]
fn what_isn_t_bytecode_is_refused() {
    let dir = temp("not-wasm");
    // Pinned correctly, but an AOT module, or anything else, named .wasm.
    for bytes in [b"\0aot\x03\0\0\0native".to_vec(), b"\x7fELF".to_vec()] {
        let fake = Fake::start();
        fake.publish(Publish { bytes: Some(bytes), ..Publish::default() });
        let e = prepare(&fake.http(), &spec("1.2.0"), &dir, None).err().unwrap();
        assert!(e.contains("isn't WebAssembly bytecode"), "{e}");
    }
}

#[test]
fn a_yanked_version_or_one_not_published_is_refused() {
    let (fake, dir) = (Fake::start(), temp("yanked"));
    fake.publish(Publish { yanked: true, ..Publish::default() });
    assert!(prepare(&fake.http(), &spec("1.2.0"), &dir, None).err().unwrap().contains("yanked"));
    assert!(prepare(&fake.http(), &spec("9.9.9"), &dir, None).err().unwrap().contains("no such version"));
}

#[test]
fn refused_access_words() {
    let dir = temp("access");
    for (word, why) in [
        ("root", "never granted"),
        ("system", "never granted"),
        ("terminal", "never granted"),
        ("logs", "never granted"),
        ("network", "its companion's"),
        ("camera", "isn't an access word"),
    ] {
        let fake = Fake::start();
        let meta = format!("title = \"WEATHER\"\naccess = [\"fb\", \"{word}\"]\n");
        fake.publish(Publish { meta: &meta, ..Publish::default() });
        let e = prepare(&fake.http(), &spec("1.2.0"), &dir, None).err().unwrap();
        assert!(e.contains(&format!("\"{word}\"")) && e.contains(why), "{e}");
    }
    // Each allowed one is.
    let fake = Fake::start();
    let meta = "title = \"X\"\naccess = [\"fb\", \"touch\", \"speaker\", \"accel\"]\n";
    fake.publish(Publish { meta, ..Publish::default() });
    assert!(prepare(&fake.http(), &spec("1.2.0"), &dir, None).is_ok());
}

#[test]
fn an_aot_from_the_author_is_refused() {
    let dir = temp("author-aot");
    let fake = Fake::start();
    let bytes = wasm("1.2.0");
    let app = format!(
        "app = {{ url = \"{b}/dl/app-1.2.0.wasm\", sha256 = \"{}\" }}\naot = {{ url = \"{b}/dl/app.aot\", sha256 = \"{}\" }}",
        sha256(&bytes),
        sha256(b"x"),
        b = fake.base
    );
    fake.publish(Publish { app: Some(app), ..Publish::default() });
    assert!(prepare(&fake.http(), &spec("1.2.0"), &dir, None).err().unwrap().contains("names an .aot"));

    // Anywhere in the metadata.
    let fake = Fake::start();
    let meta = "title = \"WEATHER\"\naot = \"app.aot\"\n";
    fake.publish(Publish { meta, ..Publish::default() });
    assert!(prepare(&fake.http(), &spec("1.2.0"), &dir, None).err().unwrap().contains("names an .aot"));

    // Or among the crate's files.
    let fake = Fake::start();
    fake.publish(Publish { extra_files: &["prebuilt/app.AOT"], ..Publish::default() });
    assert!(prepare(&fake.http(), &spec("1.2.0"), &dir, None).err().unwrap().contains("ships"));
}

#[test]
fn only_https_modules() {
    let (fake, dir) = (Fake::start(), temp("http"));
    let app = format!("app = {{ url = \"http://example.com/app.wasm\", sha256 = \"{}\" }}", sha256(&wasm("1.2.0")));
    fake.publish(Publish { app: Some(app), ..Publish::default() });
    let e = prepare(&fake.http(), &spec("1.2.0"), &dir, None).err().unwrap();
    assert!(e.contains("only HTTPS"), "{e}");
}

#[test]
fn bad_metadata_is_refused() {
    let s = spec("1.2.0");
    let with = |meta: &str, wasm: &str| {
        format!(
            "[package]\nname = \"{NAME}\"\nversion = \"1.2.0\"\nrepository = \"https://github.com/a/b\"\nkeywords = [\"neochumby-app\"]\n[package.metadata.neochumby]\n{meta}\n[package.metadata.neochumby.wasm]\n{wasm}\n"
        )
    };
    let app = format!("app = {{ url = \"https://x.example/a.wasm\", sha256 = \"{}\" }}", "0".repeat(64));
    assert!(install::Meta::parse(&with("title = \"OK\"", &app), &s).is_ok());
    for (meta, wasm, why) in [
        ("", app.as_str(), "no title"),
        ("title = \"A=B\"", &app, "without ="),
        ("title = \"T\"\ncolor = \"blue\"", &app, "six hex"),
        ("title = \"T\"\nnetwork = [\"a.example\"]", &app, "no companion"),
        ("title = \"T\"\nneochumbyd = \"maybe\"", &app, "required or optional"),
        ("title = \"T\"", "app = { url = \"https://x/a.wasm\", sha256 = \"abc\" }", "64 hex"),
        ("title = \"T\"", "", "no app"),
    ] {
        let e = install::Meta::parse(&with(meta, wasm), &s).unwrap_err();
        assert!(e.contains(why), "{why}: {e}");
    }
    let c = format!("{app}\ncompanion = {{ url = \"https://x.example/c.wasm\", sha256 = \"{}\" }}", "1".repeat(64));
    let e = install::Meta::parse(&with("title = \"T\"\nneochumbyd = \"optional\"", &c), &s).unwrap_err();
    assert!(e.contains("required"), "{e}");
    let other = with("title = \"T\"", &app).replace("1.2.0", "1.0.0");
    assert!(install::Meta::parse(&other, &s).unwrap_err().contains("another crate"));
    let no_kw = with("title = \"T\"", &app).replace("\"neochumby-app\"", "\"game\"");
    assert!(install::Meta::parse(&no_kw, &s).unwrap_err().contains("keyword"));
}

#[test]
fn yes_needs_the_access_it_asks_for() {
    let (fake, dir) = (Fake::start(), temp("yes"));
    fake.publish(Publish::default());
    let plan = prepare(&fake.http(), &spec("1.2.0"), &dir.join("tree"), None).unwrap();
    assert!(plan.approve(&Approval::default()).unwrap_err().contains("--yes --access fb,touch"));
    let e = plan.approve(&Approval { yes: true, ..Approval::default() }).unwrap_err();
    assert!(e.contains("needs --access"), "{e}");
    assert!(plan.approve(&yes("fb")).unwrap_err().contains("isn't what it asks for"));
    assert!(plan.approve(&yes("fb,touch,speaker")).is_err());
    plan.approve(&yes("touch, fb")).unwrap();
}

#[test]
fn an_update_asking_for_more_is_approved_again() {
    let (fake, dir) = (Fake::start(), temp("update"));
    let tree = dir.join("tree");
    let fb = "title = \"WEATHER\"\naccess = [\"fb\"]\n";
    fake.publish(Publish { meta: fb, ..Publish::default() });
    install(&fake, &tree, "1.2.0", &yes("fb"), None).unwrap();

    // Asking nothing more: --yes is enough.
    fake.publish(Publish { version: "1.2.1", meta: fb, ..Publish::default() });
    let plan = prepare(&fake.http(), &spec("1.2.1"), &tree, None).unwrap();
    assert!(plan.summary().contains("updates       neochumby-weather@1.2.0"));
    plan.approve(&Approval { yes: true, ..Approval::default() }).unwrap();
    plan.write(&tree, &dir.join("scratch")).unwrap();

    // Asking for touch too: the old approval doesn't cover it.
    fake.publish(Publish { version: "1.3.0", ..Publish::default() });
    let plan = prepare(&fake.http(), &spec("1.3.0"), &tree, None).unwrap();
    assert_eq!(plan.new_asks(), (vec!["touch".to_string()], vec![]));
    assert!(plan.summary().contains("it asks for more: touch"));
    assert!(plan.approve(&Approval { yes: true, ..Approval::default() }).is_err());
    assert!(plan.approve(&yes("fb")).is_err());
    plan.approve(&yes("fb,touch")).unwrap();
    plan.write(&tree, &dir.join("scratch")).unwrap();
    let m = std::fs::read_to_string(tree.join("apps/weather/manifest")).unwrap();
    assert!(m.contains("access=fb,touch\n") && m.contains("crate=neochumby-weather@1.3.0\n"), "{m}");
    assert_eq!(std::fs::read(tree.join("apps/weather/app.wasm")).unwrap(), wasm("1.3.0"));
    assert_eq!(std::fs::read_dir(tree.join("apps")).unwrap().count(), 1);

    // Never over an app that isn't the catalog's.
    std::fs::create_dir_all(tree.join("apps/clock")).unwrap();
    std::fs::write(tree.join("apps/clock/manifest"), "exec=neochumby-clock\n").unwrap();
    let clock = Spec { name: "neochumby-clock".into(), version: "1.0.0".into(), app: "clock".into() };
    assert!(prepare(&fake.http(), &clock, &tree, None).err().unwrap().contains("isn't a catalog app"));
}

/// A crate in crates.io's search.
fn found(name: &str, repo: &str, recent: u64) -> serde_json::Value {
    serde_json::json!({
        "name": name, "max_version": "0.2.0-beta", "max_stable_version": "0.1.0",
        "description": format!("{name} "), "repository": repo,
        "downloads": recent * 3, "recent_downloads": recent, "yanked": false,
    })
}

#[test]
fn discovery_joins_and_ranks() {
    let (fake, dir) = (Fake::start(), temp("discover"));
    let page1 = serde_json::json!({
        "crates": [
            found("neochumby-weather", "https://github.com/Someone/Weather.git", 50),
            found("neochumby-clock2", "https://github.com/other/clock2/", 50),
            // Not a catalog app's name, whatever its keyword.
            found("someones-lib", "https://github.com/x/y", 900),
        ],
        "meta": { "total": 4, "next_page": "?keyword=neochumby-app&sort=downloads&per_page=100&page=2" },
    });
    let page2 = serde_json::json!({
        "crates": [ found("neochumby-snake", "https://gitlab.com/s/snake", 70) ],
        "meta": { "total": 4, "next_page": null },
    });
    fake.serve("/api/v1/crates?keyword=neochumby-app&sort=downloads&per_page=100&page=1", page1.to_string());
    fake.serve("/api/v1/crates?keyword=neochumby-app&sort=downloads&per_page=100&page=2", page2.to_string());
    let search = serde_json::json!({ "items": [
        { "html_url": "https://github.com/someone/weather", "stargazers_count": 9 },
        { "html_url": "https://github.com/other/clock2", "stargazers_count": 2 },
    ]});
    let search_path = "/github/search/repositories?q=topic:neochumby-app&per_page=100";
    fake.route(search_path, 200, search.to_string(), Some("\"v1\""));

    let c = discover::refresh(&fake.http(), &dir).unwrap();
    let names: Vec<&str> = c.apps.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["neochumby-snake", "neochumby-weather", "neochumby-clock2"]);
    let w = &c.apps[1];
    assert_eq!((w.app.as_str(), w.version.as_str(), w.stars), ("weather", "0.1.0", Some(9)));
    assert_eq!(w.spec(), "crate:neochumby-weather@0.1.0");
    assert_eq!((w.description.as_str(), w.downloads, w.recent_downloads), ("neochumby-weather", 150, 50));
    assert_eq!(c.apps[0].stars, None);
    assert_eq!(c.github.etag.as_deref(), Some("\"v1\""));
    assert!(c.fetched_at > 0);
    // Kept, and read back as it was.
    assert_eq!(discover::load(&dir).unwrap().unwrap(), c);
    assert_eq!(discover::load(&temp("no-cache")).unwrap(), None);

    // Unchanged on GitHub: asked with its ETag, and the stars kept.
    let c2 = discover::refresh(&fake.http(), &dir).unwrap();
    let asked = fake.requests(search_path);
    assert_eq!(asked.len(), 2);
    assert!(!asked[0].iter().any(|(k, _)| k == "if-none-match"));
    assert!(asked[1].contains(&("if-none-match".into(), "\"v1\"".into())));
    assert_eq!(c2.apps, c.apps);
    assert_eq!(c2.github, c.github);

    // Refused by GitHub (its rate limit): the stars from before.
    fake.route(search_path, 403, "{}", None);
    let c3 = discover::refresh(&fake.http(), &dir).unwrap();
    assert_eq!(c3.apps[1].stars, Some(9));
}

#[test]
fn repositories_are_compared_normalised() {
    let k = |u: &str| repo_key(u);
    assert_eq!(k("https://github.com/Someone/Weather.git"), k("https://www.github.com/someone/weather/"));
    assert_eq!(k("https://github.com/a/b/tree/main/apps/x"), Some("github.com/a/b".into()));
    assert_eq!(k("http://gitlab.com/a/b/c"), Some("gitlab.com/a/b/c".into()));
    assert_ne!(k("https://github.com/a/b"), k("https://github.com/a/bc"));
    assert_eq!(k("https://github.com"), None);

    let mut apps = vec![
        discover::Listing { name: "c".into(), recent_downloads: 5, stars: None, ..Default::default() },
        discover::Listing { name: "b".into(), recent_downloads: 5, stars: Some(3), ..Default::default() },
        discover::Listing { name: "a".into(), recent_downloads: 1, stars: Some(100), ..Default::default() },
    ];
    rank(&mut apps);
    assert_eq!(apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["b", "c", "a"]);
}

/// A verified build is one whose attestation verifies, for this file and
/// this repository; anything else is unverified, and says why.
#[test]
fn verified_only_by_an_attestation_that_verifies() {
    let (fake, dir) = (Fake::start(), temp("verified"));
    let tree = dir.join("tree");
    let bytes = fake.publish(Publish::default());
    let label = |fake: &Fake| prepare(&fake.http(), &spec("1.2.0"), &tree, None).unwrap().attestation;

    fake.attest(&bytes);
    let v = label(&fake).unwrap();
    assert_eq!(v.workflow, ".github/workflows/release.yml@refs/tags/v1.2.0");

    // Of another file, served under this one's digest.
    fake.attest_as(&bytes, Make { sha256: sha256(b"another"), ..Make::default() });
    assert!(label(&fake).unwrap_err().contains("other files"));
    // Built by another repository's workflow.
    let uri = "https://github.com/mallory/neochumby-weather/.github/workflows/release.yml@refs/heads/main";
    fake.attest_as(&bytes, Make { sha256: sha256(&bytes), uri: uri.into(), ..Make::default() });
    assert!(label(&fake).unwrap_err().contains("workflow of https://github.com/someone/neochumby-weather"));
    // Signed by someone other than GitHub Actions.
    fake.attest_as(&bytes, Make { sha256: sha256(&bytes), issuer: "https://example.com", ..Make::default() });
    assert!(label(&fake).unwrap_err().contains("GitHub Actions"));
    // The bundle {} GitHub's API would have been taken at its word for.
    let path = format!("/github/repos/someone/neochumby-weather/attestations/sha256:{}", sha256(&bytes));
    fake.serve(&path, r#"{"attestations":[{"bundle":{}}]}"#);
    let e = label(&fake).unwrap_err();
    assert!(e.contains("doesn't verify"), "{e}");
    // Signed by a Sigstore neochumbyd doesn't trust (the real one's
    // trusted root, not the test's).
    fake.attest(&bytes);
    let plan = prepare(&Http::with_token(Sources::local(&fake.base), None), &spec("1.2.0"), &tree, None).unwrap();
    assert!(plan.attestation.is_err());
    assert!(plan.summary().contains("UNVERIFIED"));
    assert!(plan.manifest(None).contains("verified=no\n"));
}

/// The installed app at `tree`, as its manifest records it.
fn installed(tree: &Path) -> String {
    install::installed(tree, "weather").unwrap().unwrap().krate
}

/// An update asking for nothing new applies by itself, and is logged; one
/// asking for more waits for the owner, logged once.
#[test]
fn updates_apply_by_themselves_unless_they_ask_for_more() {
    let (fake, dir) = (Fake::start(), temp("auto"));
    let (state, tree) = (dir.join("state"), dir.join("tree"));
    let fb = "title = \"WEATHER\"\naccess = [\"fb\"]\n";
    fake.publish(Publish { meta: fb, ..Publish::default() });
    install(&fake, &tree, "1.2.0", &yes("fb"), None).unwrap();

    // The same version listed: nothing to do.
    fake.list(&state, "1.2.0");
    assert_eq!(update::auto_update(&fake.http(), &state, &tree, None), []);

    // A newer one asking for less: applied.
    let bytes = fake.publish(Publish { version: "1.2.1", meta: "title = \"WEATHER\"\n", ..Publish::default() });
    fake.attest(&bytes);
    fake.list(&state, "1.2.1");
    let updated = Outcome::Updated { app: "weather".into(), from: "1.2.0".into(), to: "1.2.1".into() };
    assert_eq!(update::auto_update(&fake.http(), &state, &tree, None), [updated]);
    assert_eq!(installed(&tree), "neochumby-weather@1.2.1");
    let log = std::fs::read_to_string(state.join(update::LOG)).unwrap();
    assert!(log.contains("apps/weather: updated from 1.2.0 to 1.2.1 by itself (a verified build"), "{log}");

    // An older one listed is never taken.
    fake.list(&state, "1.0.0");
    assert_eq!(update::auto_update(&fake.http(), &state, &tree, None), []);

    // A newer one asking for fb and touch again: left for the owner, and
    // said once.
    fake.publish(Publish { version: "1.3.0", ..Publish::default() });
    fake.list(&state, "1.3.0");
    let held = Outcome::Held { app: "weather".into(), to: "1.3.0".into() };
    assert_eq!(update::auto_update(&fake.http(), &state, &tree, None), [held]);
    assert_eq!(update::auto_update(&fake.http(), &state, &tree, None), []);
    assert_eq!(installed(&tree), "neochumby-weather@1.2.1");
    let log = std::fs::read_to_string(state.join(update::LOG)).unwrap();
    assert_eq!(log.matches("asks for more").count(), 1, "{log}");
    assert!(
        log.contains("(fb, touch)") && log.contains("neochumbyd install crate:neochumby-weather@1.3.0 --yes"),
        "{log}"
    );
}

/// The owner's setting: off, nothing updates by itself; on again, it does.
#[test]
fn automatic_updates_can_be_turned_off() {
    let (fake, dir) = (Fake::start(), temp("auto-off"));
    let (state, tree) = (dir.join("state"), dir.join("tree"));
    fake.publish(Publish::default());
    install(&fake, &tree, "1.2.0", &yes("fb,touch"), None).unwrap();
    fake.publish(Publish { version: "1.2.1", ..Publish::default() });
    fake.list(&state, "1.2.1");

    assert!(update::auto_updates(&state));
    update::set_auto_updates(&state, false).unwrap();
    assert!(!update::auto_updates(&state));
    assert_eq!(update::auto_update(&fake.http(), &state, &tree, None), []);
    assert_eq!(installed(&tree), "neochumby-weather@1.2.0");
    update::set_auto_updates(&state, true).unwrap();
    assert_eq!(update::auto_update(&fake.http(), &state, &tree, None).len(), 1);
    assert_eq!(installed(&tree), "neochumby-weather@1.2.1");
    let log = std::fs::read_to_string(state.join(update::LOG)).unwrap();
    assert!(log.contains("automatic updates turned off by the owner"), "{log}");
}

/// `--no-catalog-refresh`: serve's catalog thread asks crates.io nothing,
/// so updates nothing; without it, it refreshes and then updates.
#[test]
fn the_daily_refresh_can_be_turned_off() {
    let (fake, dir) = (Fake::start(), temp("no-refresh"));
    let (state, tree) = (dir.join("state"), dir.join("tree"));
    fake.publish(Publish::default());
    install(&fake, &tree, "1.2.0", &yes("fb,touch"), None).unwrap();
    fake.publish(Publish { version: "1.2.1", ..Publish::default() });
    let listing = "/api/v1/crates?keyword=neochumby-app&sort=downloads&per_page=100&page=1";
    fake.list(&state, "1.2.1");
    // The cache is a day old: serve would refresh it.
    let file = state.join(discover::FILE);
    let mut c: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    c["fetched_at"] = 0.into();
    std::fs::write(&file, c.to_string()).unwrap();
    let asked = fake.requests(listing).len();

    super::keep(fake.http(), state.clone(), tree.clone(), None, false);
    assert_eq!(fake.requests(listing).len(), asked, "crates.io wasn't asked");
    assert_eq!(installed(&tree), "neochumby-weather@1.2.0");

    let (http, st, tr) = (fake.http(), state.clone(), tree.clone());
    std::thread::spawn(move || super::keep(http, st, tr, None, true));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while installed(&tree) != "neochumby-weather@1.2.1" {
        assert!(std::time::Instant::now() < deadline, "not refreshed and updated");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(fake.requests(listing).len() > asked);
}

/// The admin socket's catalog commands: each change needs the owner's
/// token; with it, `install` checks what it asks for, as approved.
#[test]
fn installs_through_the_admin_socket_need_the_token() {
    use crate::admin::request;
    let (fake, dir) = (Fake::start(), temp("admin"));
    let (state_dir, tree) = (dir.join("state"), dir.join("tree"));
    let st = crate::state::State::open(state_dir.clone()).unwrap();
    let token = st.owner_token().unwrap();
    super::configure_with(&state_dir, tree.clone(), None, fake.http());
    let bytes = fake.publish(Publish::default());
    fake.attest(&bytes);
    let spec = "crate:neochumby-weather@1.2.0";

    assert_eq!(request(&st, &format!("install {spec} fb,touch")), "error install: wrong number of words\n");
    assert_eq!(request(&st, &format!("install {spec} fb,touch nope")), "error wrong token\n");
    assert!(!tree.join("apps/weather").exists());
    // What it asks must be what was approved.
    let e = request(&st, &format!("install {spec} fb {token}"));
    assert!(e.starts_with("error --access fb isn't what it asks for"), "{e}");
    assert!(!tree.join("apps/weather").exists());
    let a = request(&st, &format!("install {spec} touch,fb {token}"));
    assert!(a.ends_with("ok weather\n"), "{a}");
    assert!(a.contains("summary build         verified build"), "{a}");
    assert_eq!(installed(&tree), "neochumby-weather@1.2.0");

    assert_eq!(request(&st, "auto-update"), "ok on\n");
    assert_eq!(request(&st, "auto-update off"), "error auto-update: wrong number of words\n");
    assert_eq!(request(&st, "auto-update off nope"), "error wrong token\n");
    assert_eq!(request(&st, &format!("auto-update maybe {token}")), "error auto-update [on|off TOKEN]\n");
    assert_eq!(request(&st, &format!("auto-update off {token}")), "ok\n");
    assert_eq!(request(&st, "auto-update"), "ok off\n");

    assert_eq!(request(&st, "uninstall weather nope"), "error wrong token\n");
    assert!(tree.join("apps/weather").exists());
    assert_eq!(request(&st, &format!("uninstall weather {token}")), "ok\n");
    assert!(!tree.join("apps/weather").exists());
    let e = request(&st, &format!("uninstall weather {token}"));
    assert!(e.starts_with("error no app weather"), "{e}");

    // The command line's way: the same protocol, over a pair of its own.
    let out = crate::admin::local(&st, &format!("install {spec} fb,touch {token}")).unwrap();
    assert_eq!(out.last().map(String::as_str), Some("weather"));
    assert!(crate::admin::local(&st, &format!("uninstall weather {token}x")).unwrap_err().contains("wrong token"));
    let log = std::fs::read_to_string(state_dir.join(update::LOG)).unwrap();
    for want in ["apps/weather: installed crate:neochumby-weather@1.2.0", "apps/weather: removed", "turned off"] {
        assert!(log.contains(want), "{want}: {log}");
    }
}
