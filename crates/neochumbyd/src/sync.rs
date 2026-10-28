//! `delivery=sync`: neochumbyd's side (neochumby-sync has the rounds). A
//! Chumby whose settings say `delivery=sync` keeps the tree on its stick
//! and asks for rounds over its session; any other is refused.
//!
//! What a Chumby is sent is the tree NFS serves. Files of a Chumby's own
//! (a renderer's output for that Chumby, say) would plug in where [`round`]
//! scans it, as a directory laid over the tree.

use std::io;
use std::time::Duration;

use neochumby_proto::{Msg, Session};
use neochumby_sync::serve::{self, Served};

use crate::api::Api;

/// Rounds for `name`, the first asked for by `first`, until it hangs up.
pub fn serve(api: &Api, name: &str, mut s: Session, first: Msg) -> io::Result<()> {
    // The Chumby stops reading while it fsyncs a large file to its stick.
    s.tcp().set_write_timeout(Some(Duration::from_secs(300)))?;
    let mut m = first;
    loop {
        match m {
            Msg::SyncHave { live } => round(api, name, &mut s, live)?,
            // The Chumby's word on a round neochumbyd refused: already said.
            Msg::SyncDone { .. } => {}
            Msg::Refused => return Ok(()),
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "expected SyncHave")),
        }
        m = match s.recv() {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
    }
}

fn round(api: &Api, name: &str, s: &mut Session, live: [u8; 32]) -> io::Result<()> {
    if !api.state.config(name).iter().any(|(k, v)| k == "delivery" && v == "sync") {
        return s.send(&Msg::Refused);
    }
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
    match served {
        Served::UpToDate => {}
        Served::Refused(why) => eprintln!("neochumbyd: sync for {name}: refused: {why}"),
        Served::Sent { files, bytes, ok, detail } => {
            let how = if ok { "done" } else { "failed" };
            eprintln!("neochumbyd: sync for {name}: sent {files} files, {bytes} bytes; {how}: {detail}");
        }
    }
    Ok(())
}
