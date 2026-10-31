//! GET APPS: what a Chumby's catalog app (apps/catalog) and its
//! neochumbyd say to each other (docs/proposals/0003-wasm-apps.md).
//!
//! The app's companion is neochumbyd itself (`companion =
//! "neochumbyd:catalog"` in its manifest): neochumbyd answers from the
//! catalog's cache, which it keeps fresh once a day, and from its tree.
//! The Chumby never reaches crates.io or GitHub, and never installs on its
//! own say: an [`Ask::Install`] or [`Ask::Remove`] becomes a request that
//! waits, under a short code, for the owner's approval on the server
//! (`neochumbyd approve CODE`), but for an update that asks for nothing
//! new.
//!
//! Each message is one frame of [`Catalog`]'s channel
//! (crate::channel), so each must encode to at most a frame: the listing
//! comes a page at a time ([`Ask::List`]), and its texts are cut short
//! ([`MAX_DESCRIPTION`]).

use serde::{Deserialize, Serialize};

use crate::channel::Channel;

/// The channel: [`Ask`] to neochumbyd, [`Answer`] back.
pub struct Catalog;

impl Channel for Catalog {
    type ToCompanion = Ask;
    type ToApp = Answer;
}

/// The value of `companion` that names neochumbyd's own catalog, in
/// place of a program of the app's.
pub const COMPANION: &str = "neochumbyd:catalog";
/// The most apps in one [`Answer::Page`].
pub const PAGE: u32 = 32;
/// The most characters of a description sent.
pub const MAX_DESCRIPTION: usize = 160;
/// The access words a catalog app is never granted: shown, if a crate
/// asks for one, as refused.
pub const NEVER: &[&str] = &["root", "system", "terminal", "logs", "network"];

/// The app, to neochumbyd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ask {
    /// The catalog's apps from `from` on, at most [`PAGE`], best first.
    List { from: u32 },
    /// One app's details: what it asks for, its size, how it would run.
    /// Looked up on the server if it hasn't been yet.
    Details { krate: String, version: String },
    /// Install this version, whose access and companion's hosts the owner
    /// has approved on the Chumby, exactly these. neochumbyd answers
    /// [`Answer::Waiting`] once it has checked it, then waits for the owner
    /// on the server.
    Install { krate: String, version: String, access: Vec<String>, network: Vec<String> },
    /// Remove the installed catalog app `app`; it too waits for the owner.
    Remove { app: String },
    /// Take back the request waiting.
    Cancel,
}

/// neochumbyd, to the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    /// Part of the listing, answering [`Ask::List`].
    Page(Page),
    /// Answering [`Ask::Details`].
    Details(Box<Details>),
    /// The request is made: the owner approves it on the server with
    /// `neochumbyd approve CODE`, or it goes after `minutes`.
    Waiting { code: String, minutes: u16 },
    /// A step of the install, once approved, as a few words.
    Progress(String),
    /// Installed: in the tree as apps/<app>. `home` is the order the
    /// app list shows the tree's apps in (by directory), for saying where.
    /// `restarts`: the Chumby's session is started again once it leaves
    /// GET APPS, so that it knows the app; else (`delivery=sync`) its
    /// next sync brings it.
    Installed { app: String, title: String, home: Vec<String>, restarts: bool },
    /// Removed from the tree.
    Removed { app: String },
    /// Refused or failed, and why, in a sentence. The owner refusing, or
    /// the request lapsing, is one of these too.
    Failed(String),
}

/// A page of the catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    /// When neochumbyd last fetched the catalog, in seconds since 1970;
    /// 0 if it never has.
    pub fetched_at: u64,
    /// How many apps it has in all.
    pub total: u32,
    /// The first app's place in the whole.
    pub from: u32,
    pub apps: Vec<Listing>,
    /// The catalog apps in the tree, with the first page only.
    pub installed: Vec<Installed>,
    /// Whether apps installed now run compiled: false if the server has no
    /// wamrc, so they would be interpreted, and slow.
    pub compiles: bool,
}

/// One app in the listing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    /// The crate, `neochumby-<app>`.
    pub krate: String,
    /// Its newest version.
    pub version: String,
    /// Its directory in the tree, once installed.
    pub app: String,
    /// Its title, once the server has looked it up; else empty.
    pub title: String,
    /// Its owners on crates.io, once looked up; else empty.
    pub publisher: String,
    /// Cut to [`MAX_DESCRIPTION`] characters.
    pub description: String,
    pub downloads: u64,
    pub recent_downloads: u64,
    pub stars: Option<u64>,
    /// "verified build" (Some(true)) or "unverified" (Some(false)), once
    /// looked up; None before, or if it can't be installed.
    pub verified: Option<bool>,
    /// Looked up, and it can't be installed (it asks for what is refused,
    /// say).
    pub refused: bool,
}

/// One app's details, as found by fetching and checking it on the server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Details {
    pub krate: String,
    pub version: String,
    pub app: String,
    pub title: String,
    pub publisher: String,
    pub description: String,
    pub repository: String,
    pub downloads: u64,
    pub verified: bool,
    /// Verified: which workflow built it ("built by .github/workflows/...
    /// in https://github.com/..."); else why it is unverified.
    #[serde(default)]
    pub build: String,
    /// What it asks for that it may be granted, in its order.
    pub access: Vec<String>,
    /// What it asks for that a catalog app never gets ([`NEVER`], or any
    /// word that isn't an access word): it can't be installed.
    pub refused: Vec<String>,
    /// Its companion's hosts, if it has a companion.
    pub network: Vec<String>,
    pub companion: bool,
    /// Its `.wasm`, in bytes.
    pub size: u64,
    /// Whether it would run compiled; else interpreted, slowly.
    pub compiled: bool,
    /// The version installed now, if any.
    pub installed: Option<String>,
    /// For an update: what it asks for that the installed version wasn't
    /// granted.
    pub more: Vec<String>,
    /// Why it can't be installed, if it can't; else empty.
    pub problem: String,
}

/// A catalog app in the tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub app: String,
    pub title: String,
    pub krate: String,
    pub version: String,
    pub access: Vec<String>,
}

/// `s` cut to at most `n` characters, with "..." if it was longer.
pub fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel;

    /// A full page of long listings still fits in a frame.
    #[test]
    fn a_full_page_fits_a_frame() {
        let long = "x".repeat(200);
        let l = Listing {
            krate: format!("neochumby-{}", "a".repeat(54)),
            version: "1.2.3-beta.4+build.5".into(),
            app: "a".repeat(54),
            title: "T".repeat(32),
            publisher: cut(&long, 64),
            description: cut(&long, MAX_DESCRIPTION),
            downloads: u64::MAX,
            recent_downloads: u64::MAX,
            stars: Some(u64::MAX),
            verified: Some(true),
            refused: false,
        };
        let installed = Installed {
            app: "a".repeat(54),
            title: "T".repeat(32),
            krate: "k".repeat(64),
            version: "1".repeat(64),
            access: vec!["touch".into(); 4],
        };
        let page = Answer::Page(Page {
            fetched_at: 1,
            total: 2000,
            from: 0,
            apps: vec![l; PAGE as usize],
            installed: vec![installed; 64],
            compiles: true,
        });
        assert!(channel::encode(&page).is_ok());
    }

    #[test]
    fn cut_short() {
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abcdef", 5), "ab...");
        assert_eq!(cut("ééééé", 4), "é...");
    }
}
