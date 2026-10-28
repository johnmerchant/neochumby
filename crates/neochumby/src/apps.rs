//! The apps in the tree: each a directory apps/<name>/ holding a `manifest`
//! of key=value lines, file names relative to the app's directory:
//!
//!   title=DOOM                 listed on the home screen; runs `exec`
//!   exec=neochumby-doom
//!   colour=dc281e              (its colour, hex RGB; optional)
//!   service=neochumby-airplay  runs in the background while neochumby does
//!   companion=...              its server half, which neochumbyd runs
//!   neochumbyd=required        needs a paired neochumbyd (or `optional`:
//!                              uses one when there is one; absent: never)
//!   home=neochumby-home        this app is the home screen
//!   shells=wx home             offered only in these home screens (each
//!                              its app name); absent: in every one
//!
//! An app may have any of these. The session (neochumby) and the home
//! screen agree on the tree through NEOCHUMBY_ROOT.

use std::path::{Path, PathBuf};

use crate::conf;
use crate::fb::rgb;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Needs {
    Nothing,
    /// Uses a neochumbyd when the Chumby is paired with one.
    Optional,
    /// Only on a Chumby paired with a neochumbyd.
    Required,
}

#[derive(Debug)]
pub struct App {
    /// apps/<name>/ in the tree; `name` is how neochumbyd knows it.
    pub dir: PathBuf,
    pub name: String,
    pub title: Option<String>,
    pub exec: Option<PathBuf>,
    pub colour: u16,
    pub service: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub needs: Needs,
    /// The home screens that offer it, by app name; empty: all of them.
    pub shells: Vec<String>,
}

impl App {
    /// Whether it can run here: what it needs of a neochumbyd it has.
    pub fn runnable(&self) -> bool {
        self.needs != Needs::Required || conf::paired().is_some()
    }

    /// Whether the home screen `shell` (its app name) should offer it.
    pub fn offered_in(&self, shell: &str) -> bool {
        self.shells.is_empty() || self.shells.iter().any(|s| s == shell)
    }
}

fn parse_colour(s: &str) -> Option<u16> {
    let v = u32::from_str_radix(s.trim_start_matches('#'), 16).ok()?;
    Some(rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn load(dir: &Path) -> Option<App> {
    let manifest = std::fs::read_to_string(dir.join("manifest")).ok()?;
    let field = |k: &str| {
        manifest
            .lines()
            .filter_map(|l| l.split_once('='))
            .find(|(key, _)| key.trim() == k)
            .map(|(_, v)| v.trim().to_string())
    };
    let file = |k: &str| field(k).filter(|f| !f.is_empty()).map(|f| dir.join(f));
    Some(App {
        dir: dir.to_path_buf(),
        name: dir.file_name()?.to_string_lossy().into_owned(),
        title: field("title"),
        exec: file("exec"),
        colour: field("colour").and_then(|c| parse_colour(&c)).unwrap_or(rgb(220, 160, 40)),
        service: file("service"),
        home: file("home"),
        needs: match field("neochumbyd").as_deref() {
            Some("required") => Needs::Required,
            Some("optional") => Needs::Optional,
            _ => Needs::Nothing,
        },
        shells: field("shells").map(|v| v.split_whitespace().map(String::from).collect()).unwrap_or_default(),
    })
}

/// The tree: NEOCHUMBY_ROOT, which the session sets for what it runs, or
/// the directory of the running binary (the session's own).
pub fn root() -> PathBuf {
    std::env::var_os("NEOCHUMBY_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)))
        .unwrap_or_else(|| "/mnt/neochumby".into())
}

/// Every app in the tree, in name order.
pub fn all(root: &Path) -> Vec<App> {
    let mut dirs: Vec<PathBuf> =
        std::fs::read_dir(root.join("apps")).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    dirs.sort();
    dirs.iter().filter_map(|d| load(d)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(manifest: &str) -> App {
        let dir = std::env::temp_dir().join(format!("neochumby-apps-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest"), manifest).unwrap();
        let a = load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        a
    }

    #[test]
    fn shells() {
        let any = app("title=DOOM\nexec=neochumby-doom\n");
        assert!(any.offered_in("home") && any.offered_in("wx"));
        let some = app("title=WEATHER\nshells=wx  other\n");
        assert_eq!(some.shells, ["wx", "other"]);
        assert!(some.offered_in("wx") && some.offered_in("other") && !some.offered_in("home"));
    }
}
