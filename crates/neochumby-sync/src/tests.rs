//! Rounds over a real session on loopback: neochumbyd's side and the
//! Chumby's, a temporary directory standing in for the stick.

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use neochumby_proto::{Identity, Msg, Session};

use crate::manifest::tests::entry;
use crate::receive::{Outcome, round};
use crate::serve::{self, Hashes, Served};
use crate::stick::tests::tempdir;
use crate::stick::{Set, Stick};

const WAIT: Duration = Duration::from_secs(10);

fn sessions() -> (Session, Session) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let t = std::thread::spawn(move || Session::accept(l.accept().unwrap().0, &Identity::generate().unwrap()).unwrap());
    let c = Session::connect(TcpStream::connect(addr).unwrap(), &Identity::generate().unwrap()).unwrap();
    (t.join().unwrap(), c)
}

/// neochumbyd's loop, as neochumbyd runs it: a round for each SyncHave,
/// until the Chumby hangs up. What each round did.
fn server(mut s: Session, layers: Vec<PathBuf>) -> std::thread::JoinHandle<Vec<Served>> {
    std::thread::spawn(move || {
        let hashes = Hashes::default();
        let mut out = Vec::new();
        while let Ok(Msg::SyncHave { live }) = s.recv() {
            let tree = serve::scan(&layers, &hashes).unwrap();
            out.push(serve::round(&mut s, live, &tree).unwrap());
        }
        out
    })
}

fn put(dir: &Path, path: &str, data: &[u8]) {
    let p = dir.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, data).unwrap();
}

fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

#[test]
fn rounds_end_to_end() {
    let root = tempdir("rounds");
    let (tree, overlay) = (root.join("tree"), root.join("overlay"));
    put(&tree, "neochumby", b"session 1");
    put(&tree, "userhook2", b"#!/bin/sh\n");
    put(&tree, "apps/home/manifest", b"home=neochumby-home\n");
    put(&tree, "apps/doom/doom1.wad", &noise(300_000, 1));
    put(&tree, ".hidden", b"never sent");
    std::os::unix::fs::symlink("/etc/passwd", tree.join("apps/link")).unwrap();
    let st = Stick::new(root.join("stick/neochumby"), None);
    fs::create_dir_all(root.join("stick")).unwrap();

    let (srv, mut c) = sessions();
    let served = server(srv, vec![tree.clone(), overlay.clone()]);

    // First: everything, into a.
    let o = round(&mut c, &st, WAIT).unwrap();
    assert_eq!(o, Outcome::Flipped { to: Set::A, files: 4, received: 4, bytes: 300_000 + 9 + 10 + 20 });
    let a = st.set_dir(Set::A);
    assert_eq!(fs::read(a.join("apps/doom/doom1.wad")).unwrap(), noise(300_000, 1));
    assert!(!a.join(".hidden").exists() && !a.join("apps/link").exists());
    // Again: nothing to do, nothing written.
    let marker_time = fs::metadata(root.join("stick/neochumby/a.complete")).unwrap().modified().unwrap();
    assert_eq!(round(&mut c, &st, WAIT).unwrap(), Outcome::UpToDate);

    // The tree changes, and this Chumby gets a file of its own: into b,
    // only what b lacks (b is empty), a untouched.
    put(&tree, "neochumby", b"session 2");
    fs::remove_file(tree.join("apps/home/manifest")).unwrap();
    put(&overlay, "apps/wx/forecast.raw", &noise(70_000, 2));
    put(&overlay, "userhook2", b"#!/bin/sh\n# mine\n");
    let o = round(&mut c, &st, WAIT).unwrap();
    assert!(matches!(o, Outcome::Flipped { to: Set::B, files: 4, received: 4, .. }), "{o:?}");
    assert_eq!(fs::read(st.set_dir(Set::B).join("userhook2")).unwrap(), b"#!/bin/sh\n# mine\n");
    assert_eq!(fs::read(a.join("neochumby")).unwrap(), b"session 1");
    assert!(a.join("apps/home/manifest").exists());
    assert_eq!(fs::metadata(root.join("stick/neochumby/a.complete")).unwrap().modified().unwrap(), marker_time);

    // One file changes: into a, which is sent what it lacks against its
    // own marker. The home manifest is gone from a too.
    put(&tree, "neochumby", b"session 3");
    let o = round(&mut c, &st, WAIT).unwrap();
    assert_eq!(o, Outcome::Flipped { to: Set::A, files: 4, received: 3, bytes: 9 + 70_000 + 17 });
    assert!(!a.join("apps/home/manifest").exists());
    assert_eq!(fs::read(a.join("neochumby")).unwrap(), b"session 3");
    assert_eq!(st.live().unwrap().0, Set::A);

    // Rolled back: b live, and no rounds while held.
    assert_eq!(st.rollback().unwrap(), Set::B);
    assert_eq!(round(&mut c, &st, WAIT).unwrap(), Outcome::Held);
    st.resume().unwrap();
    // Running from b after a flip to a: nothing to write until a restart.
    let running = Stick::new(st.dir.clone(), Some(Set::A));
    assert_eq!(round(&mut c, &running, WAIT).unwrap(), Outcome::WaitingForRestart(Set::A));
    assert_eq!(round(&mut c, &st, WAIT).unwrap(), Outcome::Flipped { to: Set::A, files: 4, received: 0, bytes: 0 });

    drop(c);
    let served = served.join().unwrap();
    let sent: Vec<(usize, bool)> = served
        .iter()
        .filter_map(|s| match s {
            Served::Sent { files, ok, .. } => Some((*files, *ok)),
            Served::UpToDate => None,
        })
        .collect();
    assert_eq!(sent, [(4, true), (4, true), (3, true), (0, true)]);
    fs::remove_dir_all(&root).unwrap();
}

/// A neochumbyd that sends what it likes: refused, the live set as it was.
#[test]
fn a_bad_server_changes_nothing_live() {
    let root = tempdir("bad");
    let st = Stick::new(root.join("neochumby"), None);
    let good = vec![entry("neochumby", b"session")];
    // A live a, from a good round.
    crate::stick::tests::write_set(&st, Set::A, &[("neochumby", b"session")]).unwrap();
    st.flip(Set::A).unwrap();

    let cases: Vec<(&str, Vec<Msg>)> = vec![
        ("a path out of the set", vec![manifest_msg(vec![entry("../x", b"x"), good[0].clone()])]),
        ("no session", vec![manifest_msg(vec![entry("x", b"x")])]),
        (
            "a file not in the manifest",
            vec![manifest_msg(vec![entry("neochumby", b"new")]), Msg::SyncFile(entry("other", b"x"))],
        ),
        (
            "bytes that do not match",
            vec![
                manifest_msg(vec![entry("neochumby", b"new")]),
                Msg::SyncFile(entry("neochumby", b"new")),
                Msg::SyncData(b"old".to_vec()),
            ],
        ),
        ("a file never sent", vec![manifest_msg(vec![entry("neochumby", b"new")]), Msg::SyncEnd]),
    ];
    for (what, msgs) in cases {
        let (mut srv, mut c) = sessions();
        let t = std::thread::spawn(move || {
            let Ok(Msg::SyncHave { .. }) = srv.recv() else { panic!() };
            srv.send(&Msg::SyncWant).unwrap();
            while !matches!(srv.recv().unwrap(), Msg::SyncManifest { last: true, .. }) {}
            for m in msgs {
                srv.send(&m).unwrap();
            }
            // The Chumby says why, unless it already hung up.
            srv.recv_timeout(WAIT).ok().flatten()
        });
        assert!(round(&mut c, &st, WAIT).is_err(), "{what}");
        drop(c);
        let said = t.join().unwrap();
        assert!(matches!(said, Some(Msg::SyncDone { ok: false, .. }) | None), "{what}: {said:?}");
        assert_eq!(st.live().unwrap(), (Set::A, good.clone()), "{what}");
        assert_eq!(fs::read(st.set_dir(Set::A).join("neochumby")).unwrap(), b"session");
    }
    fs::remove_dir_all(&root).unwrap();
}

fn manifest_msg(entries: Vec<crate::Entry>) -> Msg {
    Msg::SyncManifest { entries, last: true }
}

/// The connection drops mid-file: the next round sends only what the
/// journal does not list.
#[test]
fn a_dropped_round_resumes() {
    let root = tempdir("dropped");
    let tree = root.join("tree");
    put(&tree, "apps/doom/doom1.wad", &noise(200_000, 3));
    put(&tree, "apps/quake/id1/pak0.pak", &noise(500_000, 4));
    put(&tree, "neochumby", b"session");
    let st = Stick::new(root.join("neochumby"), None);

    let (mut srv, mut c) = sessions();
    let layers = vec![tree.clone()];
    let t = std::thread::spawn(move || {
        let Ok(Msg::SyncHave { .. }) = srv.recv() else { panic!() };
        let t = serve::scan(&layers, &Hashes::default()).unwrap();
        srv.send(&Msg::SyncWant).unwrap();
        while !matches!(srv.recv().unwrap(), Msg::SyncManifest { last: true, .. }) {}
        for m in crate::manifest::messages(&t.manifest()) {
            srv.send(&m).unwrap();
        }
        // The wad whole, then half of the pak, then gone.
        let wad = fs::read(&t.files[0].file).unwrap();
        srv.send(&Msg::SyncFile(t.files[0].entry.clone())).unwrap();
        for c in wad.chunks(crate::CHUNK) {
            srv.send(&Msg::SyncData(c.to_vec())).unwrap();
        }
        srv.send(&Msg::SyncFile(t.files[1].entry.clone())).unwrap();
        srv.send(&Msg::SyncData(noise(500_000, 4)[..60_000].to_vec())).unwrap();
    });
    assert!(round(&mut c, &st, WAIT).is_err());
    t.join().unwrap();
    assert!(st.live().is_none());

    let (srv, mut c) = sessions();
    let served = server(srv, vec![tree]);
    let o = round(&mut c, &st, WAIT).unwrap();
    assert_eq!(o, Outcome::Flipped { to: Set::A, files: 3, received: 2, bytes: 500_007 });
    drop(c);
    assert!(matches!(served.join().unwrap()[..], [Served::Sent { files: 2, ok: true, .. }]));
    fs::remove_dir_all(&root).unwrap();
}
