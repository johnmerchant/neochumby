//! Clips: recorded animation loops for a page (the `clip` directive), in
//! the clip format v1 (`docs/clip-format.md` in phoozle/neochumby-scummvm):
//! a key frame, then row-delta records, a wrap record back to the loop's
//! start, and an index of the records' offsets and sizes.
//!
//! A clip is streamed: only the picture and the record read ahead are in
//! memory, never the clip. Everything read is checked (the index against
//! the files, every span against the screen and its record), and a clip
//! that fails a check is skipped, never fatal. The clips themselves are the
//! owner's (made from their own files); none is in the repository.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use neochumby_sys::chan::{self, Receiver, Sender};

use crate::cache;

pub const MAGIC: &[u8; 8] = b"NCCLIP01";
const W: usize = 320;
const H: usize = 240;
const PIXELS: usize = W * H;
/// The most frames a clip may have (its index is then 512 KB).
pub const MAX_FRAMES: u32 = 65_536;
/// The largest record taken: a record can't usefully be bigger (a span of
/// one pixel per pixel is 8 bytes a pixel).
pub const MAX_RECORD: u32 = 8 * PIXELS as u32 + 8;
/// Clips a `clip` line may name.
pub const MAX_CLIPS: usize = 64;

/// A clip's index, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    pub fps: u32,
    pub frames: u32,
    /// The frame the loop goes back to.
    pub loop_to: u32,
    pub max_record: u32,
    /// Each record's (offset, size) in the .clip file: the frames, then the
    /// wrap.
    pub records: Vec<(u32, u32)>,
}

fn u16_at(b: &[u8], o: usize) -> u32 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u32
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Reads and checks an index (`idx`), against the size of its .clip file.
/// None if anything is off.
pub fn parse_index(idx: &[u8], clip_len: u64) -> Option<Index> {
    if idx.len() < 32 || &idx[..8] != MAGIC || u16_at(idx, 8) != 1 {
        return None;
    }
    if u16_at(idx, 10) != W as u32 || u16_at(idx, 12) != H as u32 {
        return None;
    }
    let (fps, frames, loop_to, max_record, data_size) =
        (u16_at(idx, 14), u32_at(idx, 16), u32_at(idx, 20), u32_at(idx, 24), u32_at(idx, 28));
    let ok = (1..=50).contains(&fps)
        && (1..=MAX_FRAMES).contains(&frames)
        && loop_to < frames
        && (8..=MAX_RECORD).contains(&max_record)
        && max_record.is_multiple_of(4)
        && data_size as u64 == clip_len
        && idx.len() == 32 + (frames as usize + 1) * 8;
    if !ok {
        return None;
    }
    let mut records = Vec::with_capacity(frames as usize + 1);
    let mut next: u64 = 0;
    for k in 0..=frames as usize {
        let (off, size) = (u32_at(idx, 32 + k * 8), u32_at(idx, 36 + k * 8));
        if off as u64 != next || size < 8 || !size.is_multiple_of(4) || size > max_record {
            return None;
        }
        next += size as u64;
        records.push((off, size));
    }
    (next == data_size as u64).then_some(Index { fps, frames, loop_to, max_record, records })
}

/// A record that breaks the format.
#[derive(Debug, PartialEq, Eq)]
pub struct BadRecord;

/// Whether a record is a key (draws every pixel).
pub fn is_key(rec: &[u16]) -> bool {
    rec.first().is_some_and(|w| w & 0xFF == 0)
}

/// Applies a record (its bytes as little-endian words, `len` bytes) to the
/// picture. Every span is checked before it is drawn: inside the screen
/// and its row, in order, not overlapping, and inside the record. A bad
/// record may have drawn its spans up to the bad one.
pub fn apply(rec: &[u16], len: usize, pic: &mut [u16]) -> Result<(), BadRecord> {
    if len < 8 || !len.is_multiple_of(4) || rec.len() * 2 < len || pic.len() != PIXELS {
        return Err(BadRecord);
    }
    let word = |o: usize| rec[o / 2] as u32 | (rec[o / 2 + 1] as u32) << 16;
    let head = word(0);
    if head & 0xFF > 1 || head >> 8 != 0 {
        return Err(BadRecord);
    }
    let spans = word(4);
    let mut off = 8;
    let mut done = 0; // the screen up to here is behind us
    for _ in 0..spans {
        if off + 4 > len {
            return Err(BadRecord);
        }
        let s = word(off);
        let (start, n, fill) = ((s & 0x1FFFF) as usize, ((s >> 17) & 0x1FF) as usize, s >> 31);
        if s & 0x7C00_0000 != 0 || n == 0 || start < done || start % W + n > W || start + n > PIXELS {
            return Err(BadRecord);
        }
        off += 4;
        if fill == 1 {
            if off + 4 > len {
                return Err(BadRecord);
            }
            pic[start..start + n].fill(rec[off / 2]);
            off += 4;
        } else {
            let bytes = (n * 2).div_ceil(4) * 4;
            if off + bytes > len {
                return Err(BadRecord);
            }
            pic[start..start + n].copy_from_slice(&rec[off / 2..off / 2 + n]);
            off += bytes;
        }
        done = start + n;
    }
    if off != len { Err(BadRecord) } else { Ok(()) }
}

/// Reads `len` bytes at `offset` as words (len is a multiple of 4).
pub fn read_range(path: &Path, offset: u64, len: usize) -> Result<Vec<u16>, BadRecord> {
    let mut f = File::open(path).map_err(|_| BadRecord)?;
    f.seek(SeekFrom::Start(offset)).map_err(|_| BadRecord)?;
    let mut buf = cache::alloc(len).ok_or(BadRecord)?;
    let mut chunk = [0u8; 16384];
    let mut got = 0;
    while got < len {
        let want = (len - got).min(chunk.len());
        match f.read(&mut chunk[..want]) {
            Ok(0) | Err(_) => return Err(BadRecord),
            Ok(n) => {
                cache::put_bytes(&mut buf, got, &chunk[..n]);
                got += n;
            }
        }
    }
    Ok(buf)
}

/// The clips a `clip` line names that are there: `DIR/PREFIX` for each
/// PREFIX whose `.idx` and `.clip` both exist, in order. DIR `.` is the
/// picture set `set`; another relative DIR is inside it.
pub fn candidates(set: &Path, dir: &str, prefixes: &[String]) -> Vec<PathBuf> {
    let d = if dir == "." { set.to_path_buf() } else { set.join(dir) };
    prefixes
        .iter()
        .take(MAX_CLIPS)
        .map(|p| d.join(p))
        .filter(|b| b.with_extension("idx").is_file() && b.with_extension("clip").is_file())
        .collect()
}

/// A clip's index from its files (`base`.idx and `base`.clip), checked;
/// also refused if its buffers wouldn't fit `budget` bytes.
pub fn open(base: &Path, budget: usize) -> Option<Index> {
    let clip_len = std::fs::metadata(base.with_extension("clip")).ok()?.len();
    let idx_path = base.with_extension("idx");
    if std::fs::metadata(&idx_path).ok()?.len() > 32 + (MAX_FRAMES as u64 + 1) * 8 {
        return None;
    }
    let ix = parse_index(&std::fs::read(idx_path).ok()?, clip_len)?;
    // The picture, the record showing and the one read ahead.
    (PIXELS * 2 + 2 * ix.max_record as usize <= budget).then_some(ix)
}

/// Reads records in a thread, one at a time, so a slow stick never stalls
/// a frame. Over neochumby_sys::chan: no futex waits.
pub struct Reader {
    req: Sender<(PathBuf, u64, usize)>,
    rep: Receiver<Result<Vec<u16>, BadRecord>>,
}

impl Reader {
    pub fn start() -> Option<Reader> {
        let (req, req_rx) = chan::channel::<(PathBuf, u64, usize)>().ok()?;
        let (rep_tx, rep) = chan::channel::<Result<Vec<u16>, BadRecord>>().ok()?;
        std::thread::Builder::new()
            .name("classicclip".into())
            .spawn(move || {
                while let Ok(r) = req_rx.recv(-1) {
                    let Some((path, off, len)) = r else { continue };
                    if rep_tx.send(read_range(&path, off, len)).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Reader { req, rep })
    }
}

/// A clip playing: its picture, and the next record.
pub struct Play {
    base: PathBuf,
    ix: Index,
    pub pic: Vec<u16>,
    /// The record to apply next (`frames`: the wrap).
    next: u32,
    /// That record, once read.
    ready: Option<Vec<u16>>,
    asked: bool,
    /// When the next record is due, in ms.
    due: i64,
    /// A record failed: the picture holds.
    pub broken: bool,
}

impl Play {
    /// Starts a clip at `now`: reads and applies its key frame (frame 0).
    pub fn start(base: &Path, ix: Index, now: i64) -> Option<Play> {
        let (off, size) = ix.records[0];
        let rec = read_range(&base.with_extension("clip"), off as u64, size as usize).ok()?;
        if !is_key(&rec) {
            return None;
        }
        let mut pic = vec![0u16; PIXELS];
        apply(&rec, size as usize, &mut pic).ok()?;
        let period = 1000 / ix.fps as i64;
        let next = if ix.frames == 1 { ix.frames } else { 1 };
        Some(Play {
            base: base.to_path_buf(),
            ix,
            pic,
            next,
            ready: None,
            asked: false,
            due: now + period,
            broken: false,
        })
    }

    fn after(&self, applied: u32) -> u32 {
        let frame = if applied == self.ix.frames { self.ix.loop_to } else { applied };
        if frame == self.ix.frames - 1 { self.ix.frames } else { frame + 1 }
    }

    fn range(&self, r: u32) -> (PathBuf, u64, usize) {
        let (off, size) = self.ix.records[r as usize];
        (self.base.with_extension("clip"), off as u64, size as usize)
    }

    /// Asks for the next record, if it isn't here or on its way: from the
    /// reader thread, or (None) read now.
    fn fetch(&mut self, reader: Option<&Reader>) {
        if self.ready.is_some() || self.asked || self.broken {
            return;
        }
        let (path, off, len) = self.range(self.next);
        if let Some(r) = reader {
            match r.req.send((path, off, len)) {
                Ok(()) => self.asked = true,
                Err(_) => self.broken = true,
            }
            return;
        }
        match read_range(&path, off, len) {
            Ok(rec) => self.ready = Some(rec),
            Err(_) => self.broken = true,
        }
    }

    /// Takes in the record the reader has finished, without waiting.
    fn receive(&mut self, reader: Option<&Reader>) {
        if !self.asked {
            return;
        }
        let Some(r) = reader else { return };
        match r.rep.recv(0) {
            Ok(Some(Ok(rec))) => {
                self.asked = false;
                self.ready = Some(rec);
            }
            Ok(Some(Err(_))) | Err(_) => {
                self.asked = false;
                self.broken = true;
            }
            Ok(None) => {}
        }
    }

    /// Moves the picture on to `now`: applies the next record if it is due
    /// and here (never skipping one: a late record holds the picture
    /// longer), then asks for the one after. True if the picture changed.
    pub fn step(&mut self, reader: Option<&Reader>, now: i64) -> bool {
        self.receive(reader);
        self.fetch(reader);
        if self.broken || now < self.due {
            return false;
        }
        let Some(rec) = self.ready.take() else { return false };
        let len = self.ix.records[self.next as usize].1 as usize;
        if apply(&rec, len, &mut self.pic).is_err() {
            self.broken = true;
            return false;
        }
        self.next = self.after(self.next);
        // One record a frame, each shown at least a frame: a clip faster
        // than the screen plays slower, and a late one goes on from now
        // rather than catching up.
        let period = 1000 / self.ix.fps as i64;
        self.due = if now - self.due >= period { now + period } else { self.due + period };
        self.fetch(reader);
        self.receive(reader);
        true
    }
}

#[cfg(test)]
pub mod testclip {
    //! Synthetic clips, made by the tests: nothing from any game.

    use super::*;

    /// A span: a row's pixels from `x`, or one colour `fill`ed `n` times.
    pub enum Span {
        Px(usize, usize, Vec<u16>),
        Fill(usize, usize, usize, u16),
    }

    pub fn record(key: bool, spans: &[Span]) -> Vec<u8> {
        let mut r = vec![if key { 0 } else { 1 }, 0, 0, 0];
        r.extend((spans.len() as u32).to_le_bytes());
        for s in spans {
            match s {
                Span::Px(x, y, px) => {
                    r.extend(((y * W + x) as u32 | (px.len() as u32) << 17).to_le_bytes());
                    for p in px {
                        r.extend(p.to_le_bytes());
                    }
                    if px.len() % 2 == 1 {
                        r.extend([0, 0]);
                    }
                }
                Span::Fill(x, y, n, c) => {
                    r.extend(((y * W + x) as u32 | (*n as u32) << 17 | 1 << 31).to_le_bytes());
                    r.extend(c.to_le_bytes());
                    r.extend([0, 0]);
                }
            }
        }
        r
    }

    /// A key frame of one colour, by fills.
    pub fn key(c: u16) -> Vec<u8> {
        record(true, &(0..H).map(|y| Span::Fill(0, y, W, c)).collect::<Vec<_>>())
    }

    /// Writes `base`.idx and `base`.clip: `records` are the frames, then
    /// the wrap.
    pub fn write(base: &Path, fps: u16, loop_to: u32, records: &[Vec<u8>]) {
        let frames = records.len() as u32 - 1;
        let mut idx = Vec::new();
        idx.extend(MAGIC);
        for v in [1u16, W as u16, H as u16, fps] {
            idx.extend(v.to_le_bytes());
        }
        let max = records.iter().map(Vec::len).max().unwrap_or(8) as u32;
        let total: usize = records.iter().map(Vec::len).sum();
        for v in [frames, loop_to, max, total as u32] {
            idx.extend(v.to_le_bytes());
        }
        let mut off = 0u32;
        for r in records {
            idx.extend(off.to_le_bytes());
            idx.extend((r.len() as u32).to_le_bytes());
            off += r.len() as u32;
        }
        std::fs::write(base.with_extension("idx"), idx).unwrap();
        std::fs::write(base.with_extension("clip"), records.concat()).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::testclip::{Span, key, record, write};
    use super::*;

    fn words(b: &[u8]) -> Vec<u16> {
        let mut w = vec![0u16; b.len().div_ceil(2)];
        cache::put_bytes(&mut w, 0, b);
        w
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("classic-clip-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn records_apply_and_are_checked() {
        let mut pic = vec![0u16; PIXELS];
        let k = key(7);
        assert_eq!(k.len(), 8 + 240 * 8);
        apply(&words(&k), k.len(), &mut pic).unwrap();
        assert!(pic.iter().all(|&p| p == 7));
        let d = record(false, &[Span::Px(5, 1, vec![1, 2, 3]), Span::Fill(0, 2, 4, 9)]);
        apply(&words(&d), d.len(), &mut pic).unwrap();
        assert_eq!(&pic[W + 4..W + 9], &[7, 1, 2, 3, 7]);
        assert_eq!(&pic[2 * W..2 * W + 5], &[9, 9, 9, 9, 7]);
        // Bad spans: past the row, out of order, overlapping, a reserved
        // bit, a length of 0, a short record, a bad kind.
        let bad = |b: Vec<u8>| apply(&words(&b), b.len(), &mut vec![0u16; PIXELS]);
        assert!(bad(record(false, &[Span::Px(318, 0, vec![1, 2, 3])])).is_err());
        assert!(bad(record(false, &[Span::Px(5, 1, vec![1]), Span::Px(0, 1, vec![1])])).is_err());
        assert!(bad(record(false, &[Span::Fill(0, 1, 4, 1), Span::Px(3, 1, vec![1])])).is_err());
        let mut r = record(false, &[Span::Px(0, 0, vec![1])]);
        r[11] |= 0x04;
        assert!(bad(r).is_err());
        let mut r = record(false, &[Span::Px(0, 0, vec![1])]);
        r[4] = 2; // two spans, one there
        assert!(bad(r).is_err());
        let mut r = record(false, &[]);
        r[0] = 2;
        assert!(bad(r).is_err());
        assert!(bad(vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_err(), "bytes after the spans");
        assert_eq!(bad(record(false, &[])), Ok(()), "an empty delta");
    }

    #[test]
    fn indexes_are_checked() {
        let d = dir("idx");
        let base = d.join("a");
        write(&base, 12, 0, &[key(1), record(false, &[]), record(false, &[])]);
        let ix = open(&base, 4 << 20).unwrap();
        assert_eq!((ix.fps, ix.frames, ix.loop_to, ix.records.len()), (12, 2, 0, 3));
        let good = std::fs::read(base.with_extension("idx")).unwrap();
        let len = std::fs::metadata(base.with_extension("clip")).unwrap().len();
        let mutate = |at: usize, v: u8| {
            let mut b = good.clone();
            b[at] = v;
            parse_index(&b, len)
        };
        assert!(mutate(0, b'X').is_none(), "magic");
        assert!(mutate(8, 2).is_none(), "version");
        assert!(mutate(10, 0).is_none(), "width");
        assert!(mutate(14, 0).is_none(), "fps 0");
        assert!(mutate(14, 51).is_none(), "fps 51");
        assert!(mutate(20, 2).is_none(), "loop past the end");
        assert!(mutate(36, 12).is_none(), "a size that doesn't add up");
        assert!(parse_index(&good, len + 4).is_none(), "the .clip's size");
        assert!(parse_index(&good[..40], len).is_none(), "short");
        assert!(open(&base, 1000).is_none(), "bigger than the budget");
        // Only complete clips are offered, in the line's order.
        write(&d.join("b"), 12, 0, &[key(1), record(false, &[])]);
        std::fs::write(d.join("c.idx"), b"x").unwrap();
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(candidates(&d, ".", &names(&["b", "c", "missing", "a"])), vec![d.join("b"), d.join("a")]);
        assert!(candidates(&d, "nowhere", &names(&["a"])).is_empty());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn plays_through_the_wrap_in_time() {
        let d = dir("play");
        let base = d.join("loop");
        // Frames: 0 all 1; 1 sets pixel 0 to 2; 2 sets pixel 1 to 3; the
        // wrap goes back to frame 1's picture (pixel 1 back to 1).
        let px = |x, c| record(false, &[Span::Px(x, 0, vec![c])]);
        write(&base, 10, 1, &[key(1), px(0, 2), px(1, 3), px(1, 1)]);
        let ix = open(&base, 4 << 20).unwrap();
        for reader in [None, Reader::start()] {
            let mut p = Play::start(&base, ix.clone(), 0).unwrap();
            let shown = |p: &Play| (p.pic[0], p.pic[1]);
            assert_eq!(shown(&p), (1, 1));
            let mut seen = vec![shown(&p)];
            let mut t = 0;
            while seen.len() < 7 {
                t += 40;
                // The thread's read may not be in at once: give it time.
                for _ in 0..200 {
                    if p.step(reader.as_ref(), t) || p.ready.is_some() || reader.is_none() {
                        break;
                    }
                    neochumby_sys::time::sleep_ms(1);
                }
                if shown(&p) != *seen.last().unwrap() || t % 100 == 0 {
                    seen.push(shown(&p));
                }
            }
            seen.dedup();
            assert_eq!(seen[..5], [(1, 1), (2, 1), (2, 3), (2, 1), (2, 3)], "frame 0, 1, 2, wrap to 1, 2");
            assert!(!p.broken);
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_broken_record_holds_the_picture() {
        let d = dir("broken");
        let base = d.join("x");
        let mut bad = record(false, &[Span::Px(0, 0, vec![5])]);
        bad[6] = 9; // a lot of spans
        write(&base, 25, 0, &[key(4), bad, record(false, &[])]);
        let ix = open(&base, 4 << 20).unwrap();
        let mut p = Play::start(&base, ix, 0).unwrap();
        p.step(None, 1000);
        assert!(p.broken);
        let before = p.pic.clone();
        p.step(None, 2000);
        assert_eq!(p.pic, before);
        // A first frame that isn't a key isn't started.
        write(&base, 25, 0, &[record(false, &[]), record(false, &[])]);
        assert!(Play::start(&base, open(&base, 4 << 20).unwrap(), 0).is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn garbage_never_panics() {
        let d = dir("fuzz");
        let base = d.join("f");
        let good =
            [key(1), record(false, &[Span::Px(3, 4, vec![1, 2, 3]), Span::Fill(0, 9, 30, 6)]), record(false, &[])];
        write(&base, 12, 0, &good);
        let clip = std::fs::read(base.with_extension("clip")).unwrap();
        let mut seed = 0x1234_5678u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..2000 {
            let mut c = clip.clone();
            for _ in 0..1 + rnd() % 8 {
                let i = rnd() as usize % c.len();
                c[i] = rnd() as u8;
            }
            std::fs::write(base.with_extension("clip"), &c).unwrap();
            if let Some(ix) = open(&base, 4 << 20)
                && let Some(mut p) = Play::start(&base, ix, 0)
            {
                for t in 1..6 {
                    p.step(None, t * 100);
                }
            }
        }
        std::fs::remove_dir_all(&d).unwrap();
    }
}
