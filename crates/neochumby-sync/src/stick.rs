//! The stick's side: the two sets, their markers, writing the standby set,
//! the flip and rollback. Every write is inside the stick's neochumby
//! directory, to paths [`manifest::valid_path`] takes, through no symbolic
//! link, and never to the live set or the one the Chumby is running from;
//! and only while the stick is mounted, so a stick pulled mid-round never
//! sends writes to the filesystem under its mount point.
//!
//! On the Chumby's 2.6.16 kernel, fsync on FAT syncs the whole device, FAT
//! included. So files are written in batches: each is renamed into place
//! unsynced, then one sync() covers the batch, then its journal lines are
//! appended, then one more sync(). A journal line is never durable before
//! its file.
//!
//! Every operation on the stick goes through a [`Disk`] (disk.rs), so the
//! crash tests can cut the power after any one of them.

use std::collections::HashSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ring::digest::{Context, SHA256};

use crate::disk::{Disk, Kind, Output, RealDisk};
use crate::manifest::{self, line};
use crate::{Entry, LIMITS, Limits, invalid};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Set {
    A,
    B,
}

impl Set {
    pub fn name(self) -> &'static str {
        match self {
            Set::A => "a",
            Set::B => "b",
        }
    }

    pub fn other(self) -> Set {
        match self {
            Set::A => Set::B,
            Set::B => Set::A,
        }
    }

    pub fn parse(s: &str) -> Option<Set> {
        match s {
            "a" => Some(Set::A),
            "b" => Some(Set::B),
            _ => None,
        }
    }
}

/// How often finish() reports progress while it hashes kept files.
const PROGRESS: Duration = Duration::from_secs(30);

/// When a batch of written files is synced: once it holds this many files,
/// or bytes.
#[derive(Clone, Copy, Debug)]
pub struct Batch {
    pub files: usize,
    pub bytes: u64,
}

pub const BATCH: Batch = Batch { files: 64, bytes: 4 << 20 };

/// The stick's neochumby directory.
pub struct Stick {
    pub dir: PathBuf,
    /// Where the stick is mounted, if `dir` is on one: nothing is written
    /// unless it is a different device from the directory it is in.
    pub mount: Option<PathBuf>,
    /// The set the session runs from, if it runs from one: never written.
    pub running: Option<Set>,
    pub limits: Limits,
    /// What reads and writes it.
    pub disk: Arc<dyn Disk>,
    pub batch: Batch,
}

/// `neochumby check-set DIR`, which the boot hook runs: whether the set in
/// DIR is complete. Its marker (DIR.complete) must be whole and match its
/// own digest, and every file it names must be there at its size. No file
/// is read, only listed: the receiver hashed each one as it was written,
/// and hashing a whole set at boot would take a minute on the stick. The
/// set's manifest, if so.
pub fn check_set(dir: &Path, limits: &Limits) -> io::Result<Vec<Entry>> {
    check_set_on(&RealDisk, dir, limits)
}

/// [`check_set`], on `disk`.
pub fn check_set_on(disk: &dyn Disk, dir: &Path, limits: &Limits) -> io::Result<Vec<Entry>> {
    let name = dir.file_name().and_then(|n| n.to_str()).ok_or_else(|| invalid("not a set's directory"))?;
    let marker = dir.with_file_name(format!("{name}.complete"));
    let text = read_capped(disk, &marker, limits.max_files as u64 * 400 + 200)?;
    let entries = manifest::parse_marker(&text, limits)?;
    for e in &entries {
        let m = disk.stat(&dir.join(&e.path)).map_err(|err| invalid(format!("{}: {err}", e.path)))?;
        if m.kind != Kind::File || m.len != e.size {
            return Err(invalid(format!("{}: not the {} bytes its marker says", e.path, e.size)));
        }
    }
    Ok(entries)
}

impl Stick {
    pub fn new(dir: impl Into<PathBuf>, running: Option<Set>) -> Stick {
        Stick { dir: dir.into(), mount: None, running, limits: LIMITS, disk: Arc::new(RealDisk), batch: BATCH }
    }

    /// The stick mounted at `mount`, its neochumby directory `dir`.
    pub fn mounted_at(mount: impl Into<PathBuf>, dir: impl Into<PathBuf>, running: Option<Set>) -> Stick {
        Stick { mount: Some(mount.into()), ..Stick::new(dir, running) }
    }

    /// Whether the stick is there to write to: its mount point is another
    /// device than its parent. Checked before every write that could create
    /// something.
    pub fn mounted(&self) -> io::Result<()> {
        match &self.mount {
            Some(m) if !self.disk.mounted(m) => {
                Err(io::Error::new(io::ErrorKind::NotFound, format!("no stick mounted at {}", m.display())))
            }
            _ => Ok(()),
        }
    }

    pub fn set_dir(&self, s: Set) -> PathBuf {
        self.dir.join(s.name())
    }

    fn marker(&self, s: Set) -> String {
        format!("{}.complete", s.name())
    }

    fn journal(&self, s: Set) -> String {
        format!("{}.journal", s.name())
    }

    /// The set `current` names; else the one in `current.new`, which a flip
    /// cut short by a power cut can leave on its own.
    pub fn current(&self) -> Option<Set> {
        ["current", "current.new"]
            .iter()
            .filter_map(|f| read_capped(&*self.disk, &self.dir.join(f), 16).ok())
            .find_map(|s| Set::parse(s.trim_end_matches('\n')))
    }

    /// `s`'s manifest, if it is complete ([`check_set`]).
    pub fn check(&self, s: Set) -> io::Result<Vec<Entry>> {
        check_set_on(&*self.disk, &self.set_dir(s), &self.limits)
    }

    /// The set the boot hook runs: the one `current` names if it is
    /// complete, else the other if that is. With its manifest.
    pub fn live(&self) -> Option<(Set, Vec<Entry>)> {
        let first = self.current().unwrap_or(Set::A);
        [first, first.other()].into_iter().find_map(|s| self.check(s).ok().map(|m| (s, m)))
    }

    /// After a rollback: no rounds until `resume`.
    pub fn held(&self) -> bool {
        self.disk.stat(&self.dir.join("hold")).is_ok()
    }

    /// The files `s` holds, as far as is known without reading them: its
    /// marker's if it is complete, else its journal's from a round cut
    /// short; only those there at their size.
    pub fn have(&self, s: Set) -> Vec<Entry> {
        let listed = self.check(s).unwrap_or_else(|_| {
            let cap = self.limits.max_files as u64 * 400;
            read_capped(&*self.disk, &self.dir.join(self.journal(s)), cap)
                .map(|j| manifest::parse_journal(&j))
                .unwrap_or_default()
        });
        let dir = self.set_dir(s);
        listed
            .into_iter()
            .filter(|e| self.disk.stat(&dir.join(&e.path)).is_ok_and(|m| m.kind == Kind::File && m.len == e.size))
            .collect()
    }

    fn writable(&self, s: Set) -> io::Result<()> {
        let live = self.live().map(|(l, _)| l);
        if self.running == Some(s) || live == Some(s) {
            let why = if live == Some(s) { "live" } else { "running" };
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("set {} is {why}", s.name())));
        }
        Ok(())
    }

    /// Starts writing `s` as `tree`, `have` being what it holds. The journal
    /// is written first, listing the files it keeps; then its marker goes,
    /// and from there on the set is incomplete. Then everything the tree
    /// does not name is removed: old files, a cut-short round's temporary
    /// files, and anything that is not a plain file or directory. Then the
    /// stick must have room for what is still to come.
    pub fn begin(&self, s: Set, tree: &[Entry], have: &[Entry]) -> io::Result<Writer<'_>> {
        self.writable(s)?;
        manifest::validate(tree, &self.limits)?;
        // Files kept on a journal's word, not a marker's, are hashed before
        // the set is marked complete: see Writer::finish.
        let from_journal = self.check(s).is_err();
        self.mounted()?;
        let d = &*self.disk;
        real_dir(d, &self.dir)?;
        let have: HashSet<&Entry> = have.iter().collect();
        let kept: Vec<Entry> = tree.iter().filter(|e| have.contains(e)).cloned().collect();
        write_whole(d, &self.dir, &self.journal(s), kept.iter().map(line).collect::<String>().as_bytes())?;
        remove_if_there(d, &self.dir.join(self.marker(s)))?;
        d.sync_dir(&self.dir)?;

        let dir = self.set_dir(s);
        real_dir(d, &dir)?;
        let files: HashSet<String> = tree.iter().map(|e| e.path.to_ascii_lowercase()).collect();
        let dirs: HashSet<String> = tree
            .iter()
            .flat_map(|e| e.path.match_indices('/').map(|(i, _)| e.path[..i].to_ascii_lowercase()).collect::<Vec<_>>())
            .collect();
        prune(d, &dir, "", &files, &dirs)?;
        room(d, &dir, tree, &have)?;
        let journal = d.append(&self.dir.join(self.journal(s)))?;
        let unverified = if from_journal { kept.clone() } else { Vec::new() };
        Ok(Writer {
            stick: self,
            set: s,
            dir,
            tree: tree.to_vec(),
            journal,
            listed: kept,
            pending: Vec::new(),
            pending_bytes: 0,
            unverified,
        })
    }

    /// Makes `to` live: checks it is complete, then rewrites `current`.
    pub fn flip(&self, to: Set) -> io::Result<()> {
        self.check(to)?;
        self.mounted()?;
        write_whole(&*self.disk, &self.dir, "current", format!("{}\n", to.name()).as_bytes())
    }

    /// Makes the other complete set live, and holds syncing, which would
    /// otherwise bring the tree straight back, until [`Stick::resume`].
    pub fn rollback(&self) -> io::Result<Set> {
        let (live, _) = self.live().ok_or_else(|| invalid("no complete set"))?;
        let to = live.other();
        self.check(to).map_err(|e| invalid(format!("set {} is not complete: {e}", to.name())))?;
        self.mounted()?;
        write_whole(&*self.disk, &self.dir, "hold", b"rolled back: syncing waits for `neochumby-sync resume`\n")?;
        self.flip(to)?;
        Ok(to)
    }

    pub fn resume(&self) -> io::Result<()> {
        self.mounted()?;
        remove_if_there(&*self.disk, &self.dir.join("hold"))?;
        self.disk.sync_dir(&self.dir)
    }

    /// The sets, for people.
    pub fn status(&self) -> String {
        let live = self.live().map(|(l, _)| l);
        let mut out = String::new();
        for s in [Set::A, Set::B] {
            let mut tags = String::new();
            if live == Some(s) {
                tags.push_str(", live");
            }
            if self.running == Some(s) {
                tags.push_str(", running");
            }
            match self.check(s) {
                Ok(m) => {
                    let bytes: u64 = m.iter().map(|e| e.size).sum();
                    let d = neochumby_proto::hex(&manifest::digest(&m));
                    out.push_str(&format!(
                        "{}: complete, {} files, {bytes} bytes, {}{tags}\n",
                        s.name(),
                        m.len(),
                        &d[..12]
                    ));
                }
                Err(e) => out.push_str(&format!("{}: not complete ({e}){tags}\n", s.name())),
            }
        }
        if self.held() {
            out.push_str("held: rolled back; `neochumby-sync resume` syncs again\n");
        }
        out
    }
}

/// A set being written.
pub struct Writer<'a> {
    stick: &'a Stick,
    set: Set,
    dir: PathBuf,
    tree: Vec<Entry>,
    journal: Box<dyn Output>,
    /// What the journal lists.
    listed: Vec<Entry>,
    /// Renamed into place, not yet synced or in the journal.
    pending: Vec<Entry>,
    pending_bytes: u64,
    /// Kept on a journal's word: hashed before the marker.
    unverified: Vec<Entry>,
}

impl Writer<'_> {
    /// Writes one file of the tree, its bytes coming from `next`: under a
    /// temporary name, hashed as it streams, then renamed into place. It is
    /// listed in the journal with the rest of its batch, after a sync.
    pub fn receive(&mut self, e: &Entry, next: impl FnMut() -> io::Result<Vec<u8>>) -> io::Result<()> {
        if self.tree.binary_search_by(|t| t.path.cmp(&e.path)).ok().map(|i| &self.tree[i]) != Some(e) {
            return Err(invalid(format!("{}: not in the manifest", e.path)));
        }
        self.stick.mounted()?;
        let parts: Vec<&str> = e.path.split('/').collect();
        let (name, parents) = parts.split_last().expect("a valid path");
        let disk = &*self.stick.disk;
        let mut d = self.dir.clone();
        for p in parents {
            d.push(p);
            real_dir(disk, &d)?;
        }
        let tmp = d.join(format!(".{name}.part"));
        remove_if_there(disk, &tmp)?;
        let r = write_hashed(disk, &tmp, e, next).and_then(|()| disk.rename(&tmp, &d.join(name)));
        if r.is_err() {
            let _ = disk.remove_file(&tmp);
        }
        r?;
        self.pending.push(e.clone());
        self.pending_bytes += e.size;
        if self.pending.len() >= self.stick.batch.files || self.pending_bytes >= self.stick.batch.bytes {
            self.flush()?;
        }
        Ok(())
    }

    /// The batch: one sync for its files and their directories, then its
    /// journal lines, then one sync for those.
    fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.stick.disk.sync();
        let lines: String = self.pending.iter().map(line).collect();
        self.journal.write_all(lines.as_bytes())?;
        self.stick.disk.sync();
        self.listed.append(&mut self.pending);
        self.pending_bytes = 0;
        Ok(())
    }

    /// Ends the set. Every file must be there at its size. Those received
    /// were hashed as they came, and those a marker vouched for are taken
    /// as they are; but those kept on a journal's word, from a round cut
    /// short, are hashed now. A power cut can leave a renamed file sharing
    /// its clusters with its temporary name, and a cheap stick can reorder
    /// its own writes, so the sizes can match over the wrong bytes. A file
    /// that fails, or can't be read at all (a damaged FAT chain gives EIO),
    /// is removed and dropped from the journal, for the next round to send
    /// again; otherwise it would fail every round. Then the marker is
    /// written, last. Hashing a large set takes minutes on the stick, so
    /// `progress(done, total)` is called every 30 seconds meanwhile.
    pub fn finish(mut self, mut progress: impl FnMut(u64, u64)) -> io::Result<()> {
        self.flush()?;
        let total: u64 = self.unverified.iter().map(|e| e.size).sum();
        let (mut done, mut said) = (0u64, Instant::now());
        let mut bad = Vec::new();
        for e in &self.unverified {
            let r = hash_file(&*self.stick.disk, &self.dir.join(&e.path), &mut |n| {
                done += n;
                if said.elapsed() >= PROGRESS {
                    progress(done, total);
                    said = Instant::now();
                }
            });
            if r.as_ref().ok() != Some(&(e.size, e.sha256)) {
                // A stick pulled mid-hash fails every read: nothing is
                // removed then, and the round fails as it is.
                self.stick.mounted()?;
                bad.push(e.path.clone());
            }
        }
        if !bad.is_empty() {
            for p in &bad {
                let _ = self.stick.disk.remove_file(&self.dir.join(p));
            }
            self.listed.retain(|e| !bad.contains(&e.path));
            let st = self.stick;
            let journal: String = self.listed.iter().map(line).collect();
            write_whole(&*st.disk, &st.dir, &st.journal(self.set), journal.as_bytes())?;
            return Err(invalid(format!("{}: not the bytes the journal says; sent again next round", bad.join(", "))));
        }
        for e in &self.tree {
            let m =
                self.stick.disk.stat(&self.dir.join(&e.path)).map_err(|err| invalid(format!("{}: {err}", e.path)))?;
            if m.kind != Kind::File || m.len != e.size {
                return Err(invalid(format!("{}: not {} bytes", e.path, e.size)));
            }
        }
        let st = self.stick;
        st.mounted()?;
        let d = &*st.disk;
        write_whole(d, &st.dir, &st.marker(self.set), manifest::marker(&self.tree).as_bytes())?;
        remove_if_there(d, &st.dir.join(st.journal(self.set)))?;
        d.sync_dir(&st.dir)
    }
}

fn write_hashed(
    disk: &dyn Disk,
    tmp: &Path,
    e: &Entry,
    mut next: impl FnMut() -> io::Result<Vec<u8>>,
) -> io::Result<()> {
    let mut f = disk.create(tmp, 0o755)?;
    let mut h = Context::new(&SHA256);
    let mut got = 0u64;
    while got < e.size {
        let chunk = next()?;
        if chunk.is_empty() || got + chunk.len() as u64 > e.size {
            return Err(invalid(format!("{}: not {} bytes", e.path, e.size)));
        }
        f.write_all(&chunk)?;
        h.update(&chunk);
        got += chunk.len() as u64;
    }
    if h.finish().as_ref() != e.sha256 {
        return Err(invalid(format!("{}: its SHA-256 does not match", e.path)));
    }
    Ok(())
}

/// A round that fails keeps what it wrote: the files received whole go in
/// the journal, in the same order as ever, for the next round.
impl Drop for Writer<'_> {
    fn drop(&mut self) {
        if self.stick.mounted().is_ok() {
            let _ = self.flush();
        }
    }
}

/// A file's size and SHA-256, read in full, `read(n)` told of each part.
fn hash_file(disk: &dyn Disk, p: &Path, read: &mut dyn FnMut(u64)) -> io::Result<(u64, [u8; 32])> {
    let mut f = disk.open(p)?;
    let mut h = Context::new(&SHA256);
    let mut buf = vec![0u8; 64 << 10];
    let mut n = 0u64;
    loop {
        match f.read(&mut buf)? {
            0 => break,
            k => {
                h.update(&buf[..k]);
                n += k as u64;
                read(k as u64);
            }
        }
    }
    Ok((n, h.finish().as_ref().try_into().expect("32 bytes")))
}

/// Whether the stick has room for what `tree` still needs, once `dir` holds
/// only files the tree names. A file replaced keeps its old bytes until its
/// new ones are renamed over it, so the largest of those is counted too.
fn room(disk: &dyn Disk, dir: &Path, tree: &[Entry], have: &HashSet<&Entry>) -> io::Result<()> {
    let (mut need, mut freed, mut largest) = (0u64, 0u64, 0u64);
    for e in tree.iter().filter(|e| !have.contains(e)) {
        need += e.size;
        if let Ok(m) = disk.stat(&dir.join(&e.path)) {
            freed += m.len;
            largest = largest.max(m.len);
        }
    }
    let free = disk.available(dir)?;
    if free + freed < need + largest {
        let short = need + largest - free - freed;
        return Err(io::Error::new(io::ErrorKind::StorageFull, format!("the stick needs {short} more bytes free")));
    }
    Ok(())
}

/// Removes what is in `rel` under `base` and not named: files not in
/// `files`, directories not in `dirs` (lower case, as FAT compares), and
/// anything else. A symbolic link is removed, never followed.
fn prune(disk: &dyn Disk, base: &Path, rel: &str, files: &HashSet<String>, dirs: &HashSet<String>) -> io::Result<()> {
    let here = base.join(rel);
    for (name, t) in disk.list(&here)? {
        let r = match name.to_str() {
            Some(n) if rel.is_empty() => n.to_string(),
            Some(n) => format!("{rel}/{n}"),
            None => String::new(),
        };
        let path = here.join(&name);
        let lower = r.to_ascii_lowercase();
        // Something already gone is fine: a filesystem may keep a file of
        // its own beside another and remove the two together (macOS does,
        // on FAT).
        if t == Kind::Dir && dirs.contains(&lower) {
            prune(disk, base, &r, files, dirs)?;
        } else if t == Kind::Dir {
            gone(disk.remove_dir_all(&path))?;
        } else if !(t == Kind::File && files.contains(&lower)) {
            gone(disk.remove_file(&path))?;
        }
    }
    Ok(())
}

fn gone(r: io::Result<()>) -> io::Result<()> {
    match r {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// Makes sure `p` is a directory, not a link to one, making it if missing.
fn real_dir(disk: &dyn Disk, p: &Path) -> io::Result<()> {
    match disk.stat(p) {
        Ok(m) if m.kind == Kind::Dir => Ok(()),
        Ok(_) => Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{}: not a directory", p.display()))),
        // Made durable with the next sync, before anything in it is listed.
        Err(e) if e.kind() == io::ErrorKind::NotFound => disk.create_dir(p),
        Err(e) => Err(e),
    }
}

/// Writes `name` in `dir` whole or not at all, and on the stick before it
/// returns: `name.new`, fsynced, then renamed over it, then a whole sync()
/// (and the directory fsynced, which then has nothing left to write, but
/// says if the stick has gone). Not the directory's fsync alone: on FAT a file's length is in its
/// directory entry, and 2.6.16's vfat makes a renamed file's new entry with
/// length 0 and fills it in only when the file's inode is next written (an
/// fsync of the file, or a sync()), so a power cut after a directory fsync
/// still found `current` and the set's marker empty, and the boot ran the
/// old set. On that kernel an fsync costs a whole sync anyway.
fn write_whole(disk: &dyn Disk, dir: &Path, name: &str, data: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.new"));
    remove_if_there(disk, &tmp)?;
    let mut f = disk.create(&tmp, 0o644)?;
    f.write_all(data)?;
    f.sync()?;
    drop(f);
    disk.rename(&tmp, &dir.join(name))?;
    disk.sync();
    disk.sync_dir(dir)
}

fn remove_if_there(disk: &dyn Disk, p: &Path) -> io::Result<()> {
    gone(disk.remove_file(p))
}

/// A small file, refused if longer than `cap`, never through a link.
fn read_capped(disk: &dyn Disk, p: &Path, cap: u64) -> io::Result<String> {
    let f = disk.open(p)?;
    let mut s = String::new();
    f.take(cap + 1).read_to_string(&mut s)?;
    if s.len() as u64 > cap {
        return Err(invalid(format!("{}: too long", p.display())));
    }
    Ok(s)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs::{self, File};

    use super::*;
    use crate::manifest::tests::entry;

    pub fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("neochumby-sync-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Writes a whole set through a Writer, as a round would.
    pub fn write_set(st: &Stick, s: Set, files: &[(&str, &[u8])]) -> io::Result<()> {
        let tree: Vec<Entry> = files.iter().map(|(p, d)| entry(p, d)).collect();
        let have = st.have(s);
        let mut w = st.begin(s, &tree, &have)?;
        for ((_, d), e) in files.iter().zip(&tree) {
            if have.contains(e) {
                continue;
            }
            let mut chunks = d.chunks(3).map(|c| c.to_vec()).collect::<Vec<_>>().into_iter();
            w.receive(e, || Ok(chunks.next().unwrap_or_default()))?;
        }
        w.finish(|_, _| {})
    }

    const V1: &[(&str, &[u8])] = &[("apps/home/manifest", b"home=neochumby-home\n"), ("neochumby", b"session 1")];
    const V2: &[(&str, &[u8])] = &[
        ("apps/doom/doom1.wad", b"IWAD"),
        ("apps/home/manifest", b"home=neochumby-home\n"),
        ("neochumby", b"session 2"),
    ];

    #[test]
    fn write_flip_rollback() {
        let root = tempdir("flip");
        let st = Stick::new(root.join("neochumby"), None);
        assert!(st.live().is_none());
        write_set(&st, Set::A, V1).unwrap();
        st.flip(Set::A).unwrap();
        assert_eq!(st.current(), Some(Set::A));
        assert_eq!(st.live().unwrap().0, Set::A);
        assert_eq!(fs::read(root.join("neochumby/a/neochumby")).unwrap(), b"session 1");
        assert!(!root.join("neochumby/a.journal").exists());

        // The live set cannot be written; the standby can.
        assert!(st.begin(Set::A, &[], &[]).is_err());
        write_set(&st, Set::B, V2).unwrap();
        st.flip(Set::B).unwrap();
        assert_eq!(st.live().unwrap().0, Set::B);
        assert_eq!(fs::read(root.join("neochumby/b/apps/doom/doom1.wad")).unwrap(), b"IWAD");

        // Rollback: back to a, held; then a round may write b again.
        assert_eq!(st.rollback().unwrap(), Set::A);
        assert_eq!(st.live().unwrap().0, Set::A);
        assert!(st.held());
        st.resume().unwrap();
        assert!(!st.held());
        // b is now standby, complete: rollback goes forward again.
        assert_eq!(st.rollback().unwrap(), Set::B);
        st.resume().unwrap();

        // The next round into a keeps what a has and removes what the tree
        // no longer names.
        fs::write(root.join("neochumby/a/stray"), b"x").unwrap();
        let have = st.have(Set::A);
        assert_eq!(have.len(), 2);
        write_set(&st, Set::A, V2).unwrap();
        assert!(!root.join("neochumby/a/stray").exists());
        assert_eq!(fs::read(root.join("neochumby/a/neochumby")).unwrap(), b"session 2");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_running_set_is_never_written() {
        let root = tempdir("running");
        let st = Stick::new(root.join("neochumby"), Some(Set::B));
        write_set(&st, Set::A, V1).unwrap();
        st.flip(Set::A).unwrap();
        // b is standby but running: refused.
        let e = st.begin(Set::B, &[entry("neochumby", b"x")], &[]).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn markers_decide() {
        let root = tempdir("markers");
        let st = Stick::new(root.join("neochumby"), None);
        write_set(&st, Set::A, V1).unwrap();
        write_set(&st, Set::B, V2).unwrap();
        st.flip(Set::B).unwrap();
        let dir = root.join("neochumby");

        // A torn current: current.new is used, else a.
        fs::remove_file(dir.join("current")).unwrap();
        assert_eq!(st.live().unwrap().0, Set::A);
        fs::write(dir.join("current.new"), "b\n").unwrap();
        assert_eq!(st.live().unwrap().0, Set::B);
        fs::write(dir.join("current"), "garbage").unwrap();
        assert_eq!(st.current(), Some(Set::B));
        fs::write(dir.join("current"), "b\n").unwrap();

        // b's marker cut short, or a file of b's the wrong size, or gone:
        // a is live.
        let marker = fs::read_to_string(dir.join("b.complete")).unwrap();
        fs::write(dir.join("b.complete"), &marker[..marker.len() - 5]).unwrap();
        assert_eq!(st.live().unwrap().0, Set::A);
        fs::write(dir.join("b.complete"), &marker).unwrap();
        assert_eq!(st.live().unwrap().0, Set::B);
        fs::write(dir.join("b/neochumby"), b"session 22").unwrap();
        assert!(check_set(&dir.join("b"), &LIMITS).is_err());
        assert_eq!(st.live().unwrap().0, Set::A);
        fs::remove_file(dir.join("b/neochumby")).unwrap();
        assert_eq!(st.live().unwrap().0, Set::A);
        // A flip to an incomplete set is refused.
        assert!(st.flip(Set::B).is_err());
        // Neither: none.
        fs::remove_file(dir.join("a.complete")).unwrap();
        assert!(st.live().is_none());
        assert!(st.rollback().is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_cut_short_round_resumes_from_its_journal() {
        let root = tempdir("journal");
        let st = Stick::new(root.join("neochumby"), None);
        write_set(&st, Set::A, V1).unwrap();
        st.flip(Set::A).unwrap();

        // Into b: one file lands, the next is cut short mid-stream.
        let tree: Vec<Entry> = V2.iter().map(|(p, d)| entry(p, d)).collect();
        let mut w = st.begin(Set::B, &tree, &[]).unwrap();
        let mut once = Some(b"IWAD".to_vec());
        w.receive(&tree[0], || Ok(once.take().unwrap_or_default())).unwrap();
        let mut cut = Some(b"home".to_vec());
        let r = w.receive(&tree[1], || cut.take().ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof)));
        assert!(r.is_err());
        drop(w);
        assert!(st.check(Set::B).is_err(), "no marker");
        assert_eq!(st.live().unwrap().0, Set::A);
        // The journal says what is good.
        assert_eq!(st.have(Set::B), [tree[0].clone()]);
        // A temporary file left by a cut (here, a power cut) is removed.
        fs::write(root.join("neochumby/b/apps/home/.manifest.part"), b"half").unwrap();
        write_set(&st, Set::B, V2).unwrap();
        assert!(!root.join("neochumby/b/apps/home/.manifest.part").exists());
        assert_eq!(st.check(Set::B).unwrap(), tree);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn bad_bytes_are_refused() {
        let root = tempdir("bytes");
        let st = Stick::new(root.join("neochumby"), None);
        let tree = vec![entry("neochumby", b"session")];
        let mut w = st.begin(Set::A, &tree, &[]).unwrap();
        // Wrong bytes of the right length; too many; a file not in the tree.
        let mut once = Some(b"sessiom".to_vec());
        assert!(w.receive(&tree[0], || Ok(once.take().unwrap_or_default())).is_err());
        let mut long = Some(b"session!".to_vec());
        assert!(w.receive(&tree[0], || Ok(long.take().unwrap_or_default())).is_err());
        assert!(w.receive(&entry("other", b"x"), || Ok(b"x".to_vec())).is_err());
        assert!(!root.join("neochumby/a/neochumby").exists());
        assert!(fs::read_dir(root.join("neochumby/a")).unwrap().next().is_none(), "no temporary file left");
        assert!(w.finish(|_, _| {}).is_err(), "incomplete");
        assert!(st.check(Set::A).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn links_are_never_followed() {
        let root = tempdir("links");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let st = Stick::new(root.join("neochumby"), None);
        fs::create_dir_all(root.join("neochumby/a")).unwrap();
        // A link in the set where a directory of the tree goes, and one
        // where a file goes: both removed, nothing written outside.
        std::os::unix::fs::symlink(&outside, root.join("neochumby/a/apps")).unwrap();
        std::os::unix::fs::symlink(outside.join("victim"), root.join("neochumby/a/neochumby")).unwrap();
        write_set(&st, Set::A, V1).unwrap();
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        assert!(fs::symlink_metadata(root.join("neochumby/a/apps")).unwrap().is_dir());
        assert!(fs::symlink_metadata(root.join("neochumby/a/neochumby")).unwrap().is_file());

        // A set directory that is a link: refused.
        std::os::unix::fs::symlink(&outside, root.join("neochumby/b")).unwrap();
        assert!(write_set(&st, Set::B, V1).is_err());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn nothing_is_written_without_the_stick() {
        let root = tempdir("unmounted");
        // A "mount point" on the same device as its parent: no stick there.
        fs::create_dir_all(root.join("usb")).unwrap();
        let st = Stick::mounted_at(root.join("usb"), root.join("usb/neochumby"), None);
        assert_eq!(st.mounted().unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(write_set(&st, Set::A, V1).is_err());
        assert!(st.flip(Set::A).is_err() && st.resume().is_err());
        assert!(!root.join("usb/neochumby").exists(), "nothing made under the mount point");
        // The stick goes mid-round: the next file is refused.
        let ok = Stick::new(root.join("usb/neochumby"), None);
        let tree: Vec<Entry> = V1.iter().map(|(p, d)| entry(p, d)).collect();
        let mut w = ok.begin(Set::A, &tree, &[]).unwrap();
        w.stick = &st;
        assert!(w.receive(&tree[0], || Ok(V1[0].1.to_vec())).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn files_kept_on_a_journals_word_are_hashed() {
        let root = tempdir("rehash");
        let st = Stick::new(root.join("neochumby"), None);
        let tree: Vec<Entry> = V2.iter().map(|(p, d)| entry(p, d)).collect();
        // A round cut short after the wad.
        let mut w = st.begin(Set::A, &tree, &[]).unwrap();
        w.receive(&tree[0], || Ok(b"IWAD".to_vec())).unwrap();
        drop(w);
        assert_eq!(st.have(Set::A), [tree[0].clone()]);
        // Its bytes go bad, the size the same (a torn rename, a stick that
        // reordered its writes).
        fs::write(root.join("neochumby/a/apps/doom/doom1.wad"), b"IWAX").unwrap();
        let e = write_set(&st, Set::A, V2).unwrap_err();
        assert!(e.to_string().contains("doom1.wad"), "{e}");
        assert!(st.check(Set::A).is_err(), "not marked complete");
        assert!(st.have(Set::A).iter().all(|e| !e.path.ends_with("doom1.wad")), "dropped from the journal");
        // The next round sends it again.
        write_set(&st, Set::A, V2).unwrap();
        assert_eq!(fs::read(root.join("neochumby/a/apps/doom/doom1.wad")).unwrap(), b"IWAD");
        assert_eq!(st.check(Set::A).unwrap(), tree);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_full_stick_is_refused_before_writing() {
        let root = tempdir("full");
        let mut st = Stick::new(root.join("neochumby"), None);
        st.limits = Limits { max_files: 10, max_file: u64::MAX, max_total: u64::MAX };
        let huge = Entry { path: "neochumby".into(), size: 1 << 60, sha256: [0; 32] };
        let e = st.begin(Set::A, &[huge], &[]).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::StorageFull, "{e}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_kept_file_that_cannot_be_read_is_sent_again() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempdir("unreadable");
        let st = Stick::new(root.join("neochumby"), None);
        let tree: Vec<Entry> = V2.iter().map(|(p, d)| entry(p, d)).collect();
        let mut w = st.begin(Set::A, &tree, &[]).unwrap();
        w.receive(&tree[0], || Ok(b"IWAD".to_vec())).unwrap();
        drop(w);
        // Reads of it fail (standing in for EIO from a damaged FAT chain).
        let wad = root.join("neochumby/a/apps/doom/doom1.wad");
        fs::set_permissions(&wad, fs::Permissions::from_mode(0o000)).unwrap();
        if File::open(&wad).is_ok() {
            // Running as root: permissions don't stop reads. Nothing to test.
            fs::remove_dir_all(&root).unwrap();
            return;
        }
        let e = write_set(&st, Set::A, V2).unwrap_err();
        assert!(e.to_string().contains("doom1.wad"), "{e}");
        assert!(!wad.exists(), "removed");
        assert!(st.have(Set::A).iter().all(|e| !e.path.ends_with("doom1.wad")), "dropped from the journal");
        write_set(&st, Set::A, V2).unwrap();
        assert_eq!(st.check(Set::A).unwrap(), tree);
        fs::remove_dir_all(&root).unwrap();
    }
}
