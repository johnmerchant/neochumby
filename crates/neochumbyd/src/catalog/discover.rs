//! Finding catalog apps, once a day, and keeping what was found in
//! `<state>/catalog.json` ([`Catalog`]). Chumbys and pages read only that
//! cache ([`load`]): GET APPS never makes a Chumby, or a page view, ask
//! crates.io or GitHub anything.
//!
//! - **crates.io:** crates with the keyword `neochumby-app`, by downloads,
//!   100 to a page, following `meta.next_page`, at most one request a
//!   second (Sources::crates_api_gap). Only crates named `neochumby-<app>`
//!   are listed, and none that is yanked.
//! - **GitHub:** repositories with the topic `neochumby-app`, for their
//!   stars, asked with the ETag of the last answer, so an unchanged search
//!   costs nothing against GitHub's limit; a refusal (its rate limit, say)
//!   keeps the stars from before.
//! - Joined by the crate's `repository`, normalised ([`repo_key`]), and
//!   ranked by recent downloads, then stars.
//!
//! The listing is what crates.io says of each crate; what an app asks for
//! is only known, and checked, on installing it (install.rs).

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::http::{Http, JSON_LIMIT};
use super::install::{KEYWORD, PREFIX};

/// The cache's file in the state directory.
pub const FILE: &str = "catalog.json";
/// How old the cache gets before `serve` refreshes it.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// The most pages of crates asked for.
const MAX_PAGES: usize = 20;

/// The cache: every catalog app found, best first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    /// When it was fetched, in seconds since 1970.
    pub fetched_at: u64,
    pub apps: Vec<Listing>,
    /// GitHub's answer, kept for its ETag.
    #[serde(default)]
    pub github: GitHub,
}

/// One catalog app, as crates.io and GitHub describe it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Listing {
    /// The crate: `neochumby-<app>`.
    pub name: String,
    /// Its directory in the tree once installed.
    pub app: String,
    /// The newest stable version (else the newest): what `neochumbyd
    /// install crate:NAME@VERSION` would be told to name.
    pub version: String,
    pub description: String,
    pub repository: Option<String>,
    pub downloads: u64,
    /// In the last 90 days.
    pub recent_downloads: u64,
    /// Its repository's stars on GitHub, if it has the topic there.
    pub stars: Option<u64>,
}

impl Listing {
    /// What to install it with.
    pub fn spec(&self) -> String {
        format!("crate:{}@{}", self.name, self.version)
    }
}

/// GitHub's repositories with the topic, as last answered.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GitHub {
    pub etag: Option<String>,
    pub repos: Vec<Repo>,
}

/// A repository with the topic.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Repo {
    pub url: String,
    pub stars: u64,
}

/// The cache in `state_dir`, if it has been fetched.
pub fn load(state_dir: &Path) -> std::io::Result<Option<Catalog>> {
    match std::fs::read(state_dir.join(FILE)) {
        Ok(b) => serde_json::from_slice(&b).map(Some).map_err(std::io::Error::other),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Keeps `c` as the cache in `state_dir`: written aside, then renamed.
pub fn save(state_dir: &Path, c: &Catalog) -> std::io::Result<()> {
    let tmp = state_dir.join(format!("{FILE}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(c).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, state_dir.join(FILE))
}

/// Asks crates.io and GitHub again, and keeps the answer in `state_dir`.
pub fn refresh(http: &Http, state_dir: &Path) -> Result<Catalog, String> {
    let before = load(state_dir).ok().flatten().unwrap_or_default();
    let c = discover(http, &before)?;
    save(state_dir, &c).map_err(|e| format!("{}: {e}", state_dir.join(FILE).display()))?;
    Ok(c)
}

/// The catalog as crates.io and GitHub have it now, given the one
/// `before` (for GitHub's ETag).
pub fn discover(http: &Http, before: &Catalog) -> Result<Catalog, String> {
    let src = &http.sources;
    let mut apps = Vec::new();
    let mut url = format!("{}/v1/crates?keyword={KEYWORD}&sort=downloads&per_page=100&page=1", src.crates_api);
    for _ in 0..MAX_PAGES {
        let v = http.get_json(&url).map_err(|e| format!("crates.io: {e}"))?;
        let crates = v["crates"].as_array().cloned().unwrap_or_default();
        for c in &crates {
            if let Some(l) = listing(c) {
                apps.push(l);
            }
        }
        match v["meta"]["next_page"].as_str() {
            Some(next) if next.starts_with('?') && !crates.is_empty() => {
                url = format!("{}/v1/crates{next}", src.crates_api);
            }
            _ => break,
        }
    }

    let github = github(http, &before.github);
    for a in &mut apps {
        let key = a.repository.as_deref().and_then(repo_key);
        a.stars =
            key.and_then(|k| github.repos.iter().find(|r| repo_key(&r.url).as_ref() == Some(&k)).map(|r| r.stars));
    }
    rank(&mut apps);
    let fetched_at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Ok(Catalog { fetched_at, apps, github })
}

/// A crate from crates.io's search, if it is a catalog app's.
fn listing(c: &serde_json::Value) -> Option<Listing> {
    let name = c["name"].as_str()?;
    let app = name.strip_prefix(PREFIX).filter(|a| neochumby_proto::valid_name(a) && !a.contains('_'))?;
    if c["yanked"].as_bool() == Some(true) {
        return None;
    }
    let version = c["max_stable_version"].as_str().or(c["max_version"].as_str())?;
    Some(Listing {
        name: name.into(),
        app: app.into(),
        version: version.into(),
        description: c["description"].as_str().unwrap_or_default().trim().into(),
        repository: c["repository"].as_str().map(str::to_string),
        downloads: c["downloads"].as_u64().unwrap_or(0),
        recent_downloads: c["recent_downloads"].as_u64().unwrap_or(0),
        stars: None,
    })
}

/// GitHub's repositories with the topic: asked with `before`'s ETag, and
/// `before` again if unchanged (304) or refused.
fn github(http: &Http, before: &GitHub) -> GitHub {
    let url = format!("{}/search/repositories?q=topic:{KEYWORD}&per_page=100", http.sources.github_api);
    let r = match http.get(&url, JSON_LIMIT, before.etag.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("neochumbyd: catalog: GitHub: {e}; its stars are from before");
            return before.clone();
        }
    };
    match r.status {
        304 => before.clone(),
        200 => {
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&r.body) else { return before.clone() };
            let repos = v["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|i| {
                    Some(Repo { url: i["html_url"].as_str()?.into(), stars: i["stargazers_count"].as_u64()? })
                })
                .collect();
            GitHub { etag: r.etag, repos }
        }
        s => {
            eprintln!(
                "neochumbyd: catalog: GitHub said {s} (its rate limit? GITHUB_TOKEN raises it); its stars are from before"
            );
            before.clone()
        }
    }
}

/// Best first: recent downloads, then stars, then name.
pub fn rank(apps: &mut [Listing]) {
    apps.sort_by(|a, b| {
        b.recent_downloads.cmp(&a.recent_downloads).then(b.stars.cmp(&a.stars)).then(a.name.cmp(&b.name))
    });
}

/// A repository URL as the join compares it: host and path, lower case,
/// without a scheme, `www.`, `.git`, a trailing `/`, or (on GitHub) any
/// path past the repository's.
pub fn repo_key(url: &str) -> Option<String> {
    let u = url.trim().to_ascii_lowercase();
    let u = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")).unwrap_or(&u);
    let u = u.strip_prefix("www.").unwrap_or(u);
    let u = u.split(['?', '#']).next().unwrap_or_default();
    let mut parts: Vec<&str> = u.split('/').filter(|p| !p.is_empty()).collect();
    if parts.first() == Some(&"github.com") {
        parts.truncate(3);
    }
    let last = parts.pop()?;
    parts.push(last.strip_suffix(".git").unwrap_or(last));
    (parts.len() >= 2).then(|| parts.join("/"))
}

/// Refreshes the cache in `state_dir` whenever it is older than
/// [`MAX_AGE`], checking hourly, for as long as `serve` runs, and calls
/// `after` after each refresh (the updates, update.rs).
pub fn daily(http: Http, state_dir: &Path, after: impl Fn(&Http)) -> ! {
    loop {
        let age = load(state_dir).ok().flatten().map(|c| {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            Duration::from_secs(now.saturating_sub(c.fetched_at))
        });
        if age.is_none_or(|a| a >= MAX_AGE) {
            match refresh(&http, state_dir) {
                Ok(c) => {
                    eprintln!("neochumbyd: catalog: {} apps", c.apps.len());
                    after(&http);
                }
                Err(e) => eprintln!("neochumbyd: catalog: {e}; trying again in an hour"),
            }
        }
        std::thread::sleep(Duration::from_secs(60 * 60));
    }
}
