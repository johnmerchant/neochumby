//! The terminal's colours: a scheme of a background, a foreground, the
//! sixteen ANSI colours and the cursor's, as terminal themes give them.
//! GitHub Dark is the default; the owner picks another with `colors=` in
//! the Chumby's settings, naming one of:
//!
//! - GitHub Dark, built in ([`BUILTIN`]: themes/github-dark), so that it
//!   needs nothing from the tree;
//! - a file of the owner's in the tree's themes/ directory, in Ghostty's
//!   theme format (`background = #rrggbb`, `foreground = ...`,
//!   `cursor-color = ...`, `palette = N=#rrggbb`), which most theme
//!   collections offer for download;
//! - a scheme in the tree's [`COLLECTION`], which tools/tree makes from
//!   iTerm2-Color-Schemes and a few themes' own repositories (README,
//!   "What's here"): `dracula`, `catppuccin-mocha`, `tokyonight-night`,
//!   `xterm` and some 900 more, each its name in lower case with `-`
//!   between words;
//! - the colours themselves, as 18 hex RGB values joined by `:`:
//!   background, foreground, then colours 0 to 15. Settings values can't
//!   hold `#` or spaces, and this fits in one.
//!
//! A name none of these has is logged, and GitHub Dark used.
//!
//! On 2.6.16 the terminal draws in these colours itself (grid.rs), its
//! cursor in the cursor's colour; on a kernel console they go in its
//! palette (vt.rs), which has no colour for the cursor (the console draws
//! it in reverse video).

use std::path::Path;

/// The schemes built in, by name: only the default, so that the session
/// carries one and the tree the rest.
pub const BUILTIN: &[(&str, &str)] = &[("github-dark", include_str!("../../themes/github-dark"))];

/// The scheme a Chumby has unless its owner chooses another.
pub const DEFAULT: &str = "github-dark";

/// The tree's collection of schemes, at its top: a line for each, its name
/// then its colours as [`Scheme::inline`] takes them, with the cursor's as
/// a 19th; `#` comments.
pub const COLLECTION: &str = "colors";

/// An RGB colour, 8 bits a component.
pub type Rgb = [u8; 3];

/// A colour scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scheme {
    pub background: Rgb,
    pub foreground: Rgb,
    /// The ANSI colours 0 to 7, then their bright forms, 8 to 15.
    pub palette: [Rgb; 16],
    /// The cursor's colour; the foreground when the theme has none.
    pub cursor: Rgb,
}

impl Scheme {
    /// A scheme in Ghostty's theme format: `key = value` lines, `#`
    /// comments. It must give the background, the foreground and all 16
    /// colours; the cursor's (`cursor-color`) is the foreground if it
    /// doesn't, and other keys (the cursor's text, the selection's) are
    /// ignored.
    pub fn parse(text: &str) -> Result<Scheme, String> {
        let (mut bg, mut fg, mut cursor) = (None, None, None);
        let mut palette: [Option<Rgb>; 16] = [None; 16];
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "background" => bg = Some(hex(v).ok_or(format!("background: {v} is not a colour"))?),
                "foreground" => fg = Some(hex(v).ok_or(format!("foreground: {v} is not a colour"))?),
                "cursor-color" => cursor = Some(hex(v).ok_or(format!("cursor-color: {v} is not a colour"))?),
                "palette" => {
                    let (n, c) = v.split_once('=').ok_or(format!("palette: {v} is not N=#rrggbb"))?;
                    let n: usize = n.trim().parse().map_err(|_| format!("palette: {n} is not a number"))?;
                    let c = hex(c.trim()).ok_or(format!("palette: {c} is not a colour"))?;
                    // Past 15 is the 256-colour table's, which stays xterm's.
                    if let Some(slot) = palette.get_mut(n) {
                        *slot = Some(c);
                    }
                }
                _ => {}
            }
        }
        let mut out = [[0; 3]; 16];
        for (i, c) in palette.iter().enumerate() {
            out[i] = c.ok_or(format!("palette: no colour {i}"))?;
        }
        let foreground = fg.ok_or("no foreground")?;
        Ok(Scheme {
            background: bg.ok_or("no background")?,
            foreground,
            palette: out,
            cursor: cursor.unwrap_or(foreground),
        })
    }

    /// A scheme as a setting's value holds one: 18 hex colours joined by
    /// `:` (background, foreground, colours 0 to 15), and optionally the
    /// cursor's as a 19th, as the [`COLLECTION`] gives it (a setting
    /// hasn't room for it: the cursor is then the foreground).
    pub fn inline(spec: &str) -> Option<Scheme> {
        let c: Vec<Rgb> = spec.split(':').map(hex).collect::<Option<_>>()?;
        let (palette, cursor) = match c.len() {
            18 => (&c[2..], c[1]),
            19 => (&c[2..18], c[18]),
            _ => return None,
        };
        Some(Scheme { background: c[0], foreground: c[1], palette: palette.try_into().ok()?, cursor })
    }

    /// A built-in scheme by name ([`BUILTIN`]).
    pub fn builtin(name: &str) -> Option<Scheme> {
        BUILTIN.iter().find(|(n, _)| *n == name).and_then(|(_, t)| Scheme::parse(t).ok())
    }

    /// `name`'s scheme in a [`COLLECTION`]'s text, if it has one.
    pub fn collected(text: &str, name: &str) -> Option<Result<Scheme, String>> {
        let spec = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_once(' '))
            .find(|(n, _)| *n == name)?
            .1
            .trim();
        Some(Scheme::inline(spec).ok_or_else(|| format!("{name}: {spec} is not 18 or 19 colours joined by :")))
    }

    /// What `colors=` names (the module's documentation), with the tree at
    /// `root`.
    pub fn named(spec: &str, root: &Path) -> Result<Scheme, String> {
        if spec.contains(':') {
            return Scheme::inline(spec).ok_or_else(|| format!("{spec}: not 18 colours joined by :"));
        }
        if let Some(s) = Scheme::builtin(spec) {
            return Ok(s);
        }
        let plain = !spec.is_empty() && spec.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b));
        if !plain || spec.starts_with('.') {
            return Err(format!("{spec}: not a scheme's name"));
        }
        // The owner's own, then the collection.
        let path = root.join("themes").join(spec);
        match std::fs::read_to_string(&path) {
            Ok(text) => return Scheme::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{}: {e}", path.display())),
            Err(_) => {}
        }
        let all = root.join(COLLECTION);
        let text =
            std::fs::read_to_string(&all).map_err(|e| format!("{spec}: not built in, and {}: {e}", all.display()))?;
        Scheme::collected(&text, spec)
            .unwrap_or_else(|| Err(format!("{spec}: not built in, in themes/ or in {}", all.display())))
    }

    /// The scheme `colors=` picks (`None`: it isn't set), with the tree at
    /// `root`: [`DEFAULT`]'s when it's unset or names none, with why.
    pub fn chosen(spec: Option<&str>, root: &Path) -> (Scheme, Option<String>) {
        match spec.map(|s| Scheme::named(s, root)) {
            None => (Scheme::default(), None),
            Some(Ok(s)) => (s, None),
            Some(Err(e)) => (Scheme::default(), Some(e)),
        }
    }

    /// The scheme as a setting's value: [`Scheme::inline`]'s form, without
    /// the cursor.
    pub fn to_inline(&self) -> String {
        let all = [self.background, self.foreground].into_iter().chain(self.palette);
        all.map(|[r, g, b]| format!("{r:02x}{g:02x}{b:02x}")).collect::<Vec<_>>().join(":")
    }
}

impl Default for Scheme {
    fn default() -> Scheme {
        Scheme::builtin(DEFAULT).unwrap_or(XTERM)
    }
}

/// A last resort, should the built-in text ever fail to parse: xterm's own
/// colours (themes/xterm, in the [`COLLECTION`] as `xterm`).
const XTERM: Scheme = Scheme {
    background: [0, 0, 0],
    foreground: [204, 204, 204],
    palette: [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ],
    cursor: [204, 204, 204],
};

/// `#rrggbb` or `rrggbb`.
fn hex(s: &str) -> Option<Rgb> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_dark_is_the_default() {
        let s = Scheme::default();
        assert_eq!(s, Scheme::builtin("github-dark").unwrap());
        assert_eq!((s.background, s.foreground), ([0x01, 0x04, 0x09], [0xe6, 0xed, 0xf3]));
        assert_eq!((s.palette[1], s.palette[15]), ([0xff, 0x7b, 0x72], [0xff, 0xff, 0xff]));
        assert_eq!(s.cursor, s.foreground);
        // #100's GitHub Dark, every colour.
        assert_eq!(
            s.to_inline(),
            "010409:e6edf3:484f58:ff7b72:3fb950:d29922:58a6ff:bc8cff:39c5cf:b1bac4:\
             6e7681:ffa198:56d364:e3b341:79c0ff:d2a8ff:56d4dd:ffffff"
        );
        // The only one built in: the rest are the tree's.
        assert_eq!(BUILTIN.len(), 1);
        assert_eq!(Scheme::parse(include_str!("../../themes/xterm")).unwrap(), XTERM);
    }

    #[test]
    fn inline_schemes_fit_a_setting() {
        let s = Scheme::default();
        let spec = s.to_inline();
        assert!(spec.len() <= 128, "{}", spec.len());
        assert!(neochumby_proto::settings::valid("colors", &spec));
        assert_eq!(Scheme::inline(&spec).unwrap(), s);
        assert_eq!(Scheme::named(&spec, Path::new("/nonexistent")).unwrap(), s);
        assert!(Scheme::inline("000000:ffffff").is_none());
        assert!(Scheme::inline(&format!("{spec}:000000:000000")).is_none());
        assert!(Scheme::inline(&spec.replace("010409", "01040g")).is_none());
        // A 19th is the cursor's.
        assert_eq!(Scheme::inline(&format!("{spec}:123456")).unwrap().cursor, [0x12, 0x34, 0x56]);
    }

    /// A tree with the owner's `mine` and `short` in themes/, and a
    /// collection holding `mine` and `dracula`.
    fn tree() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("neochumby-colors-{}-{n}", std::process::id()));
        std::fs::create_dir_all(root.join("themes")).unwrap();
        let mine = include_str!("../../themes/github-dark").replace("#010409", "#123456");
        std::fs::write(root.join("themes/mine"), &mine).unwrap();
        std::fs::write(root.join("themes/short"), "background = #000000\nforeground = #ffffff\n").unwrap();
        std::fs::write(
            root.join(COLLECTION),
            "# a comment\n\
             mine 000000:ffffff:000000:000000:000000:000000:000000:000000:000000:000000:\
             000000:000000:000000:000000:000000:000000:000000:000000:ffffff\n\
             dracula 282a36:f8f8f2:21222c:ff5555:50fa7b:f1fa8c:bd93f9:ff79c6:8be9fd:f8f8f2:\
             6272a4:ff6e6e:69ff94:ffffa5:d6acff:ff92df:a4ffff:ffffff:f8f8f1\n\
             broken 000000:ffffff\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn ghostty_themes_from_the_tree() {
        let root = tree();
        // The owner's file comes before the collection's `mine`.
        assert_eq!(Scheme::named("mine", &root).unwrap().background, [0x12, 0x34, 0x56]);
        assert!(Scheme::named("short", &root).unwrap_err().contains("no colour 0"));
        assert!(Scheme::named("absent", &root).is_err());
        assert!(Scheme::named("../themes/mine", &root).is_err());
        let cursor = Scheme::parse("cursor-color = #abcdef\n").unwrap_err();
        assert_eq!(cursor, "palette: no colour 0");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn schemes_from_the_collection() {
        let root = tree();
        let d = Scheme::named("dracula", &root).unwrap();
        assert_eq!(
            (d.background, d.foreground, d.cursor),
            ([0x28, 0x2a, 0x36], [0xf8, 0xf8, 0xf2], [0xf8, 0xf8, 0xf1])
        );
        assert_eq!((d.palette[0], d.palette[8], d.palette[15]), ([0x21, 0x22, 0x2c], [0x62, 0x72, 0xa4], [0xff; 3]));
        assert!(Scheme::named("broken", &root).unwrap_err().contains("not 18 or 19"));
        // A comment is never a scheme.
        assert!(Scheme::named("#", &root).is_err());
        assert!(Scheme::collected("# dracula 000000\n", "#").is_none());
        // Built in, the tree's collection or not.
        assert_eq!(Scheme::named("github-dark", &root).unwrap(), Scheme::default());
        assert_eq!(Scheme::named("github-dark", Path::new("/nonexistent")).unwrap(), Scheme::default());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn github_dark_when_unset_or_unknown() {
        let root = tree();
        let gh = Scheme::default();
        assert_eq!(Scheme::chosen(None, &root), (gh.clone(), None));
        let (s, why) = Scheme::chosen(Some("no-such-scheme"), &root);
        assert_eq!(s, gh);
        assert!(why.unwrap().contains("no-such-scheme"));
        // A tree without a collection (one made before it had any).
        let (s, why) = Scheme::chosen(Some("dracula"), Path::new("/nonexistent"));
        assert_eq!(s, gh);
        assert!(why.unwrap().contains(COLLECTION));
        // Broken, or not a name.
        assert_eq!(Scheme::chosen(Some("broken"), &root).0, gh);
        assert_eq!(Scheme::chosen(Some("short"), &root).0, gh);
        assert_eq!(Scheme::chosen(Some("../etc"), &root).0, gh);
        assert_eq!(Scheme::chosen(Some("dracula"), &root).0.background, [0x28, 0x2a, 0x36]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
