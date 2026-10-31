//! The apps in the tree: each a directory apps/<name>/ holding a `manifest`
//! of key=value lines, file names relative to the app's directory:
//!
//!   title=DOOM                 listed in app lists; opens `exec`
//!   exec=neochumby-doom        runs full-screen, in front, when opened
//!   color=dc281e               (its color, hex RGB; optional; `colour`,
//!                              from older trees, is read too)
//!   service=neochumby-airplay  runs in the background while neochumby does
//!   companion=...              its server half, which neochumbyd runs
//!                              for each Chumby
//!   server=...                 its half that runs once on the server for
//!                              all Chumbys, which neochumbyd starts (not
//!                              read here, nor are access= and listen=)
//!   neochumbyd=required        needs a paired neochumbyd (or `optional`:
//!                              uses one when there is one; absent: never)
//!   start=true                 may be the start app (see the session)
//!   interrupt=false            no service may replace it in front (games)
//!   button=true                gets the top button while in front (games)
//!   access=fb,touch            what it reaches beyond its jail (absent:
//!                              nothing; neochumby-manifest lists the words)
//!   player=true                a player: may be the default one, opened
//!                              by MUSIC buttons; its service takes `pause`
//!                              and `resume` (see the session)
//!   font=spleen-8x16           its terminal's font, in text mode (below)
//!   tap=q                      what a tap types, in text mode, while the
//!                              program hasn't asked for the mouse: \n, \r,
//!                              \t, \e, \\ and \xHH for what a line can't
//!                              hold
//!
//! An app may have any of these. The session (neochumby) and the apps
//! agree on the tree through NEOCHUMBY_ROOT.
//!
//! An app's exec runs in **text mode** unless its access lists `fb`, root
//! or not: its stdin and stdout are a terminal of its own, which the
//! session shows on the screen (neochumby::term), with touches arriving on
//! stdin. With `fb`, it has the framebuffer and draws on it itself.

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
    /// Its terminal's font in text mode, as its manifest names it: a
    /// built-in font, or a PSF file in its directory (term::font).
    pub font: Option<String>,
    /// What a tap types in text mode while the program hasn't asked for
    /// the mouse.
    pub tap: Vec<u8>,
}

/// An app's `access`: what the session gives it beyond its own directory
/// and a /tmp of its own. Each is false unless the manifest lists it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Access {
    /// /dev/fb0, for its exec only, in place of a terminal: it draws the
    /// screen itself.
    pub fb: bool,
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
    /// A directory of its own in /psp, lasting across boots, shared by its
    /// exec and its service (NEOCHUMBY_STORAGE).
    pub storage: bool,
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
                "fb" => a.fb = true,
                "touch" => a.touch = true,
                "speaker" => a.speaker = true,
                "accel" => a.accel = true,
                "system" => a.system = true,
                "network" => a.network = true,
                "terminal" => a.terminal = true,
                "storage" => a.storage = true,
                "logs" => a.logs = true,
                "root" => a.root = true,
                _ => {}
            }
        }
        a
    }
}

impl App {
    /// Whether it can run here: what it needs of a neochumbyd it has.
    pub fn runnable(&self) -> bool {
        self.needs != Needs::Required || conf::paired().is_some()
    }

    /// Whether its exec runs in text mode, on a terminal the session
    /// shows: unless it draws the screen itself (`fb`), root or not.
    pub fn text(&self) -> bool {
        !self.access.fb
    }
}

/// A manifest's `tap`, its escapes undone: \n, \r, \t, \e, \\ and \xHH.
/// An escape it doesn't know stays as it is.
pub fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let hex = || std::str::from_utf8(b.get(i + 2..i + 4)?).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
        let (byte, len) = match (b[i], b.get(i + 1)) {
            (b'\\', Some(b'n')) => (b'\n', 2),
            (b'\\', Some(b'r')) => (b'\r', 2),
            (b'\\', Some(b't')) => (b'\t', 2),
            (b'\\', Some(b'e')) => (0x1b, 2),
            (b'\\', Some(b'\\')) => (b'\\', 2),
            (b'\\', Some(b'x')) if hex().is_some() => (hex().unwrap_or_default(), 4),
            (c, _) => (c, 1),
        };
        out.push(byte);
        i += len;
    }
    out
}

fn parse_color(s: &str) -> Option<u16> {
    let v = u32::from_str_radix(s.trim_start_matches('#'), 16).ok()?;
    Some(rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// The app in `dir`, from its manifest: checked against the tree's signed
/// manifest where the tree is checked (crate::verified).
pub fn load(dir: &Path) -> Option<App> {
    let manifest = String::from_utf8(crate::verified::read(&dir.join("manifest")).ok()?).ok()?;
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
        font: field("font").filter(|f| !f.is_empty()),
        tap: field("tap").map(|t| unescape(&t)).unwrap_or_default(),
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
    fn taps_unescaped() {
        assert_eq!(unescape("q"), b"q");
        assert_eq!(unescape(r"\n"), b"\n");
        assert_eq!(unescape(r"\e[A\x03\\\t\r"), b"\x1b[A\x03\\\t\r");
        // Not escapes: left as they are.
        assert_eq!(unescape(r"\q\x0g\"), br"\q\x0g\");
    }

    #[test]
    fn text_mode_unless_it_draws() {
        let dir = std::env::temp_dir().join(format!("neochumby-apps-{}", std::process::id()));
        for (name, manifest, text) in [
            ("htop", "exec=x\nfont=spleen-6x12\ntap=\\n\n", true),
            ("doom", "exec=x\naccess=fb,touch\n", false),
            ("classic", "exec=x\naccess=root,fb\n", false),
        ] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
            std::fs::write(dir.join(name).join("manifest"), manifest).unwrap();
            let app = load(&dir.join(name)).unwrap();
            assert_eq!(app.text(), text, "{name}");
            if text {
                assert_eq!((app.font.as_deref(), app.tap.as_slice()), (Some("spleen-6x12"), &b"\n"[..]));
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
