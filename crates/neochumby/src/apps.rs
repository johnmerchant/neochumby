//! The apps in the tree: each a directory apps/<name>/ holding a `manifest`
//! of key=value lines, file names relative to the app's directory:
//!
//!   title=DOOM                 listed in app lists; opens `exec`
//!   exec=neochumby-doom        runs full-screen, in front, when opened
//!   color=dc281e               (its color, hex RGB; optional; `colour`,
//!                              from older trees, is read too)
//!   service=neochumby-airplay  runs in the background while neochumby does
//!   companion=...              its server half, which neochumbyd runs
//!   neochumbyd=required        needs a paired neochumbyd (or `optional`:
//!                              uses one when there is one; absent: never)
//!   start=true                 may be the start app (see the session)
//!   interrupt=false            no service may replace it in front (games)
//!   button=true                gets the top button while in front (games)
//!   access=screen,touch        what it reaches beyond its jail (absent:
//!                              nothing; neochumby-manifest lists the words)
//!   player=true                a player: may be the default one, opened
//!                              by MUSIC buttons; its service takes `pause`
//!                              and `resume` (see the session)
//!   linux=5.9                  the oldest Linux it runs on (absent: any);
//!                              on an older one it isn't listed or opened
//!
//! An app may have any of these. The session (neochumby) and the apps
//! agree on the tree through NEOCHUMBY_ROOT.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
    pub color: u16,
    pub service: Option<PathBuf>,
    /// It has a companion: the session opens it for the app
    /// (neochumby::companion).
    pub companion: bool,
    pub needs: Needs,
    /// It may be the start app: it promises to open other apps and CHUMBY
    /// MODE through the session, to keep running, and to handle the top
    /// button (a `button` line on its pipe, or NEOCHUMBY_REASON=button).
    pub start: bool,
    /// While it's in front, the start app's service may replace it (unless
    /// its manifest says `interrupt=false`).
    pub interrupt: bool,
    /// While it's in front, it has the top button (neochumby::input::Squeeze),
    /// and a press doesn't return to the start app.
    pub button: bool,
    /// A player: it may be this Chumby's default player, which start
    /// apps open for MUSIC (NEOCHUMBY_PLAYER). It promises that its service
    /// pauses and resumes on the session's `pause` and `resume` lines.
    pub player: bool,
    /// What it reaches beyond its jail.
    pub access: Access,
    /// The oldest Linux it runs on, as (major, minor): its manifest's
    /// `linux`. None: any.
    pub linux: Option<(u32, u32)>,
}

/// An app's `access`: what the session gives it beyond its own directory
/// and a /tmp of its own. Each is false unless the manifest lists it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Access {
    /// /dev/fb0, for its exec only.
    pub screen: bool,
    /// The touchscreen and its calibration, for its exec only.
    pub touch: bool,
    /// /dev/dsp and the speaker's handover.
    pub speaker: bool,
    /// The accelerometer.
    pub accel: bool,
    /// /proc, /sys and /etc.
    pub system: bool,
    /// Internet sockets, and /etc/resolv.conf and /etc/hosts.
    pub network: bool,
    /// /dev/ptmx and /dev/pts.
    pub terminal: bool,
    /// The Chumby's logs, read-only.
    pub logs: bool,
    /// No jail: root, as the session.
    pub root: bool,
}

impl Access {
    /// From the manifest's comma-separated value; a word it doesn't know
    /// grants nothing.
    pub fn parse(s: &str) -> Access {
        let mut a = Access::default();
        for w in s.split(',').map(str::trim) {
            match w {
                "screen" => a.screen = true,
                "touch" => a.touch = true,
                "speaker" => a.speaker = true,
                "accel" => a.accel = true,
                "system" => a.system = true,
                "network" => a.network = true,
                "terminal" => a.terminal = true,
                "logs" => a.logs = true,
                "root" => a.root = true,
                _ => {}
            }
        }
        a
    }
}

impl App {
    /// Whether it runs on this Chumby's Linux (its `linux`): an app that
    /// needs a newer one isn't listed, opened or started. When the running
    /// Linux can't be read (a jail without /proc), it is taken to.
    pub fn supported(&self) -> bool {
        self.linux.is_none_or(|need| running_linux().is_none_or(|have| have >= need))
    }

    /// Whether it can run here: on this Linux, and with what it needs of a
    /// neochumbyd.
    pub fn runnable(&self) -> bool {
        self.why_not().is_none()
    }

    /// Why it can't run here, or None if it can.
    pub fn why_not(&self) -> Option<&'static str> {
        if !self.supported() {
            Some("it needs a newer Linux")
        } else if self.needs == Needs::Required && conf::paired().is_none() {
            Some("it needs a paired neochumbyd")
        } else {
            None
        }
    }
}

/// A Linux version's (major, minor), from its first two numbers: "5.9",
/// "2.6.16-csb".
fn parse_linux(s: &str) -> Option<(u32, u32)> {
    let mut n = s.trim().split(|c: char| !c.is_ascii_digit());
    Some((n.next()?.parse().ok()?, n.next()?.parse().ok()?))
}

/// The running Linux's (major, minor), from /proc; None without /proc.
pub fn running_linux() -> Option<(u32, u32)> {
    static LINUX: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    *LINUX.get_or_init(|| std::fs::read_to_string("/proc/sys/kernel/osrelease").ok().and_then(|r| parse_linux(&r)))
}

fn parse_color(s: &str) -> Option<u16> {
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
        color: field("color").or_else(|| field("colour")).and_then(|c| parse_color(&c)).unwrap_or(rgb(220, 160, 40)),
        service: file("service"),
        companion: field("companion").is_some_and(|c| !c.is_empty()),
        needs: match field("neochumbyd").as_deref() {
            Some("required") => Needs::Required,
            Some("optional") => Needs::Optional,
            _ => Needs::Nothing,
        },
        start: field("start").as_deref() == Some("true"),
        interrupt: field("interrupt").as_deref() != Some("false"),
        button: field("button").as_deref() == Some("true"),
        player: field("player").as_deref() == Some("true"),
        access: field("access").map(|a| Access::parse(&a)).unwrap_or_default(),
        linux: field("linux").and_then(|l| parse_linux(&l)),
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

/// This program's own binary, to run it again (on a terminal, say): /proc's
/// link to it, else its absolute argv[0], as the session starts apps. A
/// jail without `system` has no /proc.
pub fn exe() -> Option<PathBuf> {
    std::env::current_exe().ok().or_else(|| {
        let p = PathBuf::from(std::env::args_os().next()?);
        p.is_absolute().then_some(p)
    })
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

    #[test]
    fn linux_versions() {
        assert_eq!(parse_linux("5.9"), Some((5, 9)));
        assert_eq!(parse_linux("2.6.16-csb"), Some((2, 6)));
        assert_eq!(parse_linux("7.3.0-mx21-sideload\n"), Some((7, 3)));
        assert_eq!(parse_linux("5"), None);
        assert!((2, 6) < (5, 9) && (5, 9) <= (5, 9) && (7, 2) >= (5, 9));
    }
}
