//! neochumby's logo on the screen, under the kernel's Tux, before the boot
//! steps: the name in the style of FIGlet's slant where the console is
//! wide enough for it, and elsewhere a leather bean with a prompt for a
//! screen (the Chumby's shape) beside the name.
//!
//! The console's font decides the grid, so there's one drawing for each
//! size of Spleen: the kernel's built-in 5x8 gives 64x30 on the 320x240
//! screen, and a font loaded later 53x20 (6x12), 40x15 (8x16), 26x10
//! (12x24) or 20x7 (16x32). Spleen's 5x8 and 6x12 have no block elements
//! (the kernel's 5x8 has them blank), so the drawings for a console that
//! wide are plain ASCII; the narrower ones use the half and full blocks,
//! which every larger size has. Each drawing is the same characters
//! without colour, so it reads as well on a console that shows none.

use std::fs::{self, OpenOptions};

use neochumby_sys::boot;

/// The logo for the screen's console, tty1, at the size it is now.
pub fn for_screen() -> String {
    let size = OpenOptions::new().read(true).open("/dev/tty1").ok().and_then(|t| boot::size(&t));
    let (cols, rows) = size.map_or((64, 30), |(c, r)| (usize::from(c), usize::from(r)));
    render(cols, rows, &fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default())
}

/// Where the kernel's release goes in a drawing.
struct Tag {
    row: usize,
    col: usize,
    /// `col` is where it ends (exclusive), not where it starts.
    right: bool,
}

/// One drawing of the logo, for a console at least `cols` by `rows`.
struct Logo {
    cols: usize,
    rows: usize,
    /// Half or full blocks: only for a console narrower than `SMALL_FONT`.
    blocks: bool,
    art: &'static [&'static str],
    /// Each character's colour, as the console's palette index in hex
    /// (0-7, then 8-f bright); a space is the console's own. A mask of one
    /// character paints its whole line.
    paint: &'static [&'static str],
    tag: Option<Tag>,
}

/// A console this many columns wide or more has a font six pixels wide or
/// less: Spleen's 5x8 or 6x12, without block elements.
const SMALL_FONT: usize = 45;

/// 64x30, Spleen 5x8: the name, slanted, white through yellow to brown.
const SLANT: Logo = Logo {
    cols: 59,
    rows: 24,
    blocks: false,
    art: &[
        r"                          __                     __",
        r"   ____  ___  ____  _____/ /_  __  ______ ___  / /_  __  __",
        r"  / __ \/ _ \/ __ \/ ___/ __ \/ / / / __ `__ \/ __ \/ / / /",
        r" / / / /  __/ /_/ / /__/ / / / /_/ / / / / / / /_/ / /_/ /",
        r"/_/ /_/\___/\____/\___/_/ /_/\__,_/_/ /_/ /_/_.___/\__, /",
        r"                                                /____/",
    ],
    paint: &["f", "b", "b", "3", "3", "3"],
    tag: Some(Tag { row: 5, col: 47, right: true }),
};

/// 53x20, Spleen 6x12: the bean in ASCII, and the name.
#[rustfmt::skip]
const BEAN_ASCII: Logo = Logo {
    cols: 29,
    rows: 16,
    blocks: false,
    art: &[
        r"  .--------.",
        r" /  ______  \    neochumby",
        r"|  |>_    |  |",
        r"|  |______|  |",
        r" \__________/",
    ],
    paint: &[
        "  3333333333",
        " 3  777777  3    fffbbbbbb",
        "3  7aa    7  3",
        "3  77777777  3",
        " 333333333333",
    ],
    tag: Some(Tag { row: 3, col: 17, right: false }),
};

/// 40x15, Spleen 8x16: the bean in blocks, and the name.
#[rustfmt::skip]
const BEAN: Logo = Logo {
    cols: 25,
    rows: 12,
    blocks: true,
    art: &[
        " ▄▄████████▄▄",
        "███▀▀▀▀▀▀▀▀███  neochumby",
        "███ >_     ███",
        "███▄▄▄▄▄▄▄▄███",
        "▀████████████▀",
    ],
    paint: &["3", "33333333333333  fffbbbbbb", "3333aa33333333", "3", "3"],
    tag: Some(Tag { row: 3, col: 16, right: false }),
};

/// 26x10 and 20x7, Spleen 12x24 and 16x32: a smaller bean, and the name.
const BEAN_SMALL: Logo = Logo {
    cols: 19,
    rows: 6,
    blocks: true,
    art: &["▄██████▄", "██>_  ██  neochumby", "▀██████▀"],
    paint: &["3", "33aa3333  fffbbbbbb", "3"],
    tag: Some(Tag { row: 2, col: 10, right: false }),
};

/// Anything smaller: the name.
const NAME: Logo = Logo { cols: 9, rows: 1, blocks: false, art: &["neochumby"], paint: &["fffbbbbbb"], tag: None };

/// Largest first.
const LOGOS: [&Logo; 5] = [&SLANT, &BEAN_ASCII, &BEAN, &BEAN_SMALL, &NAME];

/// The logo for a console `cols` by `rows` (0 rows: not known), with the
/// kernel's `release`, as text for the console, colours and all, and a
/// blank line after it; or nothing, on a console too small for any.
pub fn render(cols: usize, rows: usize, release: &str) -> String {
    let rows = if rows == 0 { usize::MAX } else { rows };
    let Some(logo) = LOGOS.iter().find(|l| cols >= l.cols && rows >= l.rows && !(l.blocks && cols >= SMALL_FONT))
    else {
        return String::new();
    };
    // Our kernels' local version (-mx21-sideload) says nothing here.
    let release = release.trim();
    let tag = format!("linux {}", release.split("-mx21").next().unwrap_or(release));
    let mut out = String::new();
    for (i, line) in logo.art.iter().enumerate() {
        let mut line: Vec<char> = line.chars().collect();
        let mut mask: Vec<char> = match logo.paint.get(i) {
            Some(m) if m.chars().count() == 1 => vec![m.chars().next().unwrap_or(' '); line.len()],
            Some(m) => m.chars().collect(),
            None => Vec::new(),
        };
        if let Some(t) = &logo.tag
            && t.row == i
            && !release.is_empty()
        {
            let len = tag.chars().count();
            let start = if t.right { t.col.checked_sub(len) } else { Some(t.col) };
            if let Some(start) = start.filter(|s| s + len <= cols) {
                line.resize(line.len().max(start + len), ' ');
                mask.resize(line.len(), ' ');
                for (k, c) in tag.chars().enumerate() {
                    line[start + k] = c;
                    mask[start + k] = '8';
                }
            }
        }
        out.push_str(&paint(&line, &mask));
        out.push('\n');
    }
    out.push('\n');
    out
}

/// `line` in `mask`'s colours, SGR codes only where the colour changes.
fn paint(line: &[char], mask: &[char]) -> String {
    let mut out = String::new();
    let mut now = None;
    for (k, &c) in line.iter().enumerate() {
        let colour = mask.get(k).and_then(|m| m.to_digit(16));
        if c != ' ' && colour != now {
            match colour {
                // Bold is the bright half of the palette on the console.
                Some(n) => out.push_str(&format!("\x1b[{};3{}m", u32::from(n >= 8), n % 8)),
                None => out.push_str("\x1b[0m"),
            }
            now = colour;
        }
        out.push(c);
    }
    if now.is_some() {
        out.push_str("\x1b[0m");
    }
    out.trim_end_matches(' ').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spleen's sizes on the 320x240 screen, and the drawing each gets.
    const GRIDS: [(usize, usize, &str); 6] = [
        (64, 30, "/_/ /_/"),
        (53, 20, "|  |>_"),
        (40, 15, "███ >_"),
        (26, 10, "██>_"),
        (20, 7, "██>_"),
        (10, 3, "neochumby"),
    ];

    /// What the console shows: the text without its SGR codes.
    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            match (esc, c) {
                (false, '\x1b') => esc = true,
                (true, 'm') => esc = false,
                (true, _) => {}
                (false, c) => out.push(c),
            }
        }
        out
    }

    #[test]
    fn each_size_its_own() {
        for (cols, rows, has) in GRIDS {
            let shown = plain(&render(cols, rows, "7.3.1-mx21-sideload"));
            assert!(shown.contains(has), "{cols}x{rows}:\n{shown}");
            for line in shown.lines() {
                assert!(line.chars().count() <= cols, "{cols}x{rows}: {line:?}");
            }
            assert!(shown.lines().count() < rows, "{cols}x{rows}");
        }
    }

    #[test]
    fn blocks_only_where_the_font_has_them() {
        for (cols, rows, _) in GRIDS {
            let shown = plain(&render(cols, rows, "7.4.0-rc3-mx21-sideload"));
            if cols >= SMALL_FONT {
                assert!(shown.is_ascii(), "{cols}x{rows}");
            } else {
                assert!(shown.chars().all(|c| c.is_ascii() || "█▀▄".contains(c)), "{cols}x{rows}");
            }
        }
    }

    #[test]
    fn masks_fit_their_art() {
        for logo in LOGOS {
            assert_eq!(logo.art.len(), logo.paint.len());
            for (line, mask) in logo.art.iter().zip(logo.paint) {
                let n = mask.chars().count();
                assert!(n == 1 || n <= line.chars().count(), "{line:?} {mask:?}");
                assert!(mask.chars().all(|c| c == ' ' || c.is_ascii_hexdigit()), "{mask:?}");
                // Every character drawn has a colour: the mask lines up.
                if n > 1 {
                    let mut m = mask.chars().chain(std::iter::repeat(' '));
                    assert!(line.chars().all(|c| m.next() != Some(' ') || c == ' '), "{line:?} {mask:?}");
                }
            }
            assert!(logo.art.iter().all(|l| l.chars().count() <= logo.cols));
        }
    }

    #[test]
    fn the_release_without_our_local_version() {
        let shown = plain(&render(64, 30, "7.2.13-mx21-sideload\n"));
        assert!(shown.contains("linux 7.2.13 /____/"), "{shown}");
        assert!(!plain(&render(64, 30, "")).contains("linux"));
    }

    #[test]
    fn colours_and_back() {
        let s = render(64, 30, "7.3.1");
        assert!(s.lines().next().is_some_and(|l| l.trim_start().starts_with("\x1b[1;37m__")));
        assert!(s.contains("\x1b[0;33m/_/ /_/"));
        assert!(s.lines().filter(|l| !l.is_empty()).all(|l| l.ends_with("\x1b[0m")));
    }

    #[test]
    fn nothing_on_a_tiny_console() {
        assert_eq!(render(8, 3, "7.3.1"), "");
        assert!(!render(64, 0, "7.3.1").is_empty());
    }
}
