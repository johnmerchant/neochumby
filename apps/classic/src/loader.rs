//! The loader: a thread that reads files for the engine, so a slow read
//! never stalls a frame. Reads from the Chumby's USB stick block for a long
//! time (about 175 ms for a full screen, a cold open up to 370 ms), longer
//! than a 40 ms frame, so prefetching happens here while the engine draws,
//! and the engine takes what has arrived in each frame's idle time.
//!
//! Requests and replies cross between the threads as boxes passed through
//! pipes, not through std's channels: std's blocking waits use futex flags
//! (FUTEX_PRIVATE_FLAG, FUTEX_WAIT_BITSET) that the Chumby's 2.6.16 kernel
//! doesn't have. A pipe blocks, and poll()s, on any kernel.
//!
//! If the thread's pipes fail, the engine reads synchronously instead. A
//! panic in the thread is not caught: release builds abort on panic, so it
//! would end the process like a panic anywhere else. Nothing here panics on
//! bad input: a missing, short or wrong-sized file, or a buffer that can't
//! be allocated, comes back as a [`Fail`].

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use crate::cache;

/// Why a file couldn't be read.
#[derive(Debug, PartialEq)]
pub enum Fail {
    Open,
    /// Its size isn't the size the manifest was read with.
    Size(u64),
    /// It got shorter while it was being read.
    Short,
    /// There wasn't the memory to hold it.
    Memory,
}

pub struct Reply {
    pub id: usize,
    pub data: Result<Vec<u16>, Fail>,
}

struct Req {
    id: usize,
    path: PathBuf,
    size: usize,
}

pub struct Loader {
    req: File,
    rep: File,
    tid: Arc<AtomicI32>,
}

/// Reads the first `size` bytes of a file; with `exact`, it must be that
/// size. (The loader checks; a synchronous read, like the C engine's, takes
/// the start of a file that has grown.)
pub fn read_file(path: &std::path::Path, size: usize, exact: bool) -> Result<Vec<u16>, Fail> {
    let mut f = File::open(path).map_err(|_| Fail::Open)?;
    let len = f.metadata().map_err(|_| Fail::Open)?.len();
    if exact && len != size as u64 {
        return Err(Fail::Size(len));
    }
    let mut buf = cache::alloc(size).ok_or(Fail::Memory)?;
    f.read_exact(&mut cache::as_bytes_mut(&mut buf)[..size]).map_err(|_| Fail::Short)?;
    Ok(buf)
}

fn pipe() -> Option<(File, File)> {
    let mut fd = [0; 2];
    // SAFETY: pipe() fills fd on success; each end is then owned by one File.
    if unsafe { libc::pipe(fd.as_mut_ptr()) } < 0 {
        return None;
    }
    for f in fd {
        unsafe { libc::fcntl(f, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Some(unsafe { (File::from_raw_fd(fd[0]), File::from_raw_fd(fd[1])) })
}

fn send<T>(mut to: &File, b: Box<T>) -> Result<(), Box<T>> {
    let p = Box::into_raw(b) as usize;
    // A pipe write of 8 bytes is atomic (under PIPE_BUF).
    match to.write_all(&(p as u64).to_ne_bytes()) {
        Ok(()) => Ok(()),
        // SAFETY: nothing was sent, so the box is still ours.
        Err(_) => Err(unsafe { Box::from_raw(p as *mut T) }),
    }
}

/// The next box from a pipe; None at its end.
fn take<T>(mut from: &File) -> Option<Box<T>> {
    let mut b = [0u8; 8];
    from.read_exact(&mut b).ok()?;
    // SAFETY: the other end sent this pointer once, from Box::into_raw.
    Some(unsafe { Box::from_raw(u64::from_ne_bytes(b) as usize as *mut T) })
}

impl Loader {
    pub fn start() -> Option<Loader> {
        let (req_r, req_w) = pipe()?;
        let (rep_r, rep_w) = pipe()?;
        let tid = Arc::new(AtomicI32::new(-1));
        let t = tid.clone();
        std::thread::Builder::new()
            .name("classicloader".into())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                t.store(unsafe { libc::syscall(libc::SYS_gettid) } as i32, Ordering::Relaxed);
                #[cfg(not(target_os = "linux"))]
                let _ = &t;
                while let Some(r) = take::<Req>(&req_r) {
                    let rep = Box::new(Reply { id: r.id, data: read_file(&r.path, r.size, true) });
                    if send(&rep_w, rep).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        // SAFETY: rep_r is a valid descriptor this process owns.
        unsafe { libc::fcntl(rep_r.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
        Some(Loader { req: req_w, rep: rep_r, tid })
    }

    /// The thread's id, for the stats file; -1 where there's none to give.
    pub fn tid(&self) -> i32 {
        self.tid.load(Ordering::Relaxed)
    }

    /// Asks for a file. False if the thread has gone.
    pub fn request(&self, id: usize, path: PathBuf, size: usize) -> bool {
        send(&self.req, Box::new(Req { id, path, size })).is_ok()
    }

    /// Waits up to `timeout_ms` (-1: for ever) for a reply. Err if the
    /// thread has gone.
    pub fn recv(&self, timeout_ms: i32) -> Result<Option<Box<Reply>>, ()> {
        let mut pf = libc::pollfd { fd: self.rep.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        loop {
            // SAFETY: one valid pollfd.
            let n = unsafe { libc::poll(&mut pf, 1, timeout_ms) };
            if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if n <= 0 {
                return Ok(None);
            }
            break;
        }
        // The 8 bytes of a pointer arrive together; a hang-up with nothing
        // to read is the thread's end.
        let mut b = [0u8; 8];
        match (&self.rep).read(&mut b) {
            Ok(8) => Ok(Some(unsafe { Box::from_raw(u64::from_ne_bytes(b) as usize as *mut Reply) })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_in_the_background() {
        let dir = std::env::temp_dir().join(format!("wx-loader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), [1u8, 0, 2, 0, 3]).unwrap();
        let l = Loader::start().unwrap();
        assert!(l.request(7, dir.join("a"), 5));
        let r = l.recv(5000).unwrap().unwrap();
        assert_eq!(r.id, 7);
        let d = r.data.unwrap();
        assert_eq!(&cache::as_bytes(&d)[..5], &[1, 0, 2, 0, 3]);
        assert!(l.request(8, dir.join("a"), 4));
        assert_eq!(l.recv(5000).unwrap().unwrap().data.unwrap_err(), Fail::Size(5));
        assert!(l.request(9, dir.join("missing"), 4));
        assert_eq!(l.recv(5000).unwrap().unwrap().data.unwrap_err(), Fail::Open);
        assert!(l.recv(0).unwrap().is_none(), "nothing more");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
