//! neochumby-colors: the tree's collection of colour schemes (`colors`, as
//! neochumby::term::colors reads it), made from directories of terminal
//! themes in Ghostty's format. Run on the host by tools/tree.
//!
//!   neochumby-colors DIR ... > TREE/colors
//!
//! Each file in a DIR is a theme, named by its file name in lower case,
//! `.conf` dropped, with a `-` for each run of anything but letters and
//! digits (`+` is `-plus`): `Catppuccin Mocha` is `catppuccin-mocha`,
//! `Dracula+` `dracula-plus`, `tokyonight_night` `tokyonight-night`. A name
//! in an earlier DIR wins over the same name in a later one (a theme's own
//! repository over iTerm2-Color-Schemes' copy); two in one DIR, or a file
//! that isn't a whole theme, fail.
//!
//! A theme gives the background, the foreground, the 16 ANSI colours and,
//! optionally, the cursor's (else the foreground). What else it gives is
//! dropped: the cursor's text, the selection's colours, and anything past
//! colour 15 (the 256-colour table stays xterm's).
//!
//! Each line is a name, a space, then the 19 colours as hex joined by `:`:
//! background, foreground, colours 0 to 15, the cursor.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::path::Path;
use std::process::ExitCode;

type Rgb = [u8; 3];

/// A theme's colours: background, foreground, 0 to 15, the cursor.
type Colours = [Rgb; 19];

/// A file name as a scheme's name, or None if nothing of it is left.
fn name(file: &str) -> Option<String> {
    let stem = file.strip_suffix(".conf").unwrap_or(file);
    let mut out = String::new();
    for c in stem.replace('+', "-plus").chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    (!out.is_empty()).then_some(out)
}

/// `#rrggbb` or `rrggbb`.
fn hex(s: &str) -> Option<Rgb> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// A theme in Ghostty's format: `key = value` lines, `#` comments.
fn parse(text: &str) -> Result<Colours, String> {
    let mut c: [Option<Rgb>; 19] = [None; 19];
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        let (slot, v) = match k {
            "background" => (0, v),
            "foreground" => (1, v),
            "cursor-color" => (18, v),
            "palette" => {
                let (n, v) = v.split_once('=').ok_or(format!("palette: {v} is not N=#rrggbb"))?;
                let n: usize = n.trim().parse().map_err(|_| format!("palette: {n} is not a number"))?;
                if n > 15 {
                    continue;
                }
                (n + 2, v.trim())
            }
            _ => continue,
        };
        c[slot] = Some(hex(v).ok_or(format!("{k}: {v} is not a colour"))?);
    }
    c[18] = c[18].or(c[1]);
    let what = |i: usize| match i {
        0 => "no background".to_string(),
        1 => "no foreground".to_string(),
        n => format!("palette: no colour {}", n - 2),
    };
    let mut out = [[0; 3]; 19];
    for (i, v) in c.iter().enumerate() {
        out[i] = v.ok_or_else(|| what(i))?;
    }
    Ok(out)
}

/// A scheme's line in the collection, without its newline.
fn line(name: &str, c: &Colours) -> String {
    let hex: Vec<String> = c.iter().map(|[r, g, b]| format!("{r:02x}{g:02x}{b:02x}")).collect();
    format!("{name} {}", hex.join(":"))
}

/// The collection from `dirs`, earlier ones first: its text, and how many
/// names a later DIR had that an earlier one already gave.
fn collect(dirs: &[&Path]) -> Result<(String, usize), String> {
    let mut all: BTreeMap<String, Colours> = BTreeMap::new();
    let mut shadowed = 0;
    for dir in dirs {
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<Result<_, _>>()
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        files.retain(|f| !f.starts_with('.'));
        files.sort();
        let mut here: BTreeMap<String, String> = BTreeMap::new();
        for f in files {
            let path = dir.join(&f);
            let n = name(&f).ok_or_else(|| format!("{}: no name in it", path.display()))?;
            if let Some(other) = here.insert(n.clone(), f.clone()) {
                return Err(format!("{}: {f} and {other} are both {n}", dir.display()));
            }
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let c = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            match all.entry(n) {
                Entry::Vacant(e) => {
                    e.insert(c);
                }
                Entry::Occupied(_) => shadowed += 1,
            }
        }
    }
    let mut out = String::from(
        "# neochumby's colour schemes (colors=), made by tools/tree: a name, then\n\
         # background:foreground:colours 0 to 15:cursor.\n",
    );
    for (n, c) in &all {
        out.push_str(&line(n, c));
        out.push('\n');
    }
    Ok((out, shadowed))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a.starts_with('-')) {
        eprintln!("usage: neochumby-colors DIR ...");
        return ExitCode::from(2);
    }
    let dirs: Vec<&Path> = args.iter().map(Path::new).collect();
    match collect(&dirs) {
        Ok((text, shadowed)) => {
            print!("{text}");
            let n = text.lines().filter(|l| !l.starts_with('#')).count();
            eprintln!("neochumby-colors: {n} schemes, {} bytes ({shadowed} names given earlier)", text.len());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("neochumby-colors: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dracula, as iTerm2-Color-Schemes' ghostty/Dracula gives it.
    const DRACULA: &str = "palette = 0=#21222c\npalette = 1=#ff5555\npalette = 2=#50fa7b\n\
        palette = 3=#f1fa8c\npalette = 4=#bd93f9\npalette = 5=#ff79c6\npalette = 6=#8be9fd\n\
        palette = 7=#f8f8f2\npalette = 8=#6272a4\npalette = 9=#ff6e6e\npalette = 10=#69ff94\n\
        palette = 11=#ffffa5\npalette = 12=#d6acff\npalette = 13=#ff92df\npalette = 14=#a4ffff\n\
        palette = 15=#ffffff\nbackground = #282a36\nforeground = #f8f8f2\ncursor-color = #f8f8f2\n\
        cursor-text = #282a36\nselection-background = #44475a\nselection-foreground = #ffffff\n";

    #[test]
    fn names() {
        for (file, want) in [
            ("Catppuccin Mocha", "catppuccin-mocha"),
            ("catppuccin-mocha.conf", "catppuccin-mocha"),
            ("Dracula+", "dracula-plus"),
            ("tokyonight_night", "tokyonight-night"),
            ("Black Metal (Bathory)", "black-metal-bathory"),
            ("Hopscotch.256", "hopscotch-256"),
            ("Moss & Ember", "moss-ember"),
            ("GitHub Dark", "github-dark"),
        ] {
            assert_eq!(name(file).as_deref(), Some(want), "{file}");
        }
        assert_eq!(name("()"), None);
        assert_eq!(name(".conf"), None);
    }

    #[test]
    fn a_theme_to_its_nineteen_colours() {
        let c = parse(DRACULA).unwrap();
        assert_eq!(
            line("dracula", &c),
            "dracula 282a36:f8f8f2:21222c:ff5555:50fa7b:f1fa8c:bd93f9:ff79c6:8be9fd:f8f8f2:\
             6272a4:ff6e6e:69ff94:ffffa5:d6acff:ff92df:a4ffff:ffffff:f8f8f2"
        );
        // Catppuccin's own writes its background and foreground without #,
        // and a cursor of its own.
        let mocha = DRACULA
            .replace("background = #282a36", "background = 1e1e2e")
            .replace("cursor-color = #f8f8f2", "cursor-color = f5e0dc");
        let c = parse(&mocha).unwrap();
        assert_eq!((c[0], c[18]), ([0x1e, 0x1e, 0x2e], [0xf5, 0xe0, 0xdc]));
        // No cursor: the foreground's.
        let c = parse(&DRACULA.replace("cursor-color = #f8f8f2\n", "foreground = #010203\n")).unwrap();
        assert_eq!(c[18], [1, 2, 3]);
        // Past 15 is dropped.
        assert_eq!(parse(&format!("{DRACULA}palette = 16=#000000\n")).unwrap(), parse(DRACULA).unwrap());
    }

    #[test]
    fn what_isnt_a_whole_theme() {
        assert_eq!(parse(&DRACULA.replace("palette = 9=#ff6e6e\n", "")).unwrap_err(), "palette: no colour 9");
        assert_eq!(parse(&DRACULA.replace("background = #282a36\n", "")).unwrap_err(), "no background");
        assert!(parse(&DRACULA.replace("#ff5555", "red")).unwrap_err().contains("red is not a colour"));
        assert!(parse(&DRACULA.replace("1=#ff5555", "x=#ff5555")).unwrap_err().contains("x is not a number"));
    }

    #[test]
    fn earlier_directories_win() {
        let root = std::env::temp_dir().join(format!("neochumby-colors-{}", std::process::id()));
        let (own, collection) = (root.join("own"), root.join("collection"));
        for d in [&own, &collection] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(own.join("dracula"), DRACULA.replace("#282a36", "#000001")).unwrap();
        std::fs::write(collection.join("Dracula"), DRACULA).unwrap();
        std::fs::write(collection.join("Dracula+"), DRACULA).unwrap();
        std::fs::write(collection.join(".hidden"), "not a theme").unwrap();
        let (text, shadowed) = collect(&[&own, &collection]).unwrap();
        assert_eq!(shadowed, 1);
        let lines: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("dracula 000001:"), "{}", lines[0]);
        assert!(lines[1].starts_with("dracula-plus 282a36:"), "{}", lines[1]);
        // Two of one name in one directory.
        std::fs::write(collection.join("dracula.conf"), DRACULA).unwrap();
        assert!(collect(&[&collection]).unwrap_err().contains("are both dracula"));
        std::fs::remove_file(collection.join("dracula.conf")).unwrap();
        // Something that isn't a theme.
        std::fs::write(collection.join("README"), "# Themes\n").unwrap();
        assert!(collect(&[&collection]).unwrap_err().contains("README: no background"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
