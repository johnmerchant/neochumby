//! The Chumby's side of a round, run by neochumby-sync over a session that
//! is past `Welcome` and `Config`.

use std::collections::HashMap;
use std::io;

use neochumby_proto::{Msg, Session};

use crate::stick::{Set, Stick};
use crate::{Clock, Entry, Timing, invalid, manifest, next, recv_manifest, unexpected};

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Rolled back: nothing is synced until `neochumby-sync resume`.
    Held,
    /// The only set it could write is the one running: after a flip, until
    /// the Chumby restarts into the new set.
    WaitingForRestart(Set),
    /// The live set is the tree.
    UpToDate,
    /// neochumbyd does not sync this Chumby (its delivery is not sync).
    Refused,
    /// The standby set was brought up to the tree and made live: `received`
    /// of its `files` were sent, `bytes` in all.
    Flipped { to: Set, files: usize, received: usize, bytes: u64 },
}

/// One round. On an error the live set is as it was, and the standby set
/// is left incomplete for the next round to finish; neochumbyd is told why
/// if the session still works.
pub fn round(s: &mut Session, stick: &Stick, t: Timing) -> io::Result<Outcome> {
    let r = run(s, stick, &t.start());
    if let Err(e) = &r {
        let mut detail = e.to_string();
        detail.truncate(1000);
        let _ = s.send(&Msg::SyncDone { ok: false, detail });
    }
    r
}

fn run(s: &mut Session, stick: &Stick, wait: &Clock) -> io::Result<Outcome> {
    if stick.held() {
        return Ok(Outcome::Held);
    }
    stick.mounted()?;
    let live = stick.live();
    let target = match &live {
        Some((l, _)) => l.other(),
        None => stick.current().map_or(Set::A, Set::other),
    };
    if stick.running == Some(target) {
        return Ok(Outcome::WaitingForRestart(target));
    }
    let digest = live.as_ref().map_or([0; 32], |(_, m)| manifest::digest(m));
    s.send(&Msg::SyncHave { live: digest })?;
    match next(s, wait)? {
        Msg::SyncUpToDate => return Ok(Outcome::UpToDate),
        Msg::Refused => return Ok(Outcome::Refused),
        Msg::SyncWant => {}
        Msg::SyncDone { detail, .. } => return Err(invalid(format!("neochumbyd will not send its tree: {detail}"))),
        m => return Err(unexpected(&m)),
    }

    let have = stick.have(target);
    for m in manifest::messages(&have) {
        s.send(&m)?;
    }
    let tree = recv_manifest(s, wait, &stick.limits)?;
    manifest::validate(&tree, &stick.limits)?;
    // Exactly these must come, and nothing else.
    let mut need: HashMap<String, Entry> =
        manifest::plan(&tree, &have).into_iter().map(|e| (e.path.clone(), e.clone())).collect();
    // Complete with this tree already (a flip a power cut stopped short):
    // flipped without a write.
    let done = need.is_empty() && stick.check(target).is_ok_and(|m| m == tree);
    let mut w = if done { None } else { Some(stick.begin(target, &tree, &have)?) };
    let (mut received, mut bytes) = (0, 0u64);
    loop {
        match next(s, wait)? {
            Msg::SyncFile(e) => {
                if need.remove(&e.path).as_ref() != Some(&e) {
                    return Err(invalid(format!("{}: not a file this set lacks", e.path)));
                }
                let w = w.as_mut().ok_or_else(|| invalid("nothing to write"))?;
                w.receive(&e, || match next(s, wait)? {
                    Msg::SyncData(d) => Ok(d),
                    m => Err(unexpected(&m)),
                })?;
                received += 1;
                bytes += e.size;
            }
            Msg::SyncEnd => break,
            m => return Err(unexpected(&m)),
        }
    }
    if let Some(e) = need.values().next() {
        return Err(invalid(format!("{}: never sent", e.path)));
    }
    if let Some(w) = w {
        w.finish()?;
    }
    stick.flip(target)?;
    let files = tree.len();
    let detail = format!("set {} live: {received} of {files} files sent, {bytes} bytes", target.name());
    s.send(&Msg::SyncDone { ok: true, detail })?;
    Ok(Outcome::Flipped { to: target, files, received, bytes })
}
