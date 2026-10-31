//! The terminal's colours: a scheme of a background, a foreground and the
//! sixteen ANSI colours, as terminal themes give them. GitHub Dark is the
//! default; the owner picks another with `colors=` in the Chumby's
//! settings, naming one of:
//!
//! - a scheme built in ([`BUILTIN`]: themes/);
//! - a file in the tree's themes/ directory, in Ghostty's theme format
//!   (`background = #rrggbb`, `foreground = ...`, `palette = N=#rrggbb`),
//!   which most theme collections offer for download;
//! - the colours themselves, as 18 hex RGB values joined by `:`:
//!   background, foreground, then colours 0 to 15. Settings values can't
//!   hold `#` or spaces, and this fits in one.
//!
//! On 2.6.16 the terminal draws in these colours itself (grid.rs); on a
//! kernel console they go in its palette (vt.rs).

use std::path::Path;

/// The schemes built in, by name.
pub const BUILTIN: &[(&str, &str)] =
    &[("github-dark", include_str!("../../themes/github-dark")), ("xterm", include_str!("../../themes/xterm"))];

/// The scheme a Chumby has unless its owner chooses another.
pub const DEFAULT: &str = "github-dark";

/// An RGB colour, 8 bits a component.
pub type Rgb = [u8; 3];

/// A colour scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scheme {
    pub background: Rgb,
    pub foreground: Rgb,
    /// The ANSI colours 0 to 7, then their bright forms, 8 to 15.
    pub palette: [Rgb; 16],
}

impl Scheme {
    /// A scheme in Ghostty's theme format: `key = value` lines, `#`
    /// comments. It must give the background, the foreground and all 16
    /// colours; other keys (the cursor's, the selection's) are ignored.
    pub fn parse(text: &str) -> Result<Scheme, String> {
        let (mut bg, mut fg) = (None, None);
        let mut palette: [Option<Rgb>; 16] = [None; 16];
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "background" => bg = Some(hex(v).ok_or(format!("background: {v} is not a colour"))?),
                "foreground" => fg = Some(hex(v).ok_or(format!("foreground: {v} is not a colour"))?),
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
        Ok(Scheme { background: bg.ok_or("no background")?, foreground: fg.ok_or("no foreground")?, palette: out })
    }

    /// A scheme as a setting's value holds one: 18 hex colours joined by
    /// `:` (background, foreground, colours 0 to 15).
    pub fn inline(spec: &str) -> Option<Scheme> {
        let c: Vec<Rgb> = spec.split(':').map(hex).collect::<Option<_>>()?;
        let palette: [Rgb; 16] = c.get(2..)?.try_into().ok()?;
        Some(Scheme { background: c[0], foreground: c[1], palette })
    }

    /// A built-in scheme by name ([`BUILTIN`]).
    pub fn builtin(name: &str) -> Option<Scheme> {
        BUILTIN.iter().find(|(n, _)| *n == name).and_then(|(_, t)| Scheme::parse(t).ok())
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
        let path = root.join("themes").join(spec);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Scheme::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The scheme as a setting's value: [`Scheme::inline`]'s form.
    pub fn to_inline(&self) -> String {
        let all = [self.background, self.foreground].into_iter().chain(self.palette);
        all.map(|[r, g, b]| format!("{r:02x}{g:02x}{b:02x}")).collect::<Vec<_>>().join(":")
    }
}

impl Default for Scheme {
    fn default() -> Scheme {
        Scheme::builtin(DEFAULT).unwrap_or_else(|| Scheme::parse(BUILTIN[0].1).unwrap_or(XTERM))
    }
}

/// A last resort, should the built-in text ever fail to parse.
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
        assert_eq!(Scheme::builtin("xterm").unwrap(), XTERM);
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
        assert!(Scheme::inline(&format!("{spec}:000000")).is_none());
        assert!(Scheme::inline(&spec.replace("010409", "01040g")).is_none());
    }

    #[test]
    fn ghostty_themes_from_the_tree() {
        let root = std::env::temp_dir().join(format!("neochumby-colors-{}", std::process::id()));
        std::fs::create_dir_all(root.join("themes")).unwrap();
        let mine = include_str!("../../themes/github-dark").replace("#010409", "#123456");
        std::fs::write(root.join("themes/mine"), &mine).unwrap();
        std::fs::write(root.join("themes/short"), "background = #000000\nforeground = #ffffff\n").unwrap();
        assert_eq!(Scheme::named("mine", &root).unwrap().background, [0x12, 0x34, 0x56]);
        assert!(Scheme::named("short", &root).unwrap_err().contains("no colour 0"));
        assert!(Scheme::named("absent", &root).is_err());
        assert!(Scheme::named("../themes/mine", &root).is_err());
        assert_eq!(Scheme::named("xterm", &root).unwrap(), XTERM);
        std::fs::remove_dir_all(root).unwrap();
    }
}
