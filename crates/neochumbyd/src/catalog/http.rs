//! The catalog's HTTPS, and where it goes: crates.io's sparse index, its
//! downloads and its API, and GitHub's API. ureq on rustls (ring), with
//! Mozilla's roots built in; every request names neochumbyd and its
//! repository in its User-Agent, as crates.io asks, and gives up after a
//! minute.
//!
//! Only HTTPS is fetched, redirects included. Tests point [`Sources`] at a
//! server of their own on 127.0.0.1 ([`Sources::local`], test builds only),
//! and only URLs under it may then be plain HTTP.
//!
//! It also holds what attestations are checked against (sigstore.rs):
//! Sigstore's public-good instance, except in tests, which sign their own.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::sigstore::Trust;

/// The User-Agent every request sends.
pub const USER_AGENT: &str =
    concat!("neochumbyd/", env!("CARGO_PKG_VERSION"), " (https://github.com/johnmerchant/neochumby)");

/// The most a JSON reply may be.
pub const JSON_LIMIT: u64 = 8 << 20;

/// Where the catalog's requests go.
#[derive(Clone, Debug)]
pub struct Sources {
    /// crates.io's sparse index, ending in `/`.
    pub index: String,
    /// Where `.crate` files are downloaded from:
    /// `{static_crates}/{name}/{name}-{version}.crate`.
    pub static_crates: String,
    /// crates.io's API, without `/v1`.
    pub crates_api: String,
    /// GitHub's API.
    pub github_api: String,
    /// The least time between two requests to crates.io's API: it asks for
    /// at most one a second.
    pub crates_api_gap: Duration,
    /// In tests, the local server whose URLs may be plain HTTP.
    local: Option<String>,
}

impl Default for Sources {
    fn default() -> Sources {
        Sources {
            index: "https://index.crates.io/".into(),
            static_crates: "https://static.crates.io/crates".into(),
            crates_api: "https://crates.io/api".into(),
            github_api: "https://api.github.com".into(),
            crates_api_gap: Duration::from_millis(1100),
            local: None,
        }
    }
}

impl Sources {
    /// Every source under `base` (http://127.0.0.1:PORT), for tests, with no
    /// wait between requests.
    #[cfg(test)]
    pub fn local(base: &str) -> Sources {
        Sources {
            index: format!("{base}/index/"),
            static_crates: format!("{base}/static"),
            crates_api: format!("{base}/api"),
            github_api: format!("{base}/github"),
            crates_api_gap: Duration::ZERO,
            local: Some(base.to_string()),
        }
    }
}

/// What came back.
pub struct Reply {
    pub status: u16,
    pub etag: Option<String>,
    pub body: Vec<u8>,
}

/// The catalog's client.
pub struct Http {
    agent: ureq::Agent,
    pub sources: Sources,
    /// GITHUB_TOKEN, if set: GitHub then allows more requests an hour.
    github_token: Option<String>,
    /// When crates.io's API was last asked.
    last_crates_api: Mutex<Option<Instant>>,
    /// Whom "verified build" is checked against.
    pub trust: Arc<Trust>,
}

impl Http {
    /// A client for `sources`, with GITHUB_TOKEN from the environment.
    pub fn new(sources: Sources) -> Http {
        let token = std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.trim().is_empty());
        Http::with_token(sources, token)
    }

    /// A client for `sources`, sending `github_token` to GitHub.
    pub fn with_token(sources: Sources, github_token: Option<String>) -> Http {
        let mut config = ureq::Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .max_redirects(5)
            .https_only(sources.local.is_none());
        if sources.local.is_some() {
            // A test's own server, never through a proxy in the environment.
            config = config.proxy(None);
        }
        Http {
            agent: config.build().into(),
            sources,
            github_token,
            last_crates_api: Mutex::new(None),
            trust: Arc::new(Trust::public_good()),
        }
    }

    /// The same, checking attestations against `trust`: a test's own.
    #[cfg(test)]
    pub fn trusting(mut self, trust: Trust) -> Http {
        self.trust = Arc::new(trust);
        self
    }

    /// Whether `url` may be fetched: HTTPS, or in tests the local server.
    pub fn check_url(&self, url: &str) -> Result<(), String> {
        let local = self.sources.local.as_deref().is_some_and(|l| url.starts_with(&format!("{l}/")));
        if local || (url.starts_with("https://") && url.len() > "https://".len()) {
            Ok(())
        } else {
            Err(format!("{url}: only HTTPS URLs are fetched"))
        }
    }

    /// GETs `url`, its body at most `limit` bytes; `etag`, if given, as
    /// If-None-Match. Any status comes back; only a failure to ask is an
    /// error.
    pub fn get(&self, url: &str, limit: u64, etag: Option<&str>) -> Result<Reply, String> {
        self.check_url(url)?;
        if url.starts_with(&self.sources.crates_api) {
            self.wait_for_crates_api();
        }
        let mut req = self.agent.get(url);
        if url.starts_with(&self.sources.github_api) {
            req = req.header("Accept", "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
            if let Some(t) = &self.github_token {
                req = req.header("Authorization", format!("Bearer {}", t.trim()));
            }
        }
        if let Some(e) = etag {
            req = req.header("If-None-Match", e);
        }
        let mut resp = req.call().map_err(|e| format!("{url}: {e}"))?;
        let status = resp.status().as_u16();
        let etag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
        let body = if status == 304 {
            Vec::new()
        } else {
            resp.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| format!("{url}: {e}"))?
        };
        Ok(Reply { status, etag, body })
    }

    /// GETs `url`, refusing any status but 200.
    pub fn get_ok(&self, url: &str, limit: u64) -> Result<Vec<u8>, String> {
        let r = self.get(url, limit, None)?;
        if r.status != 200 {
            return Err(format!("{url}: HTTP {}", r.status));
        }
        Ok(r.body)
    }

    /// GETs `url` as JSON, refusing any status but 200.
    pub fn get_json(&self, url: &str) -> Result<serde_json::Value, String> {
        let body = self.get_ok(url, JSON_LIMIT)?;
        serde_json::from_slice(&body).map_err(|e| format!("{url}: {e}"))
    }

    /// Waits until a request to crates.io's API keeps to its rate.
    fn wait_for_crates_api(&self) {
        let mut last = self.last_crates_api.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = *last {
            let since = t.elapsed();
            if since < self.sources.crates_api_gap {
                std::thread::sleep(self.sources.crates_api_gap - since);
            }
        }
        *last = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_or_the_tests_own_server() {
        let release = Http::with_token(Sources::default(), None);
        assert!(release.check_url("https://github.com/a/b/releases/download/v1/app.wasm").is_ok());
        for url in [
            "http://github.com/app.wasm",
            "http://127.0.0.1:1/dl/app.wasm",
            "ftp://x/y",
            "https://",
            "file:///etc/passwd",
        ] {
            assert!(release.check_url(url).is_err(), "{url}");
        }
        assert!(Sources::default().crates_api_gap >= Duration::from_secs(1));

        let local = Http::with_token(Sources::local("http://127.0.0.1:9"), None);
        assert!(local.check_url("http://127.0.0.1:9/dl/app.wasm").is_ok());
        assert!(local.check_url("http://127.0.0.1:99/dl/app.wasm").is_err());
        assert!(local.check_url("http://example.com/app.wasm").is_err());
    }
}
