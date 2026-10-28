//! neochumbyd's side of a round: the tree a Chumby's set is made from, and
//! what it is sent.
//!
//! The tree is made of layers, each a directory laid out like the tree
//! (neochumbyd's are the shared tree, then the Chumby's own files). A file
//! in a later layer takes the place of the same path in an earlier one.
//! Names starting with '.' are left out, so a program writing a layer puts
//! each file in under such a name and renames it into place: a file is
//! then never sent half-written. Symbolic links, and names a Chumby could
//! not take ([`manifest::valid_path`]), are left out and reported.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use neochumby_proto::{Msg, Session};
use ring::digest::{Context, SHA256};

use crate::{CHUNK, Entry, LIMITS, invalid, manifest, next, recv_manifest, unexpected};

/// How long neochumbyd waits on a Chumby mid-round: it may be writing a
/// large file to a slow stick.
const WAIT: Duration = Duration::from_secs(300);

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

/// The tree made of `layers`. The first must be there; the others may not
/// be.
pub fn scan(layers: &[PathBuf], hashes: &Hashes) -> io::Result<Tree> {
    let mut files = BTreeMap::new();
    let mut left_out = Vec::new();
    for (i, layer) in layers.iter().enumerate() {
        match fs::symlink_metadata(layer) {
            Ok(m) if m.is_dir() => walk(layer, "", &mut files, &mut left_out)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound && i > 0 => {}
            Ok(_) => return Err(invalid(format!("{}: not a directory", layer.display()))),
            Err(e) => return Err(e),
        }
    }
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
    /// `files` sent, `bytes` in all, and what the Chumby said of it.
    Sent { files: usize, bytes: u64, ok: bool, detail: String },
}

/// One round, the Chumby having sent `SyncHave { live }`. The tree is
/// checked as the Chumby will check it first, so one it would refuse is
/// never sent.
pub fn round(s: &mut Session, live: [u8; 32], tree: &Tree) -> io::Result<Served> {
    let entries = tree.manifest();
    if manifest::digest(&entries) == live {
        s.send(&Msg::SyncUpToDate)?;
        return Ok(Served::UpToDate);
    }
    manifest::validate(&entries, &LIMITS)?;
    s.send(&Msg::SyncWant)?;
    let have = recv_manifest(s, WAIT, &LIMITS)?;
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
    match next(s, WAIT)? {
        Msg::SyncDone { ok, detail } => Ok(Served::Sent { files: need.len(), bytes, ok, detail }),
        m => Err(unexpected(&m)),
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
