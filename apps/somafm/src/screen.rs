//! The Music screen, from the pictures render/render.py made at build time
//! (files/ in the crate, the app's directory on the Chumby): a background,
//! and on it the station rows, the now-playing card, play/pause and the
//! volume slider, each picked by what the app shows. Every picture is
//! RGB565, as the screen is, so drawing is copying rectangles.
//!
//! files/screen.txt says where each goes, a line each (render.py writes
//! it):
//!
//!   stations N               rows in the list
//!   rows X Y W H             the first row; the rest follow below it
//!   card X Y W H             the now-playing card
//!   play X Y W H             play/pause
//!   slider X Y W H T0 T1 KW KH   the volume slider: the track runs from
//!                            T0 to T1 within it, under a KWxKH knob
//!   hot NAME X Y W H         a tap zone: play, skip or done
//!
//! and day/ and night/ hold the same pictures, light and dark, as
//! little-endian RGB565:
//!
//!   background.rgb565    the screen
//!   rows.rgb565          each row, unselected then selected
//!   cards.rgb565         a card a station
//!   play.rgb565          play, then pause
//!   track_empty.rgb565, track_full.rgb565, knob.rgb565
//!                        the slider; the knob is 0xF81F where it's clear

use std::io;
use std::path::Path;

pub const W: usize = 320;
pub const H: usize = 240;
/// The transparent colour in the knob (magenta), as in classic.
pub const KEY: u16 = 0xF81F;

/// A rectangle on the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32 && y >= self.y as i32 && x < (self.x + self.w) as i32 && y < (self.y + self.h) as i32
    }

    fn fits(&self) -> bool {
        self.w > 0 && self.h > 0 && self.x + self.w <= W && self.y + self.h <= H
    }
}

/// The volume slider: where it is, its track's ends within it, and its
/// knob's size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slider {
    pub at: Rect,
    pub t0: usize,
    pub t1: usize,
    pub kw: usize,
    pub kh: usize,
}

/// Where everything goes: files/screen.txt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub stations: usize,
    /// The first row.
    pub row: Rect,
    pub card: Rect,
    pub play: Rect,
    pub slider: Slider,
    pub hot_play: Rect,
    pub hot_skip: Rect,
    pub hot_done: Rect,
}

impl Layout {
    pub fn parse(text: &str) -> Result<Layout, String> {
        let mut l = Layout::default();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let mut w = line.split_whitespace();
            let key = w.next().unwrap_or_default();
            let name = if key == "hot" { w.next() } else { None };
            let n: Vec<usize> =
                w.map(str::parse).collect::<Result<_, _>>().map_err(|_| format!("{line}: a bad number"))?;
            let rect = |n: &[usize]| match n {
                [x, y, w, h, ..] => Ok(Rect { x: *x, y: *y, w: *w, h: *h }),
                _ => Err(format!("{line}: too few numbers")),
            };
            match (key, name) {
                ("stations", _) => l.stations = *n.first().ok_or(format!("{line}: no count"))?,
                ("rows", _) => l.row = rect(&n)?,
                ("card", _) => l.card = rect(&n)?,
                ("play", _) => l.play = rect(&n)?,
                ("slider", _) => match n[..] {
                    [_, _, _, _, t0, t1, kw, kh] => l.slider = Slider { at: rect(&n)?, t0, t1, kw, kh },
                    _ => return Err(format!("{line}: a slider has eight numbers")),
                },
                ("hot", Some("play")) => l.hot_play = rect(&n)?,
                ("hot", Some("skip")) => l.hot_skip = rect(&n)?,
                ("hot", Some("done")) => l.hot_done = rect(&n)?,
                _ => {} // a line this version doesn't know
            }
        }
        let list = Rect { h: l.row.h * l.stations, ..l.row };
        let s = l.slider;
        let ok = l.stations > 0
            && [list, l.card, l.play, s.at, l.hot_play, l.hot_skip, l.hot_done].iter().all(Rect::fits)
            && s.t0 < s.t1
            && s.t1 <= s.at.w
            && s.kw > 0
            && s.kh > 0
            && s.kh <= s.at.h;
        if ok { Ok(l) } else { Err("a picture or a tap zone is missing or off the screen".into()) }
    }
}

/// A set of pictures, light or dark.
pub struct Theme {
    background: Vec<u16>,
    rows: Vec<u16>,
    cards: Vec<u16>,
    play: Vec<u16>,
    empty: Vec<u16>,
    full: Vec<u16>,
    knob: Vec<u16>,
}

/// Reads an RGB565 picture of exactly `pixels` pixels.
fn picture(path: &Path, pixels: usize) -> io::Result<Vec<u16>> {
    let b = std::fs::read(path)?;
    if b.len() != pixels * 2 {
        return Err(io::Error::other(format!("{}: {} bytes, not {}", path.display(), b.len(), pixels * 2)));
    }
    Ok(b.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes(*p)).collect())
}

impl Theme {
    fn load(dir: &Path, l: &Layout) -> io::Result<Theme> {
        let (r, c, p, s) = (l.row, l.card, l.play, l.slider);
        Ok(Theme {
            background: picture(&dir.join("background.rgb565"), W * H)?,
            rows: picture(&dir.join("rows.rgb565"), r.w * r.h * 2 * l.stations)?,
            cards: picture(&dir.join("cards.rgb565"), c.w * c.h * l.stations)?,
            play: picture(&dir.join("play.rgb565"), p.w * p.h * 2)?,
            empty: picture(&dir.join("track_empty.rgb565"), s.at.w * s.at.h)?,
            full: picture(&dir.join("track_full.rgb565"), s.at.w * s.at.h)?,
            knob: picture(&dir.join("knob.rgb565"), s.kw * s.kh)?,
        })
    }
}

/// What the screen shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    /// The selected station.
    pub station: usize,
    /// Whether the music is on: pause shows, rather than play.
    pub on: bool,
    /// 0-100.
    pub volume: u32,
    pub night: bool,
}

/// What a tap hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Station(usize),
    Play,
    Skip,
    Done,
}

/// The screen: where everything goes, and both sets of pictures.
pub struct Screen {
    pub layout: Layout,
    day: Theme,
    night: Theme,
}

/// Copies frame `f` of a sheet of `r`-sized frames to `r` on the screen.
fn frame(px: &mut [u16], sheet: &[u16], r: Rect, f: usize) {
    for row in 0..r.h {
        let s = (f * r.h + row) * r.w;
        let o = (r.y + row) * W + r.x;
        px[o..o + r.w].copy_from_slice(&sheet[s..s + r.w]);
    }
}

impl Screen {
    /// The screen in `dir`: its screen.txt and pictures.
    pub fn load(dir: &Path) -> io::Result<Screen> {
        let text = std::fs::read_to_string(dir.join("screen.txt"))?;
        let layout = Layout::parse(&text).map_err(|e| io::Error::other(format!("screen.txt: {e}")))?;
        let day = Theme::load(&dir.join("day"), &layout)?;
        let night = Theme::load(&dir.join("night"), &layout)?;
        Ok(Screen { layout, day, night })
    }

    fn theme(&self, v: &View) -> &Theme {
        if v.night { &self.night } else { &self.day }
    }

    /// Draws all of it into `px`, a whole screen.
    pub fn draw(&self, v: &View, px: &mut [u16]) {
        let (l, t) = (&self.layout, self.theme(v));
        px.copy_from_slice(&t.background);
        let station = v.station.min(l.stations - 1);
        for i in 0..l.stations {
            let r = Rect { y: l.row.y + i * l.row.h, ..l.row };
            frame(px, &t.rows, r, 2 * i + usize::from(i == station));
        }
        frame(px, &t.cards, l.card, station);
        frame(px, &t.play, l.play, usize::from(v.on));
        self.draw_slider(v, px);
    }

    /// Draws the slider alone, at `v`'s volume: the full track up to the
    /// knob, the empty one after it, and the knob.
    pub fn draw_slider(&self, v: &View, px: &mut [u16]) {
        let (s, t) = (self.layout.slider, self.theme(v));
        let kx = self.knob_x(v.volume);
        for r in 0..s.at.h {
            let o = (s.at.y + r) * W + s.at.x;
            let row = r * s.at.w;
            px[o..o + s.at.w].copy_from_slice(&t.empty[row..row + s.at.w]);
            px[o + s.t0..o + kx].copy_from_slice(&t.full[row + s.t0..row + kx]);
        }
        let ky = (s.at.h - s.kh) / 2;
        for r in 0..s.kh {
            for c in 0..s.kw {
                let k = t.knob[r * s.kw + c];
                // The knob's left half may hang past the slider at 0: clip it.
                let Some(x) = (kx + c).checked_sub(s.kw / 2).filter(|x| *x < s.at.w) else { continue };
                if k != KEY {
                    px[(s.at.y + ky + r) * W + s.at.x + x] = k;
                }
            }
        }
    }

    /// The knob's centre, within the slider, at volume `v`.
    fn knob_x(&self, v: u32) -> usize {
        let s = self.layout.slider;
        s.t0 + (s.t1 - s.t0) * v.min(100) as usize / 100
    }

    /// Whether (x, y) is on the slider, or near enough to drag it.
    pub fn on_slider(&self, x: i32, y: i32) -> bool {
        let s = self.layout.slider;
        let (sx, sy) = (s.at.x as i32, s.at.y as i32);
        y >= sy - 6 && y < sy + s.at.h as i32 + 6 && x >= sx + s.t0 as i32 - 14 && x <= sx + s.t1 as i32 + 14
    }

    /// The volume a finger at `x` on the slider sets.
    pub fn volume_at(&self, x: i32) -> u32 {
        let s = self.layout.slider;
        let along = x - (s.at.x + s.t0) as i32;
        (along * 100 / (s.t1 - s.t0) as i32).clamp(0, 100) as u32
    }

    /// What a tap at (x, y) hit.
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        let l = &self.layout;
        let list = Rect { h: l.row.h * l.stations, ..l.row };
        if list.contains(x, y) {
            return Some(Hit::Station((y as usize - l.row.y) / l.row.h));
        }
        [(l.hot_play, Hit::Play), (l.hot_skip, Hit::Skip), (l.hot_done, Hit::Done)]
            .into_iter()
            .find(|(r, _)| r.contains(x, y))
            .map(|(_, h)| h)
    }

    /// The rectangle of the slider, to put on the screen after
    /// [`Screen::draw_slider`].
    pub fn slider_rect(&self) -> Rect {
        self.layout.slider.at
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn files() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("files")
    }

    fn screen() -> Screen {
        Screen::load(&files()).expect("files/ as render.py made it")
    }

    /// FNV-1a, 64 bits, as render.py hashes its inputs.
    fn fnv1a64(data: &[u8]) -> u64 {
        data.iter().fold(0xcbf29ce484222325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100000001b3))
    }

    #[test]
    fn the_pictures_are_made_from_the_inputs_as_they_are() {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let rec = std::fs::read_to_string(crate_dir.join("render/rendered-from.txt")).unwrap();
        let mut seen = 0;
        for line in rec.lines().filter(|l| !l.starts_with('#')) {
            let (hash, name) = line.split_once(' ').unwrap();
            let now = format!("{:016x}", fnv1a64(&std::fs::read(crate_dir.join(name)).unwrap()));
            assert_eq!(
                hash, now,
                "{name} changed since files/ was made: run `python3 apps/somafm/render/render.py` (render.py says how)"
            );
            seen += 1;
        }
        // render.py, stations.txt and the three fonts.
        assert_eq!(seen, 5);
    }

    #[test]
    fn the_layout_matches_the_stations() {
        let s = screen();
        assert_eq!(s.layout.stations, crate::stations::all().len());
        assert_eq!(s.layout.row, Rect { x: 7, y: 43, w: 168, h: 27 });
    }

    #[test]
    fn layouts_that_dont_fit_are_refused() {
        let good = std::fs::read_to_string(files().join("screen.txt")).unwrap();
        assert!(Layout::parse(&good).is_ok());
        for bad in [
            good.replace("card 183", "card 300"),
            good.replace("stations 6", "stations 9"),
            good.replace("stations 6", "stations 0"),
            good.replace("hot done", "hot gone"),
            good.replace("slider 70 212 186 26 10 176", "slider 70 212 186 26 176 10"),
            good.replace("rows 7", "rows x"),
        ] {
            assert!(Layout::parse(&bad).is_err(), "{bad}");
        }
        // A line it doesn't know is skipped.
        assert!(Layout::parse(&(good + "glow 1 2 3\n")).is_ok());
    }

    #[test]
    fn taps_land_where_the_pictures_are() {
        let s = screen();
        assert_eq!(s.hit(20, 50), Some(Hit::Station(0)));
        assert_eq!(s.hit(170, 43 + 27 * 5 + 26), Some(Hit::Station(5)));
        assert_eq!(s.hit(170, 43 + 27 * 6), None, "below the list");
        assert_eq!(s.hit(20, 225), Some(Hit::Play));
        assert_eq!(s.hit(52, 225), Some(Hit::Skip));
        assert_eq!(s.hit(290, 225), Some(Hit::Done));
        assert_eq!(s.hit(240, 100), None, "the card");
        assert!(s.on_slider(150, 225));
        assert!(!s.on_slider(290, 225));
        assert_eq!(s.volume_at(70 + 10), 0);
        assert_eq!(s.volume_at(70 + 176), 100);
        assert_eq!(s.volume_at(0), 0);
        assert_eq!(s.volume_at(319), 100);
        assert_eq!(s.volume_at(70 + 10 + 83), 50);
    }

    #[test]
    fn what_shows_follows_the_view() {
        let s = screen();
        let draw = |v: View| {
            let mut px = vec![0u16; W * H];
            s.draw(&v, &mut px);
            px
        };
        let at = |px: &[u16], r: Rect| -> Vec<u16> {
            (0..r.h).flat_map(|y| px[(r.y + y) * W + r.x..(r.y + y) * W + r.x + r.w].to_vec()).collect()
        };
        let base = View { station: 0, on: false, volume: 50, night: false };
        let a = draw(base);
        // Another station: its row is lit, the first isn't, and the card changes.
        let b = draw(View { station: 3, ..base });
        let row = |i: usize| Rect { y: 43 + i * 27, ..s.layout.row };
        assert_ne!(at(&a, row(0)), at(&b, row(0)));
        assert_ne!(at(&a, row(3)), at(&b, row(3)));
        assert_eq!(at(&a, row(1)), at(&b, row(1)));
        assert_ne!(at(&a, s.layout.card), at(&b, s.layout.card));
        // Playing: pause shows instead of play; nothing else changes.
        let c = draw(View { on: true, ..base });
        assert_ne!(at(&a, s.layout.play), at(&c, s.layout.play));
        assert_eq!(at(&a, s.layout.card), at(&c, s.layout.card));
        // The volume moves the knob, within the slider only.
        let d = draw(View { volume: 90, ..base });
        assert_ne!(at(&a, s.slider_rect()), at(&d, s.slider_rect()));
        assert_eq!(at(&a, s.layout.play), at(&d, s.layout.play));
        // At 0 and 100 the knob is clipped, not drawn off the slider.
        for v in [0, 100] {
            draw(View { volume: v, ..base });
        }
        // Night: a darker screen.
        let n = draw(View { night: true, ..base });
        let lum = |px: &[u16]| {
            px.iter().map(|p| u64::from(p >> 11) + u64::from((p >> 5) & 63) / 2 + u64::from(p & 31)).sum::<u64>()
        };
        assert!(lum(&n) * 2 < lum(&a), "night is at least twice as dark");
        // A station past the end shows the last.
        assert_eq!(draw(View { station: 99, ..base }), draw(View { station: 5, ..base }));
    }

    #[test]
    fn a_picture_of_the_wrong_size_is_refused() {
        let dir = std::env::temp_dir().join(format!("somafm-screen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for theme in ["day", "night"] {
            std::fs::create_dir_all(dir.join(theme)).unwrap();
            for e in std::fs::read_dir(files().join(theme)).unwrap().flatten() {
                std::fs::copy(e.path(), dir.join(theme).join(e.file_name())).unwrap();
            }
        }
        std::fs::copy(files().join("screen.txt"), dir.join("screen.txt")).unwrap();
        assert!(Screen::load(&dir).is_ok());
        std::fs::write(dir.join("night/knob.rgb565"), [0u8; 10]).unwrap();
        assert!(Screen::load(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
