//! `delivery=sync`: neochumbyd's side (neochumby-sync has the rounds). A
//! Chumby whose settings say `delivery=sync` keeps its tree on its stick
//! and asks for rounds over its session; any other is refused.
//!
//! A Chumby's tree is made of layers ([`layers`]), later ones winning:
//!
//!   <tree>                  the shared tree: what NFS serves
//!   <state>/overlay/<name>  this Chumby's own files, laid out like the
//!                           tree (overlay/kitchen/apps/wx/forecast.raw)
//!
//! The overlay is the hook for a per-Chumby renderer (Matt's feeder, say):
//! it writes into overlay/<name>/, putting each file in under a name
//! starting with '.' and renaming it into place, and the Chumby is sent
//! what changed in its next round, within a minute. A renderer should keep
//! file names stable and leave unchanged files alone, so that only what
//! changed is sent. The overlay is sync's only: NFS serves the shared tree.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use neochumby_proto::{Msg, Session};
use neochumby_sync::serve::{self, Served};

use crate::api::Api;

/// The directories `name`'s tree is made of, the later winning.
pub fn layers(tree: &Path, state_dir: &Path, name: &str) -> Vec<PathBuf> {
    vec![tree.to_path_buf(), state_dir.join("overlay").join(name)]
}

/// Rounds for `name`, the first asked for by `first`, until it hangs up.
pub fn serve(api: &Api, name: &str, mut s: Session, first: Msg) -> io::Result<()> {
    // The Chumby stops reading while it fsyncs a large file to its stick.
    s.tcp().set_write_timeout(Some(Duration::from_secs(300)))?;
    let mut m = first;
    loop {
        let Msg::SyncHave { live } = m else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "expected SyncHave"));
        };
        if !api.state.config(name).iter().any(|(k, v)| k == "delivery" && v == "sync") {
            return s.send(&Msg::Refused);
        }
        let tree = serve::scan(&layers(&api.tree, &api.state_dir, name), &api.hashes)?;
        match serve::round(&mut s, live, &tree)? {
            Served::UpToDate => {}
            Served::Sent { files, bytes, ok, detail } => {
                for l in &tree.left_out {
                    eprintln!("neochumbyd: sync for {name}: left out {l}");
                }
                let how = if ok { "done" } else { "failed" };
                eprintln!("neochumbyd: sync for {name}: sent {files} files, {bytes} bytes; {how}: {detail}");
            }
        }
        m = match s.recv() {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
    }
}
