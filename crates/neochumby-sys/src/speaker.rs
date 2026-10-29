//! The speaker's handover between processes: who holds it, and who has
//! asked for it. neochumby's `audio` builds its guards on this; the rule
//! and the files are documented there.
//!
//! It is plain files, not system calls. It lives in this crate, which builds
//! on the host, so that its tests run there: the device library builds only
//! for the Chumby.
//!
//! In the directory ([`DIR`] on the device):
//! - `speaker` holds the holder's pid. It is created with link(2), which
//!   fails if it exists, so two processes can't both take it; a claim whose
//!   process has gone is removed by the next taker.
//! - `speaker.yield` holds the pid of the process asking for the speaker. It
//!   is replaced with rename(2), so it is never read half-written. It counts
//!   while its process is alive and for [`LAPSE`] after it was last written.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::process;

/// Where the files are on the device: tmpfs, so they go at reboot.
pub const DIR: &str = "/tmp/neochumby";
/// How long a request counts after it was last written: one whose asker
/// stopped trying lapses then.
pub const LAPSE: Duration = Duration::from_secs(5);

const CLAIM: &str = "speaker";
const REQUEST: &str = "speaker.yield";

/// One process's side of the handover.
#[derive(Clone, Debug)]
pub struct Handover {
    dir: PathBuf,
    me: u32,
}

/// What [`Handover::take`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    /// The speaker is this process's.
    Taken,
    /// Another process holds it and has been asked to yield it.
    Asked { holder: u32 },
    /// Another process has already asked for it, and goes first.
    Waiting { asker: u32 },
}

impl Default for Handover {
    fn default() -> Self {
        Handover::new()
    }
}

impl Handover {
    /// This process's side, in [`DIR`].
    pub fn new() -> Handover {
        Handover::at(DIR, std::process::id())
    }

    /// The side of the process `me`, in `dir`.
    fn at(dir: impl Into<PathBuf>, me: u32) -> Handover {
        Handover { dir: dir.into(), me }
    }

    /// Takes the speaker if no other live process holds it or has asked for
    /// it. If another holds it, asks it to yield (or renews this process's
    /// request); if another has asked, leaves that request be. Taking it
    /// withdraws this process's request.
    pub fn take(&self) -> io::Result<Take> {
        fs::create_dir_all(&self.dir)?;
        if let Some(asker) = self.other_request() {
            return Ok(Take::Waiting { asker });
        }
        let claim = self.dir.join(CLAIM);
        // A stale claim is removed and the link tried again; a third failure
        // means claims are coming and going faster than this can look.
        for _ in 0..3 {
            let tmp = self.dir.join(format!(".{CLAIM}.{}", self.me));
            fs::write(&tmp, format!("{}\n", self.me))?;
            let linked = crate::fs::hard_link(&tmp, &claim);
            let _ = fs::remove_file(&tmp);
            match linked {
                Ok(()) => {
                    self.withdraw();
                    return Ok(Take::Taken);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            match read_pid(&claim) {
                Some(p) if p == self.me => {
                    self.withdraw();
                    return Ok(Take::Taken);
                }
                Some(holder) if process::alive(holder) => {
                    self.ask()?;
                    return Ok(Take::Asked { holder });
                }
                _ => remove(&claim)?,
            }
        }
        Err(io::Error::other("the speaker's claim keeps changing"))
    }

    /// Gives the speaker up, if this process holds it.
    pub fn release(&self) {
        let claim = self.dir.join(CLAIM);
        if read_pid(&claim) == Some(self.me) {
            let _ = fs::remove_file(claim);
        }
    }

    /// Withdraws this process's request for the speaker, if it has one.
    pub fn withdraw(&self) {
        let request = self.dir.join(REQUEST);
        if read_pid(&request) == Some(self.me) {
            let _ = fs::remove_file(request);
        }
    }

    /// Whether another live process has asked for the speaker. Its holder
    /// polls this, and yields when it is true.
    pub fn yield_requested(&self) -> bool {
        self.other_request().is_some()
    }

    /// Whether the speaker is free for this process: no other live process
    /// holds it or has asked for it. A process that yielded waits for this
    /// before taking the speaker again, rather than asking for it.
    pub fn free(&self) -> bool {
        let holder = read_pid(&self.dir.join(CLAIM));
        let held = holder.is_some_and(|p| p != self.me && process::alive(p));
        !held && self.other_request().is_none()
    }

    /// The live process holding the speaker, if another one does: the
    /// session asks this when a service dies, to leave a player that's
    /// still going alone.
    pub fn holder(&self) -> Option<u32> {
        read_pid(&self.dir.join(CLAIM)).filter(|&p| p != self.me && process::alive(p))
    }

    fn ask(&self) -> io::Result<()> {
        let tmp = self.dir.join(format!(".{REQUEST}.{}", self.me));
        fs::write(&tmp, format!("{}\n", self.me))?;
        fs::rename(&tmp, self.dir.join(REQUEST))
    }

    /// The pid of another live process whose request hasn't lapsed.
    fn other_request(&self) -> Option<u32> {
        let request = self.dir.join(REQUEST);
        let asker = read_pid(&request).filter(|&p| p != self.me)?;
        let written = fs::metadata(&request).and_then(|m| m.modified()).ok()?;
        // A request from the future (the clock was set back) has lapsed too:
        // its asker, if still trying, writes it again.
        let fresh = SystemTime::now().duration_since(written).is_ok_and(|age| age <= LAPSE);
        (fresh && process::alive(asker)).then_some(asker)
    }
}

fn read_pid(path: &std::path::Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn remove(path: &std::path::Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::process::{Child, Command};

    /// A directory of its own for each test, removed after.
    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Dir {
            let d = std::env::temp_dir().join(format!("neochumby-speaker-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&d);
            Dir(d)
        }

        fn side(&self, pid: u32) -> Handover {
            Handover::at(&self.0, pid)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Another live process, ended when dropped.
    struct Live(Child);

    impl Live {
        fn new() -> Live {
            Live(Command::new("sleep").arg("60").spawn().unwrap())
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for Live {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// The pid of a process that has gone.
    fn dead() -> u32 {
        let mut c = Command::new("true").spawn().unwrap();
        c.wait().unwrap();
        c.id()
    }

    fn age(path: &std::path::Path, by: Duration) {
        let f = File::options().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - by).unwrap();
    }

    #[test]
    fn taken_and_released() {
        let d = Dir::new("taken");
        let me = d.side(std::process::id());
        assert!(me.free());
        assert_eq!(me.take().unwrap(), Take::Taken);
        assert_eq!(read_pid(&d.file(CLAIM)), Some(std::process::id()));
        assert_eq!(me.take().unwrap(), Take::Taken);
        me.release();
        assert!(!d.file(CLAIM).exists());
    }

    #[test]
    fn the_newcomer_asks_and_the_holder_yields() {
        let d = Dir::new("handover");
        let other = Live::new();
        let holder = d.side(other.pid());
        let me = d.side(std::process::id());
        assert_eq!(me.holder(), None);
        assert_eq!(holder.take().unwrap(), Take::Taken);
        assert!(!holder.yield_requested());
        assert!(!me.free());
        assert_eq!(me.holder(), Some(other.pid()));
        assert_eq!(holder.holder(), None);

        assert_eq!(me.take().unwrap(), Take::Asked { holder: other.pid() });
        assert!(holder.yield_requested());
        assert!(!me.yield_requested());
        // Still held: asking again renews the request.
        assert_eq!(me.take().unwrap(), Take::Asked { holder: other.pid() });

        holder.release();
        // The one that yielded waits for the asker.
        assert!(!holder.free());
        assert_eq!(me.take().unwrap(), Take::Taken);
        assert!(!d.file(REQUEST).exists());
        assert!(!holder.free());
        me.release();
        assert!(holder.free());
    }

    #[test]
    fn a_dead_holder_is_ignored() {
        let d = Dir::new("dead-holder");
        let gone = dead();
        assert_eq!(d.side(gone).take().unwrap(), Take::Taken);
        let me = d.side(std::process::id());
        assert!(me.free());
        assert_eq!(me.take().unwrap(), Take::Taken);
        assert_eq!(read_pid(&d.file(CLAIM)), Some(std::process::id()));
    }

    #[test]
    fn a_garbled_claim_is_ignored() {
        let d = Dir::new("garbled");
        fs::create_dir_all(&d.0).unwrap();
        fs::write(d.file(CLAIM), "not a pid").unwrap();
        assert_eq!(d.side(std::process::id()).take().unwrap(), Take::Taken);
    }

    #[test]
    fn dead_and_lapsed_requests_are_ignored() {
        let d = Dir::new("stale-requests");
        let me = d.side(std::process::id());
        assert_eq!(me.take().unwrap(), Take::Taken);

        let asker = Live::new();
        assert_eq!(d.side(asker.pid()).take().unwrap(), Take::Asked { holder: std::process::id() });
        assert!(me.yield_requested());
        age(&d.file(REQUEST), LAPSE + Duration::from_secs(1));
        assert!(!me.yield_requested());
        // Written in the future: the clock was set back.
        let f = File::options().write(true).open(d.file(REQUEST)).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(60)).unwrap();
        assert!(!me.yield_requested());

        assert_eq!(d.side(dead()).take().unwrap(), Take::Asked { holder: std::process::id() });
        assert!(!me.yield_requested());
    }

    #[test]
    fn the_first_asker_goes_first() {
        let d = Dir::new("two-askers");
        let (h, b, c) = (Live::new(), Live::new(), Live::new());
        let (holder, first, second) = (d.side(h.pid()), d.side(b.pid()), d.side(c.pid()));
        assert_eq!(holder.take().unwrap(), Take::Taken);
        assert_eq!(first.take().unwrap(), Take::Asked { holder: h.pid() });
        assert_eq!(second.take().unwrap(), Take::Waiting { asker: b.pid() });
        assert_eq!(read_pid(&d.file(REQUEST)), Some(b.pid()));

        holder.release();
        assert_eq!(second.take().unwrap(), Take::Waiting { asker: b.pid() });
        assert_eq!(first.take().unwrap(), Take::Taken);
        // Then the second asks the first, which yields and waits.
        assert_eq!(second.take().unwrap(), Take::Asked { holder: b.pid() });
        assert!(first.yield_requested());
        first.release();
        assert!(!first.free());
        assert_eq!(second.take().unwrap(), Take::Taken);
    }

    #[test]
    fn only_the_owner_releases_or_withdraws() {
        let d = Dir::new("owner");
        let other = Live::new();
        let them = d.side(other.pid());
        let me = d.side(std::process::id());
        assert_eq!(me.take().unwrap(), Take::Taken);
        assert_eq!(them.take().unwrap(), Take::Asked { holder: std::process::id() });
        them.release();
        assert!(d.file(CLAIM).exists());
        me.withdraw();
        assert!(d.file(REQUEST).exists());
        them.withdraw();
        assert!(!me.yield_requested());
        me.release();
        assert!(!d.file(CLAIM).exists());
    }
}
