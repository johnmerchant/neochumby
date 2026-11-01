//! Power cuts at every point of a round: sync's stick code run over a model
//! of the stick that loses power after its Nth operation, for every N, then
//! booted (the hook's choice of set, as boot/userhook2 makes it, and
//! check-set) and synced again.
//!
//! The model is FAT, taken at its weakest (as the stock 2.6.16, which
//! checks sets too, does it):
//!
//! - A file's bytes are on the stick only once that file is fsynced, or
//!   after a sync(). Bytes not yet there come back as they were before,
//!   as written, half written (a torn append), or as garbage of the
//!   written length.
//! - A directory's entries (a file made, removed or renamed, a directory
//!   made or removed) are on the stick only once that directory is
//!   fsynced, or after a sync(). Each one not yet there may be lost or
//!   kept, independently and in any order.
//! - A file renamed has a new directory entry, and on FAT a file's length
//!   lives in its entry: 2.6.16's vfat makes the new one with length 0 and
//!   fills it in only when the file's inode is next written, by an fsync of
//!   that file or a sync(). An fsync of the directory doesn't. Until then,
//!   across a cut, the file comes back empty.
//! - A rename isn't atomic: across a cut, the old name, the new name, or
//!   both may be left. Both names then share one chain of clusters, and FAT
//!   has no link count: removing either name, or renaming over it, frees
//!   the clusters the other still uses, whose bytes are then garbage.
//!
//! After each cut, whatever the boot picks must be a complete set, the old
//! tree or the new one, never anything else; and the next rounds must
//! bring the stick to the new tree.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io::{self, Cursor, Read};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use neochumby_proto::Msg;

use crate::disk::{Disk, Kind, Meta, Output};
use crate::manifest::HEADER;
use crate::receive::{self, Outcome};
use crate::serve::{self, Hashes};
use crate::stick::{Batch, Set, Stick, check_set_on};
use crate::{LIMITS, Link, Timing};

// The model.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Node {
    File(u64),
    Dir,
}

#[derive(Clone, Default, Debug)]
struct Inode {
    /// What reads give now.
    data: Vec<u8>,
    /// What is on the stick.
    durable: Vec<u8>,
    /// Written since it was last on the stick.
    dirty: bool,
    /// Renamed since its inode was last written: its entry says length 0.
    entry_stale: bool,
}

/// A change to the names, made but perhaps not on the stick yet.
#[derive(Clone, Debug)]
enum Change {
    Create(String, Node),
    Remove(String),
    Rename(String, String),
}

#[derive(Clone, Default)]
struct Fs {
    /// Absolute path to what it names, as reads see them.
    names: BTreeMap<String, Node>,
    /// The names as they are on the stick.
    durable: BTreeMap<String, Node>,
    /// Changes in `names` not yet in `durable`, in order.
    pending: Vec<Change>,
    inodes: HashMap<u64, Inode>,
    next: u64,
    /// Operations so far, and the one after which the power goes.
    ops: usize,
    cut_after: Option<usize>,
    dead: bool,
}

fn parent(p: &str) -> &str {
    p.rsplit_once('/').map_or("/", |(a, _)| if a.is_empty() { "/" } else { a })
}

fn under(p: &str, dir: &str) -> bool {
    p == dir || p.starts_with(&format!("{dir}/"))
}

fn lost() -> io::Error {
    io::Error::other("power lost")
}

fn not_found(p: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{p}: no such file"))
}

/// A small, fixed random number generator: the same seed, the same cut.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

impl Fs {
    fn empty() -> Fs {
        let mut fs = Fs::default();
        for d in ["/", "/stick"] {
            fs.names.insert(d.into(), Node::Dir);
            fs.durable.insert(d.into(), Node::Dir);
        }
        fs
    }

    fn alive(&self) -> io::Result<()> {
        if self.dead { Err(lost()) } else { Ok(()) }
    }

    /// One operation done: the power may go now.
    fn tick(&mut self) {
        self.ops += 1;
        if self.cut_after == Some(self.ops) {
            self.dead = true;
        }
    }

    fn file(&self, p: &str) -> io::Result<u64> {
        match self.names.get(p) {
            Some(Node::File(i)) => Ok(*i),
            Some(Node::Dir) => Err(io::Error::other(format!("{p}: a directory"))),
            None => Err(not_found(p)),
        }
    }

    fn need_dir(&self, p: &str) -> io::Result<()> {
        match self.names.get(parent(p)) {
            Some(Node::Dir) => Ok(()),
            _ => Err(not_found(parent(p))),
        }
    }

    /// `p`'s name goes. If another name still uses its clusters (a rename
    /// a cut tore), FAT frees them all the same: that name's bytes are
    /// garbage now.
    fn unlink(&mut self, p: &str) {
        if let Some(Node::File(i)) = self.names.remove(p)
            && self.names.values().any(|n| *n == Node::File(i))
        {
            let ino = self.inodes.get_mut(&i).expect("an inode");
            ino.data = vec![0xee; ino.data.len()];
            ino.durable = ino.data.clone();
        }
    }

    fn apply(names: &mut BTreeMap<String, Node>, c: &Change) {
        match c {
            Change::Create(p, n) => {
                names.insert(p.clone(), *n);
            }
            Change::Remove(p) => names.retain(|q, _| !under(q, p)),
            Change::Rename(f, t) => {
                if let Some(n) = names.remove(f) {
                    names.insert(t.clone(), n);
                }
            }
        }
    }

    /// The changes in `dir` (or all), now on the stick.
    fn settle(&mut self, dir: Option<&str>) {
        let touches = |c: &Change, d: &str| match c {
            Change::Create(p, _) | Change::Remove(p) => parent(p) == d,
            Change::Rename(f, t) => parent(f) == d || parent(t) == d,
        };
        let mut keep = Vec::new();
        for c in std::mem::take(&mut self.pending) {
            if dir.is_none_or(|d| touches(&c, d)) {
                Fs::apply(&mut self.durable, &c);
            } else {
                keep.push(c);
            }
        }
        self.pending = keep;
        if dir.is_none() {
            for i in self.inodes.values_mut() {
                if i.dirty {
                    i.durable = i.data.clone();
                    i.dirty = false;
                }
                i.entry_stale = false;
            }
        }
    }

    /// The stick as the power cut leaves it, `seed` choosing the fate of
    /// what wasn't on it yet: 0 loses all of it, 1 keeps all of it, others
    /// mix.
    fn crash(&self, seed: u64) -> Fs {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut pick = |n: u64| match seed {
            0 => 0,
            1 => n - 1,
            _ => rng.below(n),
        };
        let mut names = self.durable.clone();
        for c in &self.pending {
            match c {
                Change::Create(p, _) if pick(2) == 1 && names.get(parent(p)) == Some(&Node::Dir) => {
                    Fs::apply(&mut names, c)
                }
                Change::Remove(_) if pick(2) == 1 => Fs::apply(&mut names, c),
                Change::Rename(f, t) if names.contains_key(f) => match pick(3) {
                    // Only the old name; or both, sharing the clusters; or
                    // only the new.
                    0 => {}
                    1 => {
                        let n = names[f];
                        names.insert(t.clone(), n);
                    }
                    _ => Fs::apply(&mut names, c),
                },
                // The old name never reached the stick, but the rename's
                // new entry may have, pointing at its clusters.
                Change::Rename(f, t) if names.get(parent(t)) == Some(&Node::Dir) && pick(2) == 1 => {
                    if let Some(n) = self.names.get(t).filter(|_| !self.names.contains_key(f)) {
                        names.insert(t.clone(), *n);
                    }
                }
                _ => {}
            }
        }
        // Only what a name still uses: the rest is free space.
        let used: std::collections::HashSet<u64> =
            names.values().filter_map(|n| if let Node::File(i) = n { Some(*i) } else { None }).collect();
        let mut inodes = HashMap::new();
        for (i, ino) in self.inodes.iter().filter(|(i, _)| used.contains(i)) {
            let data = if ino.entry_stale {
                Vec::new()
            } else if !ino.dirty {
                ino.data.clone()
            } else {
                match pick(4) {
                    0 => ino.durable.clone(),
                    1 => vec![0xee; ino.data.len()],
                    2 => {
                        // Half of what was written after what was there.
                        let base = if ino.data.starts_with(&ino.durable) { ino.durable.len() } else { 0 };
                        ino.data[..base + (ino.data.len() - base) / 2].to_vec()
                    }
                    _ => ino.data.clone(),
                }
            };
            inodes.insert(*i, Inode { data: data.clone(), durable: data, dirty: false, entry_stale: false });
        }
        Fs { names: names.clone(), durable: names, pending: Vec::new(), inodes, next: self.next, ..Fs::default() }
    }
}

/// The model as a [`Disk`].
#[derive(Clone)]
struct Model(Arc<Mutex<Fs>>);

struct ModelOutput {
    fs: Arc<Mutex<Fs>>,
    ino: u64,
}

impl Output for ModelOutput {
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        let mut fs = self.fs.lock().unwrap();
        fs.alive()?;
        let i = fs.inodes.get_mut(&self.ino).expect("an inode");
        i.data.extend_from_slice(b);
        i.dirty = true;
        fs.tick();
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        let mut fs = self.fs.lock().unwrap();
        fs.alive()?;
        let i = fs.inodes.get_mut(&self.ino).expect("an inode");
        i.durable = i.data.clone();
        i.dirty = false;
        i.entry_stale = false;
        fs.tick();
        Ok(())
    }
}

fn s(p: &Path) -> String {
    p.to_str().expect("UTF-8").trim_end_matches('/').to_string()
}

impl Model {
    fn new(fs: Fs) -> Model {
        Model(Arc::new(Mutex::new(fs)))
    }

    fn fs(&self) -> std::sync::MutexGuard<'_, Fs> {
        self.0.lock().unwrap()
    }

    fn read(&self, p: &str) -> Option<Vec<u8>> {
        let fs = self.fs();
        fs.file(p).ok().map(|i| fs.inodes[&i].data.clone())
    }
}

impl Disk for Model {
    fn open(&self, p: &Path) -> io::Result<Box<dyn Read>> {
        let fs = self.fs();
        fs.alive()?;
        let i = fs.file(&s(p))?;
        Ok(Box::new(Cursor::new(fs.inodes[&i].data.clone())))
    }

    fn stat(&self, p: &Path) -> io::Result<Meta> {
        let fs = self.fs();
        fs.alive()?;
        match fs.names.get(&s(p)) {
            Some(Node::File(i)) => Ok(Meta { kind: Kind::File, len: fs.inodes[i].data.len() as u64 }),
            Some(Node::Dir) => Ok(Meta { kind: Kind::Dir, len: 0 }),
            None => Err(not_found(&s(p))),
        }
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<(OsString, Kind)>> {
        let fs = self.fs();
        fs.alive()?;
        let d = s(dir);
        if fs.names.get(&d) != Some(&Node::Dir) {
            return Err(not_found(&d));
        }
        Ok(fs
            .names
            .iter()
            .filter(|(p, _)| *p != &d && parent(p) == d)
            .map(|(p, n)| {
                let name = p.rsplit('/').next().unwrap_or_default();
                (OsString::from(name), if *n == Node::Dir { Kind::Dir } else { Kind::File })
            })
            .collect())
    }

    fn create(&self, p: &Path, _mode: u32) -> io::Result<Box<dyn Output>> {
        let mut fs = self.fs();
        fs.alive()?;
        let p = s(p);
        if fs.names.contains_key(&p) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, p));
        }
        fs.need_dir(&p)?;
        fs.next += 1;
        let ino = fs.next;
        fs.inodes.insert(ino, Inode::default());
        fs.names.insert(p.clone(), Node::File(ino));
        fs.pending.push(Change::Create(p, Node::File(ino)));
        fs.tick();
        Ok(Box::new(ModelOutput { fs: self.0.clone(), ino }))
    }

    fn append(&self, p: &Path) -> io::Result<Box<dyn Output>> {
        let fs = self.fs();
        fs.alive()?;
        let ino = fs.file(&s(p))?;
        Ok(Box::new(ModelOutput { fs: self.0.clone(), ino }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut fs = self.fs();
        fs.alive()?;
        let (f, t) = (s(from), s(to));
        let ino = fs.file(&f)?;
        fs.need_dir(&t)?;
        // What `to` was goes: its clusters are freed.
        fs.unlink(&t);
        fs.names.remove(&f);
        // If `from` and `to` shared clusters, `to` was just freed under it.
        fs.names.insert(t.clone(), Node::File(ino));
        fs.pending.push(Change::Rename(f, t));
        fs.inodes.get_mut(&ino).expect("an inode").entry_stale = true;
        fs.tick();
        Ok(())
    }

    fn remove_file(&self, p: &Path) -> io::Result<()> {
        let mut fs = self.fs();
        fs.alive()?;
        let p = s(p);
        fs.file(&p)?;
        fs.unlink(&p);
        fs.pending.push(Change::Remove(p));
        fs.tick();
        Ok(())
    }

    fn remove_dir_all(&self, p: &Path) -> io::Result<()> {
        let mut fs = self.fs();
        fs.alive()?;
        let p = s(p);
        if fs.names.get(&p) != Some(&Node::Dir) {
            return Err(not_found(&p));
        }
        let inside: Vec<String> = fs.names.keys().filter(|q| under(q, &p)).cloned().collect();
        for q in inside {
            fs.unlink(&q);
            fs.names.remove(&q);
        }
        fs.pending.push(Change::Remove(p));
        fs.tick();
        Ok(())
    }

    fn create_dir(&self, p: &Path) -> io::Result<()> {
        let mut fs = self.fs();
        fs.alive()?;
        let p = s(p);
        if fs.names.contains_key(&p) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, p));
        }
        fs.need_dir(&p)?;
        fs.names.insert(p.clone(), Node::Dir);
        fs.pending.push(Change::Create(p, Node::Dir));
        fs.tick();
        Ok(())
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        let mut fs = self.fs();
        fs.alive()?;
        fs.settle(Some(&s(dir)));
        fs.tick();
        Ok(())
    }

    fn sync(&self) {
        let mut fs = self.fs();
        if fs.alive().is_ok() {
            fs.settle(None);
            fs.tick();
        }
    }

    fn available(&self, _p: &Path) -> io::Result<u64> {
        self.fs().alive()?;
        Ok(1 << 40)
    }

    fn mounted(&self, _mount: &Path) -> bool {
        true
    }
}

// The boot.

const SETS: &str = "/stick/neochumby";

/// Whether the hook would take set `set`, if it came to it.
fn bootable(disk: &Model, set: Set) -> bool {
    let n = set.name();
    let Some(marker) = disk.read(&format!("{SETS}/{n}.complete")) else { return false };
    if disk.read(&format!("{SETS}/{n}/neochumby")).is_none() {
        return false;
    }
    let text = String::from_utf8_lossy(&marker).into_owned();
    // sed's lines: a final newline ends the last line rather than starting
    // another.
    let lines: Vec<&str> = text.strip_suffix('\n').unwrap_or(&text).split('\n').collect();
    let last_ok = lines.last().is_some_and(|l| {
        let f: Vec<&str> = l.split(' ').collect();
        f.len() == 3
            && f[0] == "end"
            && !f[1].is_empty()
            && f[1].bytes().all(|b| b.is_ascii_digit())
            && f[2].len() == 64
            && f[2].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    });
    lines[0] == HEADER && last_ok && check_set_on(disk, Path::new(&format!("{SETS}/{n}")), &LIMITS).is_ok()
}

/// Whether every file of set `set` holds the bytes its marker says.
fn true_to_marker(disk: &Model, set: Set) -> bool {
    let dir = format!("{SETS}/{}", set.name());
    let Ok(m) = check_set_on(disk, Path::new(&dir), &LIMITS) else { return false };
    m.iter().all(|e| {
        disk.read(&format!("{dir}/{}", e.path)).is_some_and(|d| {
            let h = ring::digest::digest(&ring::digest::SHA256, &d);
            h.as_ref() == e.sha256 && d.len() as u64 == e.size
        })
    })
}

/// The set boot/userhook2 runs, as it decides: `current`'s first line if it
/// is `a` or `b` (else `current.new`'s), that set first, then the other;
/// a set runs if its marker is a file, its session is a file, the marker's
/// first line is the header and its last is `end N HEX`, and check-set
/// takes it.
fn hook_choice(disk: &Model) -> Option<Set> {
    let first_line = |p: &str| {
        let b = disk.read(p)?;
        let text = String::from_utf8_lossy(&b).into_owned();
        Set::parse(text.split('\n').next()?)
    };
    let cur = first_line(&format!("{SETS}/current")).or_else(|| first_line(&format!("{SETS}/current.new")));
    let order = if cur == Some(Set::B) { [Set::B, Set::A] } else { [Set::A, Set::B] };
    order.into_iter().find(|set| bootable(disk, *set))
}

type Content = BTreeMap<String, Vec<u8>>;

/// What the set `set` holds, by its marker.
fn content(disk: &Model, set: Set) -> Content {
    let dir = format!("{SETS}/{}", set.name());
    let m = check_set_on(disk, Path::new(&dir), &LIMITS).expect("a complete set");
    m.iter().map(|e| (e.path.clone(), disk.read(&format!("{dir}/{}", e.path)).expect("listed"))).collect()
}

// Rounds, over channels.

struct Chan {
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
}

impl Link for Chan {
    fn send(&mut self, m: &Msg) -> io::Result<()> {
        self.tx.send(m.clone()).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn recv_timeout(&mut self, t: Duration) -> io::Result<Option<Msg>> {
        match self.rx.recv_timeout(t) {
            Ok(m) => Ok(Some(m)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::ErrorKind::UnexpectedEof.into()),
        }
    }
}

const T: Timing = Timing { step: Duration::from_secs(10), round: Duration::from_secs(30) };

/// One round against neochumbyd's side serving `tree`.
fn round(stick: &Stick, tree: &Tree) -> io::Result<Outcome> {
    let (a, b) = (mpsc::channel(), mpsc::channel());
    let mut chumby = Chan { tx: a.0, rx: b.1 };
    let mut server = Chan { tx: b.0, rx: a.1 };
    // Scanned once: the trees don't change.
    let scanned = tree.scanned.clone();
    let t = std::thread::spawn(move || {
        while let Ok(Some(Msg::SyncHave { live })) = server.recv_timeout(T.step) {
            if serve::round(&mut server, live, &scanned).is_err() {
                break;
            }
        }
    });
    let r = receive::round(&mut chumby, stick, T);
    drop(chumby);
    t.join().expect("the server");
    r
}

fn stick(m: &Model) -> Stick {
    let mut st = Stick::new(SETS, None);
    st.disk = Arc::new(m.clone());
    // Small batches, so a few files make several.
    st.batch = Batch { files: 3, bytes: 4 << 20 };
    st
}

// Scenarios.

struct Tree {
    scanned: Arc<serve::Tree>,
    content: Content,
}

fn tree(root: &Path, name: &str, files: &[(&str, Vec<u8>)]) -> Tree {
    let dir = root.join(name);
    let _ = std::fs::remove_dir_all(&dir);
    for (p, d) in files {
        let f = dir.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, d).unwrap();
    }
    let scanned = Arc::new(serve::scan(&dir, &Hashes::default()).expect("the tree"));
    Tree { scanned, content: files.iter().map(|(p, d)| (p.to_string(), d.clone())).collect() }
}

fn bytes(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

/// What a scenario does once the stick is ready, and is cut short in.
#[derive(Clone, Copy)]
enum Act<'a> {
    Sync(&'a Tree),
    Rollback,
}

fn act(m: &Model, a: Act) -> io::Result<()> {
    match a {
        Act::Sync(t) => round(&stick(m), t).map(|_| ()),
        Act::Rollback => stick(m).rollback().map(|_| ()),
    }
}

/// A stick made ready by `steps` (with no cut), synced clean.
fn ready(steps: &[Act]) -> Fs {
    let m = Model::new(Fs::empty());
    for s in steps {
        act(&m, *s).expect("a clean step");
    }
    let mut fs = m.fs().clone();
    fs.settle(None);
    fs.ops = 0;
    fs
}

/// Cuts the power after every operation of `cut` on `start`, each with a
/// few fates for what wasn't on the stick yet; boots; syncs to `goal`.
/// `may_boot` are the trees the boot may run. How many cuts it made.
fn every_cut(name: &str, start: &Fs, cut: Act, may_boot: &[&Tree], goal: &Tree, seeds: u64) -> usize {
    let whole = {
        let m = Model::new(start.clone());
        act(&m, cut).expect("a clean run");
        m.fs().ops
    };
    let booted_before = hook_choice(&Model::new(start.clone()));
    let mut cuts = 0;
    for n in 1..=whole {
        for seed in 0..seeds {
            let mut fs = start.clone();
            fs.cut_after = Some(n);
            let m = Model::new(fs);
            let finished = act(&m, cut).is_ok();
            let after = Model::new(m.fs().crash(seed));
            let at = format!("{name}: cut after operation {n} of {whole}, seed {seed}");

            // A round that said its set is live, cut the moment after: the
            // boot runs that set.
            if let (true, Act::Sync(t)) = (finished, cut) {
                let set = hook_choice(&after).unwrap_or_else(|| panic!("{at}: nothing to boot after a round"));
                assert_eq!(content(&after, set), t.content, "{at}: the round's set isn't what boots");
            }

            // The boot: a complete set of a tree it may run, and the same
            // set the receiver takes for live.
            match hook_choice(&after) {
                Some(set) => {
                    let got = content(&after, set);
                    assert!(
                        may_boot.iter().any(|t| t.content == got),
                        "{at}: the boot runs set {} holding {:?}",
                        set.name(),
                        got.keys().collect::<Vec<_>>()
                    );
                }
                None => assert!(booted_before.is_none(), "{at}: no set to boot, though one was"),
            }
            assert_eq!(stick(&after).live().map(|l| l.0), hook_choice(&after), "{at}: the receiver and the hook");
            // Any set the hook would take, were the other to fail, holds
            // what its marker says.
            for set in [Set::A, Set::B] {
                assert!(
                    !bootable(&after, set) || true_to_marker(&after, set),
                    "{at}: set {} passes the boot's checks but isn't its marker's bytes",
                    set.name()
                );
            }

            // Then rounds bring it to the goal.
            let st = stick(&after);
            let _ = st.resume();
            let mut done = false;
            for _ in 0..4 {
                if matches!(round(&st, goal), Ok(Outcome::UpToDate)) {
                    done = true;
                    break;
                }
            }
            assert!(done, "{at}: rounds never reach the tree");
            let live = hook_choice(&after).unwrap_or_else(|| panic!("{at}: nothing to boot after syncing"));
            assert_eq!(content(&after, live), goal.content, "{at}: booted after syncing");
            cuts += 1;
        }
    }
    cuts
}

#[test]
fn power_cuts_at_every_point() {
    let root = std::env::temp_dir().join(format!("neochumby-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let hook = b"#!/bin/sh\n".to_vec();
    let t1 = tree(
        &root,
        "t1",
        &[
            ("neochumby", bytes(3000, 1)),
            ("userhook2", hook.clone()),
            ("apps/home/manifest", b"title=HOME\n".to_vec()),
            ("apps/doom/doom1.wad", bytes(70_000, 2)),
            ("apps/doom/manifest", b"title=DOOM\n".to_vec()),
        ],
    );
    // A delta: one file changed, one added, one gone.
    let t2 = tree(
        &root,
        "t2",
        &[
            ("neochumby", bytes(3000, 9)),
            ("userhook2", hook.clone()),
            ("apps/home/manifest", b"title=HOME\n".to_vec()),
            ("apps/doom/doom1.wad", bytes(70_000, 2)),
            ("apps/wx/forecast.raw", bytes(5000, 3)),
        ],
    );
    // Removals: a whole app's directory goes, and a file where a directory
    // was.
    let t3 = tree(
        &root,
        "t3",
        &[("neochumby", bytes(3000, 9)), ("userhook2", hook.clone()), ("apps", b"not a directory now".to_vec())],
    );
    // The same files at the same sizes, new bytes: what a renderer's
    // pictures do on every run.
    let t5 = tree(
        &root,
        "t5",
        &[
            ("neochumby", bytes(3000, 4)),
            ("userhook2", hook.clone()),
            ("apps/home/manifest", b"title=HOME\n".to_vec()),
            ("apps/doom/doom1.wad", bytes(70_000, 6)),
            ("apps/doom/manifest", b"title=DOOM\n".to_vec()),
        ],
    );
    // Several batches' worth of files (a batch is 3 here).
    let many: Vec<(String, Vec<u8>)> = (0..10).map(|i| (format!("apps/many/f{i:02}"), bytes(10, i as u8))).collect();
    let mut files: Vec<(&str, Vec<u8>)> = many.iter().map(|(p, d)| (p.as_str(), d.clone())).collect();
    files.push(("neochumby", bytes(3000, 5)));
    let t4 = tree(&root, "t4", &files);

    let (s1, s2, s3, s4, s5) = (Act::Sync(&t1), Act::Sync(&t2), Act::Sync(&t3), Act::Sync(&t4), Act::Sync(&t5));
    let mut cuts = 0;
    cuts += every_cut("first sync", &ready(&[]), s1, &[&t1], &t1, 5);
    cuts += every_cut("delta", &ready(&[s1]), s2, &[&t1, &t2], &t2, 5);
    cuts += every_cut("removals", &ready(&[s1, s2]), s3, &[&t2, &t3], &t3, 5);
    // The standby holds t1, and t5 rewrites its files in place.
    cuts += every_cut("same sizes", &ready(&[s1, s2]), s5, &[&t2, &t5], &t5, 5);
    cuts += every_cut("batches", &ready(&[s1]), s4, &[&t1, &t4], &t4, 5);
    // Rollback from t2 to t1; after it, rounds bring t2 back.
    cuts += every_cut("rollback", &ready(&[s1, s2]), Act::Rollback, &[&t1, &t2], &t2, 5);
    // A flip only: the standby is complete with the tree already.
    let flip = {
        let fs = ready(&[s1, s2, Act::Rollback]);
        let m = Model::new(fs);
        stick(&m).resume().unwrap();
        let mut fs = m.fs().clone();
        fs.settle(None);
        fs.ops = 0;
        fs
    };
    cuts += every_cut("flip only", &flip, s2, &[&t1, &t2], &t2, 5);
    // A round cut short (here, a third of the way, the mixed fates of
    // seed 7), then cut again at every point of the round that resumes it.
    let resumed = {
        let start = ready(&[s1]);
        let whole = {
            let m = Model::new(start.clone());
            act(&m, s4).unwrap();
            m.fs().ops
        };
        let mut fs = start;
        fs.cut_after = Some(whole / 3);
        let m = Model::new(fs);
        let _ = act(&m, s4);
        let mut fs = m.fs().crash(7);
        fs.ops = 0;
        fs
    };
    cuts += every_cut("resume", &resumed, s4, &[&t1, &t4], &t4, 5);
    // A cut on a cut: the delta round cut a third of the way and at each
    // of its last three operations (around the flip), with mixed fates;
    // then every point of the round after it.
    let base = ready(&[s1]);
    let whole = {
        let m = Model::new(base.clone());
        act(&m, s2).unwrap();
        m.fs().ops
    };
    for first in [whole / 3, whole - 2, whole - 1, whole] {
        for seed in 2..7 {
            let mut fs = base.clone();
            fs.cut_after = Some(first);
            let m = Model::new(fs);
            let _ = act(&m, s2);
            let mut fs = m.fs().crash(seed);
            fs.ops = 0;
            let name = format!("cut on a cut (first at {first}, seed {seed})");
            cuts += every_cut(&name, &fs, s2, &[&t1, &t2], &t2, 3);
        }
    }
    eprintln!("{cuts} power cuts, each booted and synced again");
    std::fs::remove_dir_all(&root).unwrap();
}
