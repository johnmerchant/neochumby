//! Keeping a Chumby's stick in step with the tree: neochumbyd's side
//! (neochumby-sync has the rounds). A registered Chumby asks for rounds over
//! its session.
//!
//! Files of a Chumby's own (a renderer's output for that Chumby, say) would
//! plug in where [`round`] scans the tree, as a directory laid over it.

use std::io;
use std::time::Duration;

use neochumby_proto::{Msg, Session};
use neochumby_sync::serve::{self, Served};

use crate::api::Api;
use crate::dev;

/// Rounds for `name`, the first asked for by `first`, until it hangs up.
pub fn serve(api: &Api, name: &str, mut s: Session, first: Msg) -> io::Result<()> {
    // The Chumby stops reading while it fsyncs a large file to its stick.
    s.stream().set_write_timeout(Some(Duration::from_secs(300)))?;
    let mut m = first;
    loop {
        match m {
            Msg::SyncHave { live } => round(api, name, &mut s, live)?,
            // The Chumby's word on a round it can't start. (After a round
            // neochumbyd refused, it says so too: already answered.)
            Msg::SyncDone { ok, detail } => {
                if dev::sync_started(&api.state_dir, name) {
                    dev::sync_answer(&api.state_dir, name, ok, &detail);
                }
            }
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "expected SyncHave")),
        }
        m = match s.recv() {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
    }
}

/// One round, answering `neochumbyd sync` if it asked before the round began.
fn round(api: &Api, name: &str, s: &mut Session, live: [u8; 32]) -> io::Result<()> {
    let _running = dev::round_runs(&api.state_dir, name);
    let asked = dev::sync_started(&api.state_dir, name);
    let r = send(api, name, s, live);
    if asked {
        let (ok, what) = match &r {
            Ok(Served::UpToDate) => (true, "up to date".to_string()),
            Ok(Served::Refused(why)) => (false, why.clone()),
            Ok(Served::Sent { files, bytes, ok, detail }) => {
                (*ok, format!("sent {files} files, {bytes} bytes: {detail}"))
            }
            Err(e) => (false, e.to_string()),
        };
        dev::sync_answer(&api.state_dir, name, ok, &what);
    }
    r.map(|_| ())
}

fn send(api: &Api, name: &str, s: &mut Session, live: [u8; 32]) -> io::Result<Served> {
    let served = match serve::scan(&api.tree, &api.hashes) {
        Ok(tree) => {
            let r = serve::round(s, live, &tree)?;
            if !matches!(r, Served::UpToDate) {
                for l in &tree.left_out {
                    eprintln!("neochumbyd: sync for {name}: left out {l}");
                }
            }
            r
        }
        Err(e) => serve::refuse(s, format!("the tree: {e}"))?,
    };
    match &served {
        Served::UpToDate => {}
        Served::Refused(why) => eprintln!("neochumbyd: sync for {name}: refused: {why}"),
        Served::Sent { files, bytes, ok, detail } => {
            let how = if *ok { "done" } else { "failed" };
            eprintln!("neochumbyd: sync for {name}: sent {files} files, {bytes} bytes; {how}: {detail}");
        }
    }
    Ok(served)
}
