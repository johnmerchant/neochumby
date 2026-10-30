//! Console fonts, as the Linux console loads them: PSF, version 1 or 2,
//! with or without a Unicode table. Spleen is built in, in every size it
//! comes in (fonts/, BSD-2-Clause: fonts/LICENSE.spleen), and an app may
//! bring a font of its own in its directory.
//!
//! On 2.6.16 the terminal draws with the font itself (grid.rs); where the
//! kernel has a console on the screen, it is loaded there (vt.rs).

use std::collections::HashMap;
use std::io;
use std::path::Path;

/// The fonts built in, by name: Spleen 2.2.0's PSF files, unchanged.
pub const BUILTIN: &[(&str, &[u8])] = &[
    ("spleen-5x8", include_bytes!("../../fonts/spleen-5x8.psfu")),
    ("spleen-6x12", include_bytes!("../../fonts/spleen-6x12.psfu")),
    ("spleen-8x16", include_bytes!("../../fonts/spleen-8x16.psfu")),
    ("spleen-12x24", include_bytes!("../../fonts/spleen-12x24.psfu")),
    ("spleen-16x32", include_bytes!("../../fonts/spleen-16x32.psfu")),
    ("spleen-32x64", include_bytes!("../../fonts/spleen-32x64.psfu")),
];

/// The font a text-mode app gets when neither it nor the owner names one:
/// 64x30 characters on the 320x240 screen.
pub const DEFAULT: &str = "spleen-5x8";

/// The largest glyph the font a PSF file may hold: Spleen's largest.
const MAX_SIDE: usize = 64;
/// The most glyphs a PSF file may hold.
const MAX_GLYPHS: usize = 1024;

const PSF1_MAGIC: [u8; 2] = [0x36, 0x04];
const PSF2_MAGIC: [u8; 4] = [0x72, 0xb5, 0x4a, 0x86];

/// A bitmap font of `width` by `height` cells.
#[derive(Clone)]
pub struct Font {
    pub width: usize,
    pub height: usize,
    /// Each glyph, rows top to bottom, each row whole bytes, the leftmost
    /// pixel the top bit.
    glyphs: Vec<u8>,
    count: usize,
    /// Which glyph a character is, from the Unicode table; without one,
    /// glyph n is character n.
    map: Option<HashMap<char, usize>>,
    /// The file as it came, to load on a console.
    data: Vec<u8>,
}

impl Font {
    /// A font from a PSF file's contents.
    pub fn parse(data: &[u8]) -> io::Result<Font> {
        let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, format!("not a PSF font: {why}"));
        let (width, height, count, header, unicode, row) = if data.starts_with(&PSF1_MAGIC) && data.len() >= 4 {
            let mode = data[2];
            let height = data[3] as usize;
            (8, height, if mode & 1 != 0 { 512 } else { 256 }, 4, mode & 6 != 0, 1)
        } else if data.starts_with(&PSF2_MAGIC) && data.len() >= 32 {
            let word = |i: usize| u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
            let (header, flags, count, size, height, width) =
                (word(8), word(12), word(16), word(20), word(24), word(28));
            if size != height * width.div_ceil(8) {
                return Err(bad("its glyph size and dimensions differ"));
            }
            (width, height, count, header, flags & 1 != 0, width.div_ceil(8))
        } else {
            return Err(bad("no PSF magic"));
        };
        if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
            return Err(bad("glyphs of an unusable size"));
        }
        if !(1..=MAX_GLYPHS).contains(&count) {
            return Err(bad("an unusable number of glyphs"));
        }
        let size = row * height;
        let end = header.checked_add(count * size).filter(|&e| e <= data.len()).ok_or_else(|| bad("cut short"))?;
        let glyphs = data[header..end].to_vec();
        let map = unicode.then(|| unicode_table(&data[end..], count, data.starts_with(&PSF1_MAGIC)));
        Ok(Font { width, height, glyphs, count, map, data: data.to_vec() })
    }

    /// A built-in font by name ([`BUILTIN`]).
    pub fn builtin(name: &str) -> Option<Font> {
        BUILTIN.iter().find(|(n, _)| *n == name).and_then(|(_, d)| Font::parse(d).ok())
    }

    /// What a manifest or a setting names: a built-in font, or a PSF file
    /// (`.psf` or `.psfu`) in `dir`, the app's directory.
    pub fn named(name: &str, dir: &Path) -> io::Result<Font> {
        if is_file_name(name) {
            return Font::parse(&std::fs::read(dir.join(name))?);
        }
        Font::builtin(name).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no font {name}")))
    }

    /// The bytes of the glyph for `ch`, [`Font::row_bytes`] a row, if the
    /// font has one.
    pub fn glyph(&self, ch: char) -> Option<&[u8]> {
        let i = match &self.map {
            Some(m) => *m.get(&ch)?,
            None => ch as usize,
        };
        if i >= self.count {
            return None;
        }
        let size = self.row_bytes() * self.height;
        self.glyphs.get(i * size..(i + 1) * size)
    }

    /// The bytes in one row of a glyph.
    pub fn row_bytes(&self) -> usize {
        self.width.div_ceil(8)
    }

    /// The PSF file it came from.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Whether a font's name is a file's, in the app's directory: a plain
/// name ending in .psf or .psfu, not a path.
pub fn is_file_name(name: &str) -> bool {
    (name.ends_with(".psf") || name.ends_with(".psfu")) && !name.contains('/') && !name.starts_with('.')
}

/// Whether `name` is a font a manifest or a setting may name.
pub fn valid_name(name: &str) -> bool {
    is_file_name(name) || BUILTIN.iter().any(|(n, _)| *n == name)
}

/// The characters each glyph draws, from a PSF file's Unicode table:
/// version 1's little-endian u16s, 0xFFFF ending a glyph's, version 2's
/// UTF-8, 0xFF ending a glyph's. A sequence of characters (after 0xFFFE,
/// or 0xFE) draws as one glyph; only single characters are taken.
fn unicode_table(table: &[u8], count: usize, psf1: bool) -> HashMap<char, usize> {
    let mut map = HashMap::new();
    let mut glyph = 0;
    if psf1 {
        let mut sequences = false;
        for w in table.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)) {
            match w {
                0xffff => {
                    glyph += 1;
                    sequences = false;
                }
                0xfffe => sequences = true,
                _ if !sequences && glyph < count => {
                    if let Some(c) = char::from_u32(w.into()) {
                        map.entry(c).or_insert(glyph);
                    }
                }
                _ => {}
            }
        }
    } else {
        for entry in table.split(|&b| b == 0xff).take(count) {
            let singles = entry.split(|&b| b == 0xfe).next().unwrap_or_default();
            for c in String::from_utf8_lossy(singles).chars().filter(|&c| c != '\u{fffd}') {
                map.entry(c).or_insert(glyph);
            }
            glyph += 1;
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_size_of_spleen_is_built_in() {
        for (name, w, h) in [
            ("spleen-5x8", 5, 8),
            ("spleen-6x12", 6, 12),
            ("spleen-8x16", 8, 16),
            ("spleen-12x24", 12, 24),
            ("spleen-16x32", 16, 32),
            ("spleen-32x64", 32, 64),
        ] {
            let f = Font::builtin(name).unwrap();
            assert_eq!((f.width, f.height), (w, h), "{name}");
            // Latin, Latin-1 (all but 5x8, whose accented letters are
            // blank) and the box drawing, each drawn.
            let accented = (w > 5).then_some('é');
            for c in ['A', 'z', '─', '┼', '│'].into_iter().chain(accented) {
                let g = f.glyph(c).unwrap_or_else(|| panic!("{name}: no {c}"));
                assert_eq!(g.len(), f.row_bytes() * h);
                assert!(g.iter().any(|&b| b != 0), "{name}: {c} is blank");
            }
            assert!(f.glyph(' ').unwrap().iter().all(|&b| b == 0));
            assert!(f.glyph('\u{10ffff}').is_none());
        }
        assert!(Font::builtin("spleen-7x9").is_none());
    }

    #[test]
    fn names_are_built_in_or_files_beside_the_app() {
        assert!(valid_name("spleen-8x16") && valid_name("mine.psfu") && valid_name("mine.psf"));
        assert!(!valid_name("../x.psf") && !valid_name("/etc/x.psfu") && !valid_name(".psf") && !valid_name("x"));
        let dir = std::env::temp_dir().join(format!("neochumby-font-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.psfu"), BUILTIN[1].1).unwrap();
        assert_eq!(Font::named("mine.psfu", &dir).unwrap().width, 6);
        assert_eq!(Font::named("spleen-16x32", &dir).unwrap().height, 32);
        assert!(Font::named("nothing.psf", &dir).is_err() && Font::named("nothing", &dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn what_is_not_a_font_is_refused() {
        let spleen = BUILTIN[0].1;
        for bad in [&b""[..], b"hello", &spleen[..40], &PSF1_MAGIC[..]] {
            assert!(Font::parse(bad).is_err());
        }
        let mut huge = spleen.to_vec();
        huge[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Font::parse(&huge).is_err());
    }
}
