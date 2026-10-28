//! neochumbyd's side of a round: the tree a Chumby's set is made from, and
//! what it is sent.
//!
//! The tree is a directory, laid out as the Chumby runs it. Names starting
//! with '.' are left out silently (a tree can hold `.DS_Store` and the
//! like). Symbolic links, and names a Chumby could not take
//! ([`manifest::valid_path`]), are left out and reported.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use neochumby_proto::{Msg, Session};
use ring::digest::{Context, SHA256};

use crate::{CHUNK, Entry, LIMITS, Timing, invalid, manifest, next, recv_manifest, unexpected};

/// How long neochumbyd waits on a Chumby mid-round (it may be writing a
/// large file to a slow stick), and for a whole round.
const TIMING: Timing = Timing { step: Duration::from_secs(300), round: Duration::from_secs(2 * 3600) };

/// A file of the tree, and where it is read from.
#[derive(Debug, Clone)]
pub struct Source {
    pub entry: Entry,
    pub file: PathBuf,
}

/// The tree's files, with what was left out of it and why.
pub struct Tree {
    pub files: Vec<Source>,
    pub left_out: Vec<String>,
}

impl Tree {
    pub fn manifest(&self) -> Vec<Entry> {
        self.files.iter().map(|f| f.entry.clone()).collect()
    }
}

/// Files' hashes, kept while their size, times and inode stay the same, so
/// a round hashes only what changed.
#[derive(Default)]
pub struct Hashes(Mutex<HashMap<PathBuf, (Stamp, [u8; 32])>>);

type Stamp = (u64, i64, i64, i64, u64, u64);

fn stamp(m: &fs::Metadata) -> Stamp {
    (m.len(), m.mtime(), m.mtime_nsec(), m.ctime(), m.ino(), m.dev())
}

impl Hashes {
    /// `file`'s size and SHA-256.
    pub fn get(&self, file: &Path) -> io::Result<(u64, [u8; 32])> {
        let before = fs::symlink_metadata(file)?;
        if let Some((s, h)) = self.0.lock().unwrap().get(file)
            && *s == stamp(&before)
        {
            return Ok((before.len(), *h));
        }
        let mut f = File::open(file)?;
        let mut c = Context::new(&SHA256);
        let mut buf = vec![0u8; CHUNK];
        let mut n = 0u64;
        loop {
            match f.read(&mut buf)? {
                0 => break,
                k => {
                    c.update(&buf[..k]);
                    n += k as u64;
                }
            }
        }
        let after = f.metadata()?;
        if stamp(&after) != stamp(&before) || n != after.len() {
            return Err(invalid(format!("{}: changed while it was hashed", file.display())));
        }
        let h: [u8; 32] = c.finish().as_ref().try_into().expect("32 bytes");
        self.0.lock().unwrap().insert(file.to_path_buf(), (stamp(&after), h));
        Ok((n, h))
    }
}

/// The tree in `dir`, hashed.
pub fn scan(dir: &Path, hashes: &Hashes) -> io::Result<Tree> {
    let mut files = BTreeMap::new();
    let mut left_out = Vec::new();
    if !fs::symlink_metadata(dir)?.is_dir() {
        return Err(invalid(format!("{}: not a directory", dir.display())));
    }
    walk(dir, "", &mut files, &mut left_out)?;
    let files = files
        .into_iter()
        .map(|(path, file): (String, PathBuf)| {
            let (size, sha256) = hashes.get(&file)?;
            Ok(Source { entry: Entry { path, size, sha256 }, file })
        })
        .collect::<io::Result<Vec<Source>>>()?;
    Ok(Tree { files, left_out })
}

fn walk(base: &Path, rel: &str, out: &mut BTreeMap<String, PathBuf>, left_out: &mut Vec<String>) -> io::Result<()> {
    for e in fs::read_dir(base.join(rel))? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let r = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        let t = e.file_type()?;
        if t.is_dir() {
            walk(base, &r, out, left_out)?;
        } else if !t.is_file() {
            left_out.push(format!("{r}: not a plain file"));
        } else if !manifest::valid_path(&r) {
            left_out.push(format!("{r}: a name a Chumby cannot take"));
        } else {
            out.insert(r, e.path());
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Served {
    /// The Chumby's live set is the tree.
    UpToDate,
    /// The tree is one a Chumby would refuse; it was told why.
    Refused(String),
    /// `files` sent, `bytes` in all, and what the Chumby said of it.
    Sent { files: usize, bytes: u64, ok: bool, detail: String },
}

/// Ends a round before it starts, telling the Chumby why.
pub fn refuse(s: &mut Session, why: String) -> io::Result<Served> {
    s.send(&Msg::SyncDone { ok: false, detail: why.chars().take(1000).collect() })?;
    Ok(Served::Refused(why))
}

/// One round, the Chumby having sent `SyncHave { live }`. The tree is
/// checked as the Chumby will check it first, so one it would refuse is
/// never sent; the Chumby is told why instead.
pub fn round(s: &mut Session, live: [u8; 32], tree: &Tree) -> io::Result<Served> {
    let clock = TIMING.start();
    let entries = tree.manifest();
    if manifest::digest(&entries) == live {
        s.send(&Msg::SyncUpToDate)?;
        return Ok(Served::UpToDate);
    }
    if let Err(e) = manifest::validate(&entries, &LIMITS) {
        return refuse(s, format!("a Chumby cannot take this tree: {e}"));
    }
    s.send(&Msg::SyncWant)?;
    let have = recv_manifest(s, &clock, &LIMITS)?;
    let need = manifest::plan(&entries, &have);
    for m in manifest::messages(&entries) {
        s.send(&m)?;
    }
    let files: HashMap<&str, &Path> = tree.files.iter().map(|f| (f.entry.path.as_str(), f.file.as_path())).collect();
    let mut bytes = 0;
    for e in &need {
        send_file(s, e, files[e.path.as_str()])?;
        bytes += e.size;
    }
    s.send(&Msg::SyncEnd)?;
    // Each SyncProgress (the Chumby hashing what it kept) restarts the
    // wait; the round's own limit still holds.
    loop {
        match next(s, &clock)? {
            Msg::SyncProgress { .. } => {}
            Msg::SyncDone { ok, detail } => return Ok(Served::Sent { files: need.len(), bytes, ok, detail }),
            m => return Err(unexpected(&m)),
        }
    }
}

/// `e` from `file`, checked against its hash as it goes: a file that
/// changed since it was hashed fails the round, at both ends.
fn send_file(s: &mut Session, e: &Entry, file: &Path) -> io::Result<()> {
    let mut f = File::open(file)?;
    s.send(&Msg::SyncFile(e.clone()))?;
    let mut c = Context::new(&SHA256);
    let mut buf = vec![0u8; CHUNK];
    let mut left = e.size;
    while left > 0 {
        let want = (left as usize).min(CHUNK);
        let n = f.read(&mut buf[..want])?;
        if n == 0 {
            return Err(invalid(format!("{}: shorter than when it was hashed", e.path)));
        }
        c.update(&buf[..n]);
        s.send(&Msg::SyncData(buf[..n].to_vec()))?;
        left -= n as u64;
    }
    if c.finish().as_ref() != e.sha256 {
        return Err(invalid(format!("{}: changed since it was hashed", e.path)));
    }
    Ok(())
}
