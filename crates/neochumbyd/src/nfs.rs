//! The app tree over NFSv3, read-only (nfsserve, one TCP port for NFS and
//! MOUNT).
//!
//! - File ids are 32 bits, from a hash of the path and inode: the Chumby's
//!   32-bit userland cannot stat larger ones. They hold across restarts,
//!   and a file replaced in the tree gets a new one, so a Chumby holding
//!   the old file's handle is told it is stale instead of reading the new
//!   file's bytes as the old's.
//! - Handles carry a fixed generation, so they survive restarts too.
//! - Read-only by refusing every write; the capability reported is
//!   read-write all the same, as only then does nfsserve grant execute in
//!   ACCESS, which the Chumby's kernel asks for before running a binary.
//! - Every file is served as root's, without setuid or setgid bits
//!   ([`attr`]); the Chumby mounts the tree `nosuid,nodev` as well.
//! - Symbolic links are never followed here: a link is served as a link,
//!   for the client to resolve, so nothing outside the tree is reachable.
//!   That holds for every component of a path, not only the last: each
//!   path is walked from the tree's root one directory at a time, none of
//!   them opened through a link ([`Beneath`]), so a directory swapped for
//!   a link while it is being served leads nowhere.
//! - The tree's manifest, signed with neochumbyd's key, is served as
//!   `.neochumby-manifest` in the root (neochumby_sync::signed), made from
//!   the tree as it is when asked for, at most a second old. It is no file
//!   of the tree, and isn't listed. A Chumby checks what it mounted against
//!   it before it runs anything.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nfsserve::nfs::{fattr3, fileid3, filename3, ftype3, nfs_fh3, nfspath3, nfsstat3, nfstime3, sattr3, specdata3};
use nfsserve::vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, Stat};

const ROOT: fileid3 = 1;
/// The signed manifest's id.
const MANIFEST: fileid3 = 2;
/// How long a manifest made is served before the tree is looked at again.
const MANIFEST_FRESH: Duration = Duration::from_secs(1);
/// In every handle: "neochumb".
const GENERATION: u64 = 0x6e65_6f63_6875_6d62;

/// What signs the manifest: neochumbyd's identity, or nothing (a Noise
/// build's key can't sign).
pub type Signer = Box<dyn Fn(&[u8]) -> Option<[u8; 64]> + Send + Sync>;

pub struct TreeFs {
    root: Beneath,
    /// id → path relative to the root.
    paths: RwLock<HashMap<fileid3, PathBuf>>,
    signer: Signer,
    hashes: neochumby_sync::serve::Hashes,
    manifest: Mutex<Option<Made>>,
}

/// The manifest last made: when, its text, and since when it has been that
/// text (its mtime as served).
struct Made {
    at: Instant,
    text: Arc<Vec<u8>>,
    since: SystemTime,
}

/// The tree's root, and the paths beneath it, each walked from the root a
/// directory at a time with `O_NOFOLLOW`: a link anywhere in a path is
/// never followed, whatever it was when the path was first looked up.
/// The root itself is opened by its path each time, so a tree replaced
/// whole is served from then on.
struct Beneath(PathBuf);

impl Beneath {
    /// The directory `rel` names, opened.
    fn dir(&self, rel: &Path) -> io::Result<OwnedFd> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let mut fd = rustix::fs::openat(CWD, &self.0, flags, Mode::empty())?;
        for c in rel.components() {
            let Component::Normal(name) = c else {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a plain path"));
            };
            fd = rustix::fs::openat(&fd, name, flags | OFlags::NOFOLLOW, Mode::empty())?;
        }
        Ok(fd)
    }

    /// `rel`'s directory, opened, and its last component; None for the root.
    fn parent<'a>(&self, rel: &'a Path) -> io::Result<(OwnedFd, Option<&'a OsStr>)> {
        match rel.file_name() {
            None => Ok((self.dir(rel)?, None)),
            Some(name) => Ok((self.dir(rel.parent().unwrap_or(Path::new("")))?, Some(name))),
        }
    }

    /// `rel`'s status; a link's own, not its target's.
    fn stat(&self, rel: &Path) -> io::Result<Stat> {
        match self.parent(rel)? {
            (fd, None) => Ok(rustix::fs::fstat(&fd)?),
            (fd, Some(name)) => Ok(rustix::fs::statat(&fd, name, AtFlags::SYMLINK_NOFOLLOW)?),
        }
    }

    /// `rel`, opened to read, if it is not a link.
    fn open(&self, rel: &Path) -> io::Result<File> {
        let (fd, Some(name)) = self.parent(rel)? else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the root is not a file"));
        };
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        Ok(File::from(rustix::fs::openat(&fd, name, flags, Mode::empty())?))
    }

    /// The names in directory `rel`, without `.` and `..`.
    fn names(&self, rel: &Path) -> io::Result<Vec<OsString>> {
        let mut out = Vec::new();
        for e in rustix::fs::Dir::new(self.dir(rel)?)? {
            let name = e?.file_name().to_bytes().to_vec();
            if name != b"." && name != b".." {
                out.push(OsString::from_vec(name));
            }
        }
        Ok(out)
    }

    /// Where the link `rel` points.
    fn read_link(&self, rel: &Path) -> io::Result<Vec<u8>> {
        let (fd, Some(name)) = self.parent(rel)? else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the root is not a link"));
        };
        Ok(rustix::fs::readlinkat(&fd, name, Vec::new())?.into_bytes())
    }
}

/// `st`'s file type.
fn kind(st: &Stat) -> FileType {
    FileType::from_raw_mode(st.st_mode as _)
}

fn is_dir(st: &Stat) -> bool {
    kind(st) == FileType::Directory
}

fn is_file(st: &Stat) -> bool {
    kind(st) == FileType::RegularFile
}

fn is_symlink(st: &Stat) -> bool {
    kind(st) == FileType::Symlink
}

/// FNV-1a over the path and inode, folded to 32 bits, clear of 0 (reserved),
/// 1 (the root) and 2 (the manifest).
#[allow(clippy::unnecessary_cast)] // st_ino's type differs by system.
fn id_for(rel: &Path, meta: &Stat) -> fileid3 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in rel.as_os_str().as_bytes().iter().chain(&[0]).chain(&(meta.st_ino as u64).to_le_bytes()) {
        h = (h ^ *b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let id = (h ^ (h >> 32)) & 0xffff_ffff;
    if id <= MANIFEST { id + MANIFEST + 1 } else { id }
}

/// A file's attributes as served: owned by root, with no setuid or setgid
/// bit and no device number, whatever the tree holds. A file in an app's
/// directory can't hand out another uid on the Chumby, nor stand for one
/// of its devices, and no app's uid can match the tree's owner here.
/// Anything not a directory or a link is served as a plain file, as
/// nfsserve's own conversion does.
#[allow(clippy::unnecessary_cast)] // Stat's field types differ by system.
fn attr(id: fileid3, meta: &Stat) -> fattr3 {
    let (ftype, nlink) = if is_dir(meta) {
        (ftype3::NF3DIR, 2)
    } else if is_symlink(meta) {
        (ftype3::NF3LNK, 1)
    } else {
        (ftype3::NF3REG, 1)
    };
    let time = |s: i64, ns: i64| nfstime3 { seconds: s as u32, nseconds: ns as u32 };
    let size = meta.st_size as u64;
    fattr3 {
        ftype,
        mode: (meta.st_mode as u32) & 0o1777,
        nlink,
        uid: 0,
        gid: 0,
        size,
        used: size,
        rdev: specdata3::default(),
        fsid: 0,
        fileid: id,
        atime: time(meta.st_atime as i64, meta.st_atime_nsec as i64),
        mtime: time(meta.st_mtime as i64, meta.st_mtime_nsec as i64),
        ctime: time(meta.st_ctime as i64, meta.st_ctime_nsec as i64),
    }
}

impl TreeFs {
    pub fn new(root: PathBuf, signer: Signer) -> TreeFs {
        let paths = HashMap::from([(ROOT, PathBuf::new())]);
        TreeFs {
            root: Beneath(root),
            paths: RwLock::new(paths),
            signer,
            hashes: Default::default(),
            manifest: Mutex::new(None),
        }
    }

    /// The tree's signed manifest, and since when it has been what it is;
    /// None if it can't be made (a tree a Chumby would refuse, or no key
    /// that signs).
    fn manifest(&self) -> Option<(Arc<Vec<u8>>, SystemTime)> {
        let mut m = self.manifest.lock().unwrap();
        if let Some(made) = &*m
            && made.at.elapsed() < MANIFEST_FRESH
        {
            return Some((made.text.clone(), made.since));
        }
        let text = match self.sign() {
            Ok(t) => Arc::new(t),
            Err(e) => {
                eprintln!("neochumbyd: the tree's signed manifest: {e}");
                *m = None;
                return None;
            }
        };
        let since = match &*m {
            Some(old) if old.text == text => old.since,
            _ => SystemTime::now(),
        };
        *m = Some(Made { at: Instant::now(), text: text.clone(), since });
        Some((text, since))
    }

    /// The tree's manifest as it is now, signed.
    fn sign(&self) -> io::Result<Vec<u8>> {
        let tree = neochumby_sync::serve::scan(&self.root.0, &self.hashes)?;
        let entries = tree.manifest();
        neochumby_sync::manifest::validate(&entries, &neochumby_sync::LIMITS)?;
        let mut sig = None;
        let text = neochumby_sync::signed::sign(&entries, |m| {
            sig = (self.signer)(m);
            sig.unwrap_or([0; 64])
        });
        sig.ok_or_else(|| io::Error::other("this build's key can't sign"))?;
        Ok(text.into_bytes())
    }

    /// The manifest's attributes: a plain file, root's, read-only.
    fn manifest_attr(text: &[u8], since: SystemTime) -> fattr3 {
        let t = since.duration_since(UNIX_EPOCH).unwrap_or_default();
        let time = nfstime3 { seconds: t.as_secs() as u32, nseconds: t.subsec_nanos() };
        fattr3 {
            ftype: ftype3::NF3REG,
            mode: 0o444,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: text.len() as u64,
            used: text.len() as u64,
            rdev: specdata3::default(),
            fsid: 0,
            fileid: MANIFEST,
            atime: time,
            mtime: time,
            ctime: time,
        }
    }

    /// The relative path of `id`, if it is still the file it was.
    fn path(&self, id: fileid3) -> Result<PathBuf, nfsstat3> {
        if let Some(p) = self.paths.read().unwrap().get(&id) {
            return Ok(p.clone());
        }
        // A handle from before a restart: index the tree and look again.
        self.index(Path::new(""));
        self.paths.read().unwrap().get(&id).cloned().ok_or(nfsstat3::NFS3ERR_STALE)
    }

    /// Metadata of `id`'s file, which must still be the one `id` names.
    fn meta(&self, id: fileid3) -> Result<(PathBuf, Stat), nfsstat3> {
        let rel = self.path(id)?;
        let meta = self.root.stat(&rel).map_err(|_| nfsstat3::NFS3ERR_STALE)?;
        if id != ROOT && id_for(&rel, &meta) != id {
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        Ok((rel, meta))
    }

    fn remember(&self, rel: &Path, meta: &Stat) -> Result<fileid3, nfsstat3> {
        let id = id_for(rel, meta);
        let mut paths = self.paths.write().unwrap();
        match paths.get(&id) {
            Some(p) if p != rel => {
                eprintln!("neochumbyd: file id collision: {} and {}", p.display(), rel.display());
                Err(nfsstat3::NFS3ERR_IO)
            }
            _ => {
                paths.insert(id, rel.to_path_buf());
                Ok(id)
            }
        }
    }

    fn index(&self, dir: &Path) {
        let Ok(names) = self.root.names(dir) else { return };
        for name in names {
            let rel = dir.join(name);
            if let Ok(meta) = self.root.stat(&rel) {
                let _ = self.remember(&rel, &meta);
                if is_dir(&meta) {
                    self.index(&rel);
                }
            }
        }
    }

    /// A directory's entries, sorted by name, with their ids and metadata.
    fn entries(&self, dirid: fileid3) -> Result<Vec<(fileid3, filename3, Stat)>, nfsstat3> {
        let (rel, meta) = self.meta(dirid)?;
        if !is_dir(&meta) {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let mut names = self.root.names(&rel).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        names.sort();
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let child = rel.join(&name);
            let Ok(meta) = self.root.stat(&child) else { continue };
            let id = self.remember(&child, &meta)?;
            out.push((id, filename3::from(name.as_bytes()), meta));
        }
        Ok(out)
    }
}

#[async_trait]
impl NFSFileSystem for TreeFs {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadWrite
    }

    fn root_dir(&self) -> fileid3 {
        ROOT
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let (rel, meta) = self.meta(dirid)?;
        if !is_dir(&meta) {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let name = filename.as_ref();
        match name {
            b"." => return Ok(dirid),
            b".." => {
                let parent = rel.parent().map(Path::to_path_buf).unwrap_or_default();
                if parent.as_os_str().is_empty() {
                    return Ok(ROOT);
                }
                let meta = self.root.stat(&parent).map_err(|_| nfsstat3::NFS3ERR_STALE)?;
                return self.remember(&parent, &meta);
            }
            _ if name.is_empty() || name.contains(&b'/') || name.contains(&0) => return Err(nfsstat3::NFS3ERR_NOENT),
            _ if dirid == ROOT && name == neochumby_sync::signed::FILE.as_bytes() => {
                return self.manifest().map(|_| MANIFEST).ok_or(nfsstat3::NFS3ERR_NOENT);
            }
            _ => {}
        }
        let child = rel.join(OsStr::from_bytes(name));
        let meta = self.root.stat(&child).map_err(|_| nfsstat3::NFS3ERR_NOENT)?;
        self.remember(&child, &meta)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        if id == MANIFEST {
            let (text, since) = self.manifest().ok_or(nfsstat3::NFS3ERR_STALE)?;
            return Ok(Self::manifest_attr(&text, since));
        }
        let (_, meta) = self.meta(id)?;
        Ok(attr(id, &meta))
    }

    async fn read(&self, id: fileid3, offset: u64, count: u32) -> Result<(Vec<u8>, bool), nfsstat3> {
        if id == MANIFEST {
            let (text, _) = self.manifest().ok_or(nfsstat3::NFS3ERR_STALE)?;
            let from = (offset as usize).min(text.len());
            let to = from.saturating_add(count as usize).min(text.len());
            return Ok((text[from..to].to_vec(), to == text.len()));
        }
        let (rel, meta) = self.meta(id)?;
        if !is_file(&meta) {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        let mut f = self.root.open(&rel).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        // The file opened must be the one checked, not one put in its place
        // since.
        let opened = rustix::fs::fstat(&f).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        if (opened.st_dev, opened.st_ino) != (meta.st_dev, meta.st_ino) || !is_file(&opened) {
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        f.seek(SeekFrom::Start(offset)).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        let mut buf = Vec::with_capacity(count as usize);
        f.take(count as u64).read_to_end(&mut buf).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        #[allow(clippy::unnecessary_cast)] // st_size's type differs by system.
        let eof = offset + buf.len() as u64 >= opened.st_size as u64;
        Ok((buf, eof))
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let all = self.entries(dirid)?;
        let skip = match start_after {
            0 => 0,
            after => all.iter().position(|(id, ..)| *id == after).map(|i| i + 1).ok_or(nfsstat3::NFS3ERR_BAD_COOKIE)?,
        };
        let rest = &all[skip..];
        let take = rest.len().min(max_entries);
        Ok(ReadDirResult {
            entries: rest[..take]
                .iter()
                .map(|(id, name, meta)| DirEntry { fileid: *id, name: name.clone(), attr: attr(*id, meta) })
                .collect(),
            end: take == rest.len(),
        })
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        let (rel, meta) = self.meta(id)?;
        if !is_symlink(&meta) {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        let target = self.root.read_link(&rel).map_err(|_| nfsstat3::NFS3ERR_IO)?;
        Ok(nfspath3::from(target))
    }

    fn id_to_fh(&self, id: fileid3) -> nfs_fh3 {
        let mut data = GENERATION.to_le_bytes().to_vec();
        data.extend_from_slice(&id.to_le_bytes());
        nfs_fh3 { data }
    }

    fn fh_to_id(&self, fh: &nfs_fh3) -> Result<fileid3, nfsstat3> {
        let (generation, id) = fh.data.split_at_checked(8).ok_or(nfsstat3::NFS3ERR_BADHANDLE)?;
        if id.len() != 8 {
            return Err(nfsstat3::NFS3ERR_BADHANDLE);
        }
        if generation != GENERATION.to_le_bytes() {
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        Ok(u64::from_le_bytes(id.try_into().expect("8 bytes")))
    }

    async fn setattr(&self, _: fileid3, _: sattr3) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn write(&self, _: fileid3, _: u64, _: &[u8]) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create(&self, _: fileid3, _: &filename3, _: sattr3) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create_exclusive(&self, _: fileid3, _: &filename3) -> Result<fileid3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn mkdir(&self, _: fileid3, _: &filename3) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn remove(&self, _: fileid3, _: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn rename(&self, _: fileid3, _: &filename3, _: fileid3, _: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn symlink(
        &self,
        _: fileid3,
        _: &filename3,
        _: &nfspath3,
        _: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn tree() -> (PathBuf, TreeFs) {
        let root = std::env::temp_dir().join(format!("neochumbyd-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("apps/doom")).unwrap();
        std::fs::write(root.join("neochumby"), b"launcher").unwrap();
        std::fs::write(root.join("apps/doom/manifest"), b"title=DOOM\n").unwrap();
        std::os::unix::fs::symlink("/etc", root.join("apps/escape")).unwrap();
        (root.clone(), TreeFs::new(root, no_key()))
    }

    fn no_key() -> Signer {
        Box::new(|_| None)
    }

    #[tokio::test]
    async fn serves_reads_refuses_writes() {
        let (root, fs) = tree();
        let apps = fs.lookup(ROOT, &b"apps".as_slice().into()).await.unwrap();
        let doom = fs.lookup(apps, &b"doom".as_slice().into()).await.unwrap();
        let m = fs.lookup(doom, &b"manifest".as_slice().into()).await.unwrap();
        assert!(m < 1 << 32);
        assert_eq!(fs.read(m, 0, 100).await.unwrap(), (b"title=DOOM\n".to_vec(), true));
        assert_eq!(fs.read(m, 6, 4).await.unwrap(), (b"DOOM".to_vec(), false));
        assert_eq!(fs.lookup(doom, &b"..".as_slice().into()).await.unwrap(), apps);

        // The link is a link: not followed to /etc.
        let esc = fs.lookup(apps, &b"escape".as_slice().into()).await.unwrap();
        assert_eq!(fs.readlink(esc).await.unwrap().as_ref(), b"/etc");
        assert!(fs.lookup(esc, &b"passwd".as_slice().into()).await.is_err());
        assert!(fs.read(esc, 0, 10).await.is_err());

        assert!(matches!(fs.write(m, 0, b"x").await, Err(nfsstat3::NFS3ERR_ROFS)));

        // No setuid or setgid bit, and root's.
        std::fs::set_permissions(root.join("apps/doom/manifest"), std::fs::Permissions::from_mode(0o6755)).unwrap();
        let a = fs.getattr(m).await.unwrap();
        assert_eq!((a.mode & 0o7777, a.uid, a.gid), (0o755, 0, 0));
        let names: Vec<_> = fs.readdir(apps, 0, 10).await.unwrap().entries.into_iter().map(|e| e.name.0).collect();
        assert_eq!(names, [b"doom".to_vec(), b"escape".to_vec()]);

        // A restarted server knows the handle; a replaced file's is stale.
        let fresh = TreeFs::new(root.clone(), no_key());
        let fh = fs.id_to_fh(m);
        assert_eq!(fresh.getattr(fresh.fh_to_id(&fh).unwrap()).await.unwrap().fileid, m);
        std::fs::write(root.join("apps/doom/manifest.new"), b"title=DOOM2\n").unwrap();
        std::fs::rename(root.join("apps/doom/manifest.new"), root.join("apps/doom/manifest")).unwrap();
        assert!(matches!(fs.getattr(m).await, Err(nfsstat3::NFS3ERR_STALE)));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A directory in a path that is a link, as one swapped for a link
    /// while it is served would be, is never followed: not to stat, open,
    /// list or read a link beyond it.
    #[test]
    fn never_through_a_link() {
        let base = std::env::temp_dir().join(format!("neochumbyd-beneath-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, outside) = (base.join("tree"), base.join("state"));
        std::fs::create_dir_all(root.join("apps")).unwrap();
        std::fs::create_dir_all(outside.join("sub")).unwrap();
        std::fs::write(outside.join("identity"), b"secret").unwrap();
        std::fs::write(outside.join("sub/identity"), b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("apps/x")).unwrap();
        let b = Beneath(root.clone());
        for rel in ["apps/x/identity", "apps/x/sub/identity", "apps/x/sub"] {
            let rel = Path::new(rel);
            assert!(b.stat(rel).is_err(), "{}", rel.display());
            assert!(b.open(rel).is_err(), "{}", rel.display());
            assert!(b.names(rel).is_err(), "{}", rel.display());
            assert!(b.read_link(rel).is_err(), "{}", rel.display());
        }
        // The link itself is there, as a link, and can't be opened; the
        // root is reachable; and `..` is no way out.
        assert!(is_symlink(&b.stat(Path::new("apps/x")).unwrap()));
        assert!(b.open(Path::new("apps/x")).is_err());
        assert_eq!(b.names(Path::new("")).unwrap(), [OsString::from("apps")]);
        assert!(b.stat(Path::new("../state/identity")).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }

    /// The signed manifest is served in the root, made from the tree as it
    /// is, and checks with neochumbyd's key; a file changed in the tree
    /// changes it.
    #[tokio::test]
    async fn serves_the_signed_manifest() {
        let root = std::env::temp_dir().join(format!("neochumbyd-signed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("apps/home")).unwrap();
        std::fs::write(root.join("neochumby"), b"the session").unwrap();
        std::fs::write(root.join("apps/home/manifest"), b"exec=neochumby-home\n").unwrap();
        let id = Arc::new(neochumby_proto::Identity::generate().unwrap());
        let key = id.public();
        let fs = TreeFs::new(root.clone(), Box::new(move |m| id.sign(m)));
        let name = neochumby_sync::signed::FILE.as_bytes();

        let m = fs.lookup(ROOT, &name.into()).await.unwrap();
        assert_eq!(m, MANIFEST);
        let a = fs.getattr(m).await.unwrap();
        let (text, eof) = fs.read(m, 0, a.size as u32 + 10).await.unwrap();
        assert!(eof && text.len() as u64 == a.size && a.mode == 0o444);
        let v = neochumby_sync::signed::open(&text, &key).unwrap();
        assert_eq!(v.read(&root, "neochumby").unwrap(), b"the session");
        // Read in pieces, as a client does: the same bytes.
        let (head, eof) = fs.read(m, 0, 10).await.unwrap();
        assert!(!eof && head == text[..10]);
        // Not listed, and nowhere but the root.
        let names: Vec<_> = fs.readdir(ROOT, 0, 10).await.unwrap().entries.into_iter().map(|e| e.name.0).collect();
        assert!(!names.contains(&name.to_vec()));
        let apps = fs.lookup(ROOT, &b"apps".as_slice().into()).await.unwrap();
        assert!(fs.lookup(apps, &name.into()).await.is_err());

        // The tree changes: so does the manifest, once it's a second old.
        std::fs::write(root.join("neochumby"), b"the session, tampered with").unwrap();
        std::thread::sleep(MANIFEST_FRESH);
        let a = fs.getattr(m).await.unwrap();
        let (text2, _) = fs.read(m, 0, a.size as u32).await.unwrap();
        assert_ne!(text, text2);
        let v2 = neochumby_sync::signed::open(&text2, &key).unwrap();
        assert!(v.read(&root, "neochumby").is_err(), "the old manifest refuses the new file");
        assert!(v2.read(&root, "neochumby").is_ok());

        // With no key that signs, there is none.
        let unsigned = TreeFs::new(root.clone(), no_key());
        assert!(unsigned.lookup(ROOT, &name.into()).await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
