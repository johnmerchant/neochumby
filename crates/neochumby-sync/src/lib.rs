//! `delivery=sync`: a Chumby keeps a whole copy of its tree on its USB
//! stick, in step with its neochumbyd, and boots from it. The other way,
//! `delivery=nfs`, mounts the tree from neochumbyd on every boot. Each
//! Chumby's owner chooses (`neochumbyd set NAME delivery=sync`).
//!
//! On the stick (FAT), in its neochumby directory:
//!
//!   current        "a" or "b": the live set
//!   a/  b/         two sets, each a whole tree (the session, userhook2,
//!                  apps/...)
//!   a.complete     a's completion marker, written last: its manifest
//!   a.journal      while a is being written: its files so far
//!   hold           after a rollback: no syncing until it is removed
//!
//! A round ([`receive::round`] on the Chumby, [`serve::round`] on
//! neochumbyd), over the paired session and nothing else:
//!
//! 1. The Chumby sends the digest of its live set's manifest. If it is the
//!    tree's, nothing is written at all.
//! 2. Otherwise it sends the standby set's manifest (from its marker, or
//!    its journal if a round was cut short), and neochumbyd sends the
//!    tree's manifest, then only the files whose SHA-256 differs, whole.
//! 3. The Chumby writes the standby set only, and only while the stick is
//!    mounted. Its marker goes first, so the set is incomplete from then
//!    on. Files the new manifest does not name are removed, then the stick
//!    must have room for the rest. Each file lands under a temporary name,
//!    is hashed as it streams in (not read back: on the Chumby, dropping
//!    caches to read the stick stalls programs running from it), and is
//!    renamed into place. Files are synced in batches, and listed in the
//!    journal only once synced.
//! 4. Once every file is there, any kept on a journal's word (from a round
//!    cut short) are hashed after all, since a power cut or a cheap stick
//!    can leave the right size over the wrong bytes. Then the marker is
//!    written, carrying the manifest, then `current` is rewritten to name
//!    the set (a temporary name, then a rename, with syncs). A round has a
//!    time limit in all ([`Timing`]).
//!
//! A rename on FAT is not crash-safe, so the boot hook never trusts
//! `current` alone: it runs the set `current` names only if that set's
//! marker is whole and every file it names is there at its size
//! ([`stick::check_set`], `neochumby check-set`), else the other set, else
//! it boots as before. A power cut at any point leaves one complete set.
//! The live set is never written, nor the set the Chumby is running from.

#[cfg(test)]
mod crash;
pub mod disk;
pub mod manifest;
pub mod receive;
pub mod serve;
pub mod stick;
#[cfg(test)]
mod tests;

pub use neochumby_proto::SyncEntry as Entry;

/// What a Chumby takes in a set. neochumbyd chooses the files, so the
/// Chumby bounds them: a set it cannot hold is refused whole, and the live
/// set stays up.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_files: usize,
    /// The largest file (Quake's pak0.pak is 18 MB).
    pub max_file: u64,
    /// A whole set. The stick holds two.
    pub max_total: u64,
}

pub const LIMITS: Limits = Limits { max_files: 8192, max_file: 64 << 20, max_total: 512 << 20 };

/// What a round talks over: a paired session, or in tests a pair of
/// channels.
pub trait Link {
    fn send(&mut self, m: &neochumby_proto::Msg) -> std::io::Result<()>;
    /// The next message if one comes within `t`.
    fn recv_timeout(&mut self, t: std::time::Duration) -> std::io::Result<Option<neochumby_proto::Msg>>;
}

impl Link for neochumby_proto::Session {
    fn send(&mut self, m: &neochumby_proto::Msg) -> std::io::Result<()> {
        neochumby_proto::Session::send(self, m)
    }

    fn recv_timeout(&mut self, t: std::time::Duration) -> std::io::Result<Option<neochumby_proto::Msg>> {
        neochumby_proto::Session::recv_timeout(self, t)
    }
}

/// How long a round may wait: for each message, and in all. A round that
/// takes longer is given up, so a server cannot hold one open by sending
/// a little at a time.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub step: std::time::Duration,
    pub round: std::time::Duration,
}

/// A round's deadlines, once it has started.
struct Clock {
    step: std::time::Duration,
    end: std::time::Instant,
}

impl Timing {
    fn start(self) -> Clock {
        Clock { step: self.step, end: std::time::Instant::now() + self.round }
    }
}

/// The most of a file one `SyncData` carries.
pub const CHUNK: usize = 60 << 10;

fn invalid(what: impl ToString) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string())
}

/// The next message, within the step's time and the round's. `recv_timeout`
/// gives up after the first read that brings part of a message, so it is
/// asked again until the time is up.
fn next(s: &mut dyn Link, c: &Clock) -> std::io::Result<neochumby_proto::Msg> {
    let deadline = (std::time::Instant::now() + c.step).min(c.end);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            let what = if deadline == c.end { "the round took too long" } else { "no answer" };
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, what));
        }
        if let Some(m) = s.recv_timeout(left)? {
            return Ok(m);
        }
    }
}

/// A message that has no place here, said briefly (a `SyncData` would
/// otherwise print its bytes).
fn unexpected(m: &neochumby_proto::Msg) -> std::io::Error {
    let d: String = match m {
        neochumby_proto::Msg::SyncData(d) => format!("SyncData of {} bytes", d.len()),
        m => format!("{m:?}").chars().take(80).collect(),
    };
    invalid(format!("unexpected {d}"))
}

/// A manifest the other side sends, as many `SyncManifest`s as it takes.
fn recv_manifest(s: &mut dyn Link, c: &Clock, limits: &Limits) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    loop {
        match next(s, c)? {
            neochumby_proto::Msg::SyncManifest { entries, last } => {
                if entries.is_empty() && !last {
                    return Err(invalid("an empty part of a manifest"));
                }
                out.extend(entries);
                if out.len() > limits.max_files {
                    return Err(invalid("too many files"));
                }
                if last {
                    return Ok(out);
                }
            }
            m => return Err(unexpected(&m)),
        }
    }
}
