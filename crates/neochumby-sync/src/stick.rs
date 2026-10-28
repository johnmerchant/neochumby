//! The stick's side: the two sets, their markers, writing the standby set,
//! the flip and rollback. Every write is inside the stick's neochumby
//! directory, to paths [`manifest::valid_path`] takes, through no symbolic
//! link, and never to the live set or the one the Chumby is running from.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use ring::digest::{Context, SHA256};

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

/// The stick's neochumby directory.
pub struct Stick {
    pub dir: PathBuf,
    /// The set the session runs from, if it runs from one: never written.
    pub running: Option<Set>,
    pub limits: Limits,
}

/// `neochumby check-set DIR`, which the boot hook runs: whether the set in
/// DIR is complete. Its marker (DIR.complete) must be whole and match its
/// own digest, and every file it names must be there at its size. No file
/// is read, only listed: the receiver hashed each one as it was written,
/// and hashing a whole set at boot would take a minute on the stick. The
/// set's manifest, if so.
pub fn check_set(dir: &Path, limits: &Limits) -> io::Result<Vec<Entry>> {
    let name = dir.file_name().and_then(|n| n.to_str()).ok_or_else(|| invalid("not a set's directory"))?;
    let marker = dir.with_file_name(format!("{name}.complete"));
    let text = read_capped(&marker, limits.max_files as u64 * 400 + 200)?;
    let entries = manifest::parse_marker(&text, limits)?;
    for e in &entries {
        let m = fs::symlink_metadata(dir.join(&e.path)).map_err(|err| invalid(format!("{}: {err}", e.path)))?;
        if !m.is_file() || m.len() != e.size {
            return Err(invalid(format!("{}: not the {} bytes its marker says", e.path, e.size)));
        }
    }
    Ok(entries)
}

impl Stick {
    pub fn new(dir: impl Into<PathBuf>, running: Option<Set>) -> Stick {
        Stick { dir: dir.into(), running, limits: LIMITS }
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
            .filter_map(|f| read_capped(&self.dir.join(f), 16).ok())
            .find_map(|s| Set::parse(s.trim_end_matches('\n')))
    }

    /// `s`'s manifest, if it is complete ([`check_set`]).
    pub fn check(&self, s: Set) -> io::Result<Vec<Entry>> {
        check_set(&self.set_dir(s), &self.limits)
    }

    /// The set the boot hook runs: the one `current` names if it is
    /// complete, else the other if that is. With its manifest.
    pub fn live(&self) -> Option<(Set, Vec<Entry>)> {
        let first = self.current().unwrap_or(Set::A);
        [first, first.other()].into_iter().find_map(|s| self.check(s).ok().map(|m| (s, m)))
    }

    /// After a rollback: no rounds until `resume`.
    pub fn held(&self) -> bool {
        fs::symlink_metadata(self.dir.join("hold")).is_ok()
    }

    /// The files `s` holds, as far as is known without reading them: its
    /// marker's if it is complete, else its journal's from a round cut
    /// short; only those there at their size.
    pub fn have(&self, s: Set) -> Vec<Entry> {
        let listed = self.check(s).unwrap_or_else(|_| {
            let cap = self.limits.max_files as u64 * 400;
            read_capped(&self.dir.join(self.journal(s)), cap).map(|j| manifest::parse_journal(&j)).unwrap_or_default()
        });
        let dir = self.set_dir(s);
        listed
            .into_iter()
            .filter(|e| fs::symlink_metadata(dir.join(&e.path)).is_ok_and(|m| m.is_file() && m.len() == e.size))
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
    /// files, and anything that is not a plain file or directory.
    pub fn begin(&self, s: Set, tree: &[Entry], have: &[Entry]) -> io::Result<Writer<'_>> {
        self.writable(s)?;
        manifest::validate(tree, &self.limits)?;
        real_dir(&self.dir)?;
        let have: HashSet<&Entry> = have.iter().collect();
        let kept: String = tree.iter().filter(|e| have.contains(e)).map(line).collect();
        write_whole(&self.dir, &self.journal(s), kept.as_bytes())?;
        remove_if_there(&self.dir.join(self.marker(s)))?;
        sync_dir(&self.dir)?;

        let dir = self.set_dir(s);
        real_dir(&dir)?;
        let files: HashSet<String> = tree.iter().map(|e| e.path.to_ascii_lowercase()).collect();
        let dirs: HashSet<String> = tree
            .iter()
            .flat_map(|e| e.path.match_indices('/').map(|(i, _)| e.path[..i].to_ascii_lowercase()).collect::<Vec<_>>())
            .collect();
        prune(&dir, "", &files, &dirs)?;
        let journal =
            OpenOptions::new().append(true).custom_flags(libc::O_NOFOLLOW).open(self.dir.join(self.journal(s)))?;
        Ok(Writer { stick: self, set: s, dir, tree: tree.to_vec(), journal })
    }

    /// Makes `to` live: checks it is complete, then rewrites `current`.
    pub fn flip(&self, to: Set) -> io::Result<()> {
        self.check(to)?;
        write_whole(&self.dir, "current", format!("{}\n", to.name()).as_bytes())
    }

    /// Makes the other complete set live, and holds syncing, which would
    /// otherwise bring the tree straight back, until [`Stick::resume`].
    pub fn rollback(&self) -> io::Result<Set> {
        let (live, _) = self.live().ok_or_else(|| invalid("no complete set"))?;
        let to = live.other();
        self.check(to).map_err(|e| invalid(format!("set {} is not complete: {e}", to.name())))?;
        write_whole(&self.dir, "hold", b"rolled back: syncing waits for `neochumby-sync resume`\n")?;
        self.flip(to)?;
        Ok(to)
    }

    pub fn resume(&self) -> io::Result<()> {
        remove_if_there(&self.dir.join("hold"))?;
        sync_dir(&self.dir)
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
    journal: File,
}

impl Writer<'_> {
    /// Writes one file of the tree, its bytes coming from `next`: under a
    /// temporary name, hashed as it streams, fsynced, renamed into place,
    /// its directory fsynced, and then listed in the journal.
    pub fn receive(&mut self, e: &Entry, next: impl FnMut() -> io::Result<Vec<u8>>) -> io::Result<()> {
        if self.tree.binary_search_by(|t| t.path.cmp(&e.path)).ok().map(|i| &self.tree[i]) != Some(e) {
            return Err(invalid(format!("{}: not in the manifest", e.path)));
        }
        let parts: Vec<&str> = e.path.split('/').collect();
        let (name, parents) = parts.split_last().expect("a valid path");
        let mut d = self.dir.clone();
        for p in parents {
            d.push(p);
            real_dir(&d)?;
        }
        let tmp = d.join(format!(".{name}.part"));
        remove_if_there(&tmp)?;
        let r = write_hashed(&tmp, e, next).and_then(|()| fs::rename(&tmp, d.join(name)));
        if r.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        r?;
        sync_dir(&d)?;
        self.journal.write_all(line(e).as_bytes())?;
        self.journal.sync_data()
    }

    /// Ends the set: every file is there at its size (those received were
    /// hashed as they came; the rest were kept, as its marker or journal
    /// said), so the marker is written, last.
    pub fn finish(self) -> io::Result<()> {
        for e in &self.tree {
            let m =
                fs::symlink_metadata(self.dir.join(&e.path)).map_err(|err| invalid(format!("{}: {err}", e.path)))?;
            if !m.is_file() || m.len() != e.size {
                return Err(invalid(format!("{}: not {} bytes", e.path, e.size)));
            }
        }
        let st = self.stick;
        drop(self.journal);
        write_whole(&st.dir, &st.marker(self.set), manifest::marker(&self.tree).as_bytes())?;
        remove_if_there(&st.dir.join(st.journal(self.set)))?;
        sync_dir(&st.dir)
    }
}

fn write_hashed(tmp: &Path, e: &Entry, mut next: impl FnMut() -> io::Result<Vec<u8>>) -> io::Result<()> {
    let mut f = create_new(tmp, 0o755)?;
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
    f.sync_all()
}

/// Removes what is in `rel` under `base` and not named: files not in
/// `files`, directories not in `dirs` (lower case, as FAT compares), and
/// anything else. A symbolic link is removed, never followed.
fn prune(base: &Path, rel: &str, files: &HashSet<String>, dirs: &HashSet<String>) -> io::Result<()> {
    for e in fs::read_dir(base.join(rel))? {
        let e = e?;
        let r = match e.file_name().to_str() {
            Some(n) if rel.is_empty() => n.to_string(),
            Some(n) => format!("{rel}/{n}"),
            None => String::new(),
        };
        let lower = r.to_ascii_lowercase();
        let t = e.file_type()?;
        // Something already gone is fine: a filesystem may keep a file of
        // its own beside another and remove the two together (macOS does,
        // on FAT).
        let gone = |r: io::Result<()>| match r {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        };
        if t.is_dir() && dirs.contains(&lower) {
            prune(base, &r, files, dirs)?;
        } else if t.is_dir() {
            gone(fs::remove_dir_all(e.path()))?;
        } else if !(t.is_file() && files.contains(&lower)) {
            gone(fs::remove_file(e.path()))?;
        }
    }
    Ok(())
}

/// Makes sure `p` is a directory, not a link to one, making it if missing.
fn real_dir(p: &Path) -> io::Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{}: not a directory", p.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(p)?;
            sync_dir(p.parent().unwrap_or(Path::new(".")))
        }
        Err(e) => Err(e),
    }
}

/// Writes `name` in `dir` whole or not at all: `name.new`, fsynced, then
/// renamed over it, then the directory fsynced.
fn write_whole(dir: &Path, name: &str, data: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{name}.new"));
    remove_if_there(&tmp)?;
    let mut f = create_new(&tmp, 0o644)?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, dir.join(name))?;
    sync_dir(dir)
}

fn create_new(p: &Path, mode: u32) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).mode(mode).custom_flags(libc::O_NOFOLLOW).open(p)
}

fn remove_if_there(p: &Path) -> io::Result<()> {
    match fs::remove_file(p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// Makes `d`'s entries durable. A filesystem that cannot fsync a directory
/// says EINVAL; a whole sync() does it then.
fn sync_dir(d: &Path) -> io::Result<()> {
    match File::open(d)?.sync_all() {
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
            unsafe { libc::sync() };
            Ok(())
        }
        r => r,
    }
}

/// A small file, refused if longer than `cap`, never through a link.
fn read_capped(p: &Path, cap: u64) -> io::Result<String> {
    let f = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(p)?;
    let mut s = String::new();
    f.take(cap + 1).read_to_string(&mut s)?;
    if s.len() as u64 > cap {
        return Err(invalid(format!("{}: too long", p.display())));
    }
    Ok(s)
}

#[cfg(test)]
pub(crate) mod tests {
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
        w.finish()
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
        assert!(w.finish().is_err(), "incomplete");
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
}
