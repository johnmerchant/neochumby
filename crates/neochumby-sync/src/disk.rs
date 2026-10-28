//! Every file operation sync makes on the stick, as one small interface
//! ([`Disk`]). [`RealDisk`] is the stick itself; the crash tests put a
//! model in its place that loses power after any operation (crash.rs).
//!
//! The operations are the ones the stick code needs and no more. None
//! follows a symbolic link.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use neochumby_sys::fs::NO_FOLLOW;

/// What a name is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    File,
    Dir,
    /// A symbolic link, or anything else that isn't a plain file or
    /// directory.
    Other,
}

/// A name's kind and size, without following a link.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Meta {
    pub kind: Kind,
    pub len: u64,
}

/// A file being written.
pub trait Output {
    fn write_all(&mut self, b: &[u8]) -> io::Result<()>;
    /// Its bytes, on the stick (fsync).
    fn sync(&mut self) -> io::Result<()>;
}

pub trait Disk: Send + Sync {
    /// A file to read from start to end.
    fn open(&self, p: &Path) -> io::Result<Box<dyn Read>>;
    fn stat(&self, p: &Path) -> io::Result<Meta>;
    /// A directory's entries: each name and kind.
    fn list(&self, dir: &Path) -> io::Result<Vec<(OsString, Kind)>>;
    /// A new file, with permissions `mode` where the filesystem keeps
    /// them; an error if the name is taken.
    fn create(&self, p: &Path, mode: u32) -> io::Result<Box<dyn Output>>;
    /// A file to add to at its end.
    fn append(&self, p: &Path) -> io::Result<Box<dyn Output>>;
    /// `from` renamed to `to`, replacing what `to` was.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&self, p: &Path) -> io::Result<()>;
    fn remove_dir_all(&self, p: &Path) -> io::Result<()>;
    fn create_dir(&self, p: &Path) -> io::Result<()>;
    /// `dir`'s entries, on the stick.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
    /// Everything written, on the stick (sync(2)).
    fn sync(&self);
    /// The bytes still free on the filesystem holding `p`.
    fn available(&self, p: &Path) -> io::Result<u64>;
    /// Whether a filesystem is mounted at `mount`: it is another device
    /// than the directory it is in.
    fn mounted(&self, mount: &Path) -> bool;
}

/// The filesystem itself.
pub struct RealDisk;

struct RealOutput(File);

impl Output for RealOutput {
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        self.0.write_all(b)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.0.sync_all()
    }
}

fn kind(t: fs::FileType) -> Kind {
    if t.is_file() {
        Kind::File
    } else if t.is_dir() {
        Kind::Dir
    } else {
        Kind::Other
    }
}

impl Disk for RealDisk {
    fn open(&self, p: &Path) -> io::Result<Box<dyn Read>> {
        Ok(Box::new(OpenOptions::new().read(true).custom_flags(NO_FOLLOW).open(p)?))
    }

    fn stat(&self, p: &Path) -> io::Result<Meta> {
        let m = fs::symlink_metadata(p)?;
        Ok(Meta { kind: kind(m.file_type()), len: m.len() })
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<(OsString, Kind)>> {
        let mut out = Vec::new();
        for e in fs::read_dir(dir)? {
            let e = e?;
            out.push((e.file_name(), kind(e.file_type()?)));
        }
        Ok(out)
    }

    fn create(&self, p: &Path, mode: u32) -> io::Result<Box<dyn Output>> {
        let f = OpenOptions::new().write(true).create_new(true).mode(mode).custom_flags(NO_FOLLOW).open(p)?;
        Ok(Box::new(RealOutput(f)))
    }

    fn append(&self, p: &Path) -> io::Result<Box<dyn Output>> {
        Ok(Box::new(RealOutput(OpenOptions::new().append(true).custom_flags(NO_FOLLOW).open(p)?)))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn remove_file(&self, p: &Path) -> io::Result<()> {
        fs::remove_file(p)
    }

    fn remove_dir_all(&self, p: &Path) -> io::Result<()> {
        fs::remove_dir_all(p)
    }

    fn create_dir(&self, p: &Path) -> io::Result<()> {
        fs::create_dir(p)
    }

    /// A filesystem that cannot fsync a directory says EINVAL
    /// (InvalidInput); a whole sync() does it then.
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        match File::open(dir)?.sync_all() {
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
                self.sync();
                Ok(())
            }
            r => r,
        }
    }

    fn sync(&self) {
        neochumby_sys::fs::sync();
    }

    fn available(&self, p: &Path) -> io::Result<u64> {
        neochumby_sys::fs::available_bytes(p)
    }

    fn mounted(&self, mount: &Path) -> bool {
        let parent = mount.parent().unwrap_or(Path::new("/"));
        match (fs::metadata(mount), fs::metadata(parent)) {
            (Ok(a), Ok(b)) => a.dev() != b.dev(),
            _ => false,
        }
    }
}
