//! The speaker's handover between processes: who holds it, and who has
//! asked for it. neochumby's `audio` builds its guards on this; the rule
//! and the files are documented there.
//!
//! It is plain files, not system calls. It lives in this crate, which builds
//! on the host, so that its tests run there: the device library builds only
//! for the Chumby.
//!
//! In the directory ([`DIR`] on the device):
//! - `speaker` names the holder, `<pid> <start>`: its pid and when it
//!   started ([`process::start_time`]; `-` where there is no /proc). It is
//!   created with link(2), which fails if it exists, so two processes can't
//!   both take it; a claim whose process has gone is removed by the next
//!   taker. So is one whose pid is alive but started at another time: the
//!   pid has been given to another process.
//! - `speaker.yield` names the process asking for the speaker the same way,
//!   then when it was written, `<pid> <start> <ms>`: milliseconds of the
//!   monotonic clock ([`time::monotonic`]), which every process shares and
//!   setting the wall clock doesn't move. It is replaced with rename(2), so
//!   it is never read half-written. It counts while its process is alive
//!   and for [`LAPSE`] after it was written.
//!
//! A taker holds a lock on the directory (flock(2)) while it reads and
//! changes the files, so two newcomers can't both find the same stale claim
//! and both replace it. One that finds the lock held for long fails with
//! [`io::ErrorKind::WouldBlock`]: another is taking the speaker, so it
//! tries again soon, as when it has asked.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{process, time};

/// Where the files are on the device: tmpfs, so they go at reboot.
pub const DIR: &str = "/tmp/neochumby";
/// How long a request counts after it was last written: one whose asker
/// stopped trying lapses then.
pub const LAPSE: Duration = Duration::from_secs(5);

/// How long a taker waits for another to finish taking the speaker before
/// it fails with [`io::ErrorKind::WouldBlock`]. Short, as a game may call
/// it between frames; a 2.6.16 tick (10 ms) is the shortest sleep there.
const LOCK_WAIT: Duration = Duration::from_millis(20);

const CLAIM: &str = "speaker";
const REQUEST: &str = "speaker.yield";

/// One process's side of the handover.
#[derive(Clone, Debug)]
pub struct Handover {
    dir: PathBuf,
    me: Id,
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

/// A process as the files name it: its pid, and when it started, if known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Id {
    pid: u32,
    start: Option<u64>,
}

impl Id {
    /// The process `pid` as it is now.
    fn of(pid: u32) -> Id {
        Id { pid, start: process::start_time(pid) }
    }

    /// Whether it is `other`: the same pid, and the same start where both
    /// are known.
    fn is(self, other: Id) -> bool {
        self.pid == other.pid && (self.start.is_none() || other.start.is_none() || self.start == other.start)
    }

    /// Whether the process is still there: its pid is alive, and hasn't
    /// been given to another process since.
    fn alive(self) -> bool {
        process::alive(self.pid) && self.is(Id::of(self.pid))
    }

    /// How the files write it.
    fn line(self) -> String {
        match self.start {
            Some(s) => format!("{} {s}", self.pid),
            None => format!("{} -", self.pid),
        }
    }
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
        Handover { dir: dir.into(), me: Id::of(me) }
    }

    /// Takes the speaker if no other live process holds it or has asked for
    /// it. If another holds it, asks it to yield (or renews this process's
    /// request); if another has asked, leaves that request be. Taking it
    /// withdraws this process's request. Fails with
    /// [`io::ErrorKind::WouldBlock`] if another process is taking it at the
    /// same moment: try again soon.
    pub fn take(&self) -> io::Result<Take> {
        fs::create_dir_all(&self.dir)?;
        let _lock = self.lock()?;
        if let Some(asker) = self.other_request() {
            return Ok(Take::Waiting { asker });
        }
        let claim = self.dir.join(CLAIM);
        // A stale claim is removed and the link tried again. Every taker
        // holds the lock, so a third failure means claims are coming and
        // going faster than this can look, from something that doesn't.
        for _ in 0..3 {
            let tmp = self.dir.join(format!(".{CLAIM}.{}", self.me.pid));
            fs::write(&tmp, format!("{}\n", self.me.line()))?;
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
            match read(&claim) {
                Some((p, _)) if p.is(self.me) => {
                    self.withdraw();
                    return Ok(Take::Taken);
                }
                Some((holder, _)) if holder.alive() => {
                    self.ask()?;
                    return Ok(Take::Asked { holder: holder.pid });
                }
                _ => {
                    #[cfg(test)]
                    tests::before_removing();
                    remove(&claim)?
                }
            }
        }
        Err(io::Error::other("the speaker's claim keeps changing"))
    }

    /// Gives the speaker up, if this process holds it.
    pub fn release(&self) {
        let claim = self.dir.join(CLAIM);
        if read(&claim).is_some_and(|(p, _)| p.is(self.me)) {
            let _ = fs::remove_file(claim);
        }
    }

    /// Withdraws this process's request for the speaker, if it has one.
    pub fn withdraw(&self) {
        let request = self.dir.join(REQUEST);
        if read(&request).is_some_and(|(p, _)| p.is(self.me)) {
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
        self.holder().is_none() && self.other_request().is_none()
    }

    /// The live process holding the speaker, if another one does: the
    /// session asks this when a service dies, to leave a player that's
    /// still going alone.
    pub fn holder(&self) -> Option<u32> {
        let (holder, _) = read(&self.dir.join(CLAIM))?;
        (!holder.is(self.me) && holder.alive()).then_some(holder.pid)
    }

    /// The directory, locked: while it is open, no other taker looks. A
    /// taker holds it for a few file operations, but a child forked
    /// meanwhile (by another thread) holds it too until it execs, so a
    /// held lock is tried again for up to [`LOCK_WAIT`].
    fn lock(&self) -> io::Result<File> {
        let dir = File::open(&self.dir)?;
        let since = time::monotonic();
        while !crate::fs::try_lock(&dir)? {
            if time::monotonic() - since >= LOCK_WAIT {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "another process is taking the speaker"));
            }
            time::sleep_ms(1);
        }
        Ok(dir)
    }

    fn ask(&self) -> io::Result<()> {
        let tmp = self.dir.join(format!(".{REQUEST}.{}", self.me.pid));
        fs::write(&tmp, format!("{} {}\n", self.me.line(), time::monotonic().as_millis()))?;
        fs::rename(&tmp, self.dir.join(REQUEST))
    }

    /// The pid of another live process whose request hasn't lapsed.
    fn other_request(&self) -> Option<u32> {
        let (asker, written) = read(&self.dir.join(REQUEST))?;
        // One from the future (from before a reboot, if the directory
        // outlived it) has lapsed too, as has one with no time.
        let now = time::monotonic().as_millis();
        let fresh = written.is_some_and(|w| w <= now && now - w <= LAPSE.as_millis());
        (fresh && !asker.is(self.me) && asker.alive()).then_some(asker.pid)
    }
}

/// The process a claim or a request names, and the time a request was
/// written, if it says.
fn read(path: &Path) -> Option<(Id, Option<u128>)> {
    let text = fs::read_to_string(path).ok()?;
    let mut words = text.split_whitespace();
    let pid = words.next()?.parse().ok()?;
    let start = words.next().and_then(|s| s.parse().ok());
    let written = words.next().and_then(|w| w.parse().ok());
    Some((Id { pid, start }, written))
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::process::{Child, Command};
    use std::time::SystemTime;

    thread_local! {
        /// What a test has [`Handover::take`] do just before it removes a
        /// stale claim: once, then it's gone.
        static BEFORE_REMOVING: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    }

    /// Runs the step a test set in [`BEFORE_REMOVING`], if any.
    pub(super) fn before_removing() {
        if let Some(f) = BEFORE_REMOVING.with(|b| b.borrow_mut().take()) {
            f();
        }
    }

    /// The pid a claim or a request names.
    fn read_pid(path: &Path) -> Option<u32> {
        read(path).map(|(id, _)| id.pid)
    }

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

    /// Rewrites the request at `path` as if written `by` ago (or, if
    /// negative, that far ahead) by the monotonic clock.
    fn age(path: &Path, by: i128) {
        let (id, _) = read(path).unwrap();
        let at = time::monotonic().as_millis() as i128 - by;
        fs::write(path, format!("{} {at}\n", id.line())).unwrap();
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
        age(&d.file(REQUEST), LAPSE.as_millis() as i128 + 1000);
        assert!(!me.yield_requested());
        // Written in the future: before a reboot the directory outlived.
        age(&d.file(REQUEST), -60_000);
        assert!(!me.yield_requested());
        // With no time (as written before the time was in it).
        fs::write(d.file(REQUEST), format!("{}\n", asker.pid())).unwrap();
        assert!(!me.yield_requested());

        assert_eq!(d.side(dead()).take().unwrap(), Take::Asked { holder: std::process::id() });
        assert!(!me.yield_requested());
    }

    /// #22: the wall clock starts at 1970 and is set once the Chumby syncs.
    /// A request written just before that is still fresh after it, since
    /// its time is the monotonic clock's, not the file's.
    #[test]
    fn setting_the_clock_leaves_a_request_fresh() {
        let d = Dir::new("clock-set");
        let me = d.side(std::process::id());
        assert_eq!(me.take().unwrap(), Take::Taken);
        let asker = Live::new();
        assert_eq!(d.side(asker.pid()).take().unwrap(), Take::Asked { holder: std::process::id() });
        let f = fs::File::options().write(true).open(d.file(REQUEST)).unwrap();
        let hour = Duration::from_secs(3600);
        for written in [SystemTime::UNIX_EPOCH + hour, SystemTime::now() + hour] {
            // 2.6.16 can't set a file's times through a descriptor (futimens).
            match f.set_modified(written) {
                Err(e) if e.kind() == io::ErrorKind::Unsupported => return,
                r => r.unwrap(),
            }
            assert!(me.yield_requested(), "{written:?}");
        }
    }

    /// #20: two newcomers find the same stale claim at once. The second
    /// looks while the first is between reading the claim and replacing it;
    /// it finds the first taking the speaker, and on its next try, asks it.
    #[test]
    fn two_newcomers_on_a_stale_claim_take_it_once() {
        let d = Dir::new("stale-race");
        assert_eq!(d.side(dead()).take().unwrap(), Take::Taken);
        let other = Live::new();
        let (first, second) = (d.side(std::process::id()), d.side(other.pid()));
        let during = std::rc::Rc::new(RefCell::new(None));
        let (s, seen) = (second.clone(), during.clone());
        BEFORE_REMOVING.with(|b| *b.borrow_mut() = Some(Box::new(move || *seen.borrow_mut() = Some(s.take()))));
        assert_eq!(first.take().unwrap(), Take::Taken);
        let during = during.borrow_mut().take().expect("the first found the claim stale");
        assert_eq!(during.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert_eq!(read_pid(&d.file(CLAIM)), Some(std::process::id()));
        assert_eq!(second.take().unwrap(), Take::Asked { holder: std::process::id() });
    }

    /// #21: a holder that died without releasing, whose pid was then given
    /// to another process. The pid is alive, but started at another time.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_reused_pid_is_not_the_holder() {
        let d = Dir::new("reused");
        let other = Live::new();
        let now = Id::of(other.pid());
        let then = Id { start: now.start.map(|s| s.wrapping_sub(1)), ..now };
        fs::create_dir_all(&d.0).unwrap();
        fs::write(d.file(CLAIM), format!("{}\n", then.line())).unwrap();
        fs::write(d.file(REQUEST), format!("{} {}\n", then.line(), time::monotonic().as_millis())).unwrap();
        let me = d.side(std::process::id());
        assert_eq!(me.holder(), None);
        assert!(!me.yield_requested());
        assert!(me.free());
        assert_eq!(me.take().unwrap(), Take::Taken);
        // The process the claim names now, with its own start, still counts.
        me.release();
        assert_eq!(d.side(other.pid()).take().unwrap(), Take::Taken);
        assert_eq!(me.holder(), Some(other.pid()));
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
