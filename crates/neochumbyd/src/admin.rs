//! The admin socket: how an app's daemon (daemon.rs), such as the
//! household web page, pairs Chumbys and sets their settings. It does what
//! the command line does, through the same functions (state.rs).
//!
//! A Unix socket, `<state>/admin.sock`, mode 0600. neochumbyd also checks
//! each connection's peer: only its own user and root are answered. Daemons
//! run as neochumbyd's user, so they can connect; nobody else can.
//!
//! The protocol is lines of text. A request is a command and its words,
//! separated by spaces, ending in a newline, at most 1024 bytes. The answer
//! is any number of lines of data, then one line that is `ok`, perhaps with
//! a word, or `error` and a message. A connection takes any number of
//! requests, one after another. No value holds a space: names, codes, keys,
//! addresses and settings are all single words.
//!
//!   paired             paired NAME KEY [KEY=VALUE ...]   each Chumby paired,
//!                      ok                                with its settings
//!   pending            pending CODE NAME ADDRESS KEY     each asking to pair
//!                      ok
//!   approve CODE TOKEN ok NAME          pairs the Chumby showing CODE; TOKEN
//!                                       is the owner's (`neochumbyd token`)
//!   refuse CODE        ok
//!   forget NAME TOKEN  ok               unpairs NAME; TOKEN as above
//!   set NAME KEY=VALUE ok               one of a Chumby's settings; KEY=
//!                                       removes it
//!   check TOKEN        ok               whether TOKEN is the owner's, for
//!                                       the page's own login
//!
//! Approving and forgetting need the owner's token. Comparing the code on
//! both screens is what pairing's security rests on, so the person
//! approving must be the owner, not just someone on the network who can
//! reach the web page. Forgetting undoes a pairing, which only someone at
//! the Chumby can make again, so a fault in the page must not be able to
//! do it on its own. A wrong token is answered after a second, to slow
//! guessing through the page.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use neochumby_proto::hex;

use crate::state::State;

/// The longest request line.
const MAX_LINE: usize = 1024;

/// Binds the socket at `path`, mode 0600, in place of one left by a
/// neochumbyd that has gone. Fails if one is still answering there.
pub fn listen(path: &Path) -> io::Result<UnixListener> {
    if UnixStream::connect(path).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AddrInUse, "another neochumbyd has this admin socket"));
    }
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let l = UnixListener::bind(path)?;
    // Between bind and this, the socket has the umask's mode; the peer
    // check below holds either way.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

pub fn serve(listener: UnixListener, state: State) {
    for conn in listener.incoming().flatten() {
        match peer_uid(&conn) {
            Ok(uid) if allowed(uid) => {
                let state = state.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle(&state, conn) {
                        eprintln!("neochumbyd: admin: {e}");
                    }
                });
            }
            Ok(uid) => eprintln!("neochumbyd: admin: refused a connection from uid {uid}"),
            Err(e) => eprintln!("neochumbyd: admin: {e}"),
        }
    }
}

/// neochumbyd's own user, and root.
fn allowed(uid: libc::uid_t) -> bool {
    uid == 0 || uid == unsafe { libc::geteuid() }
}

/// The user at the other end of the connection, as the kernel says.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(s: &UnixStream) -> io::Result<libc::uid_t> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

/// The user at the other end of the connection, as the kernel says.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn peer_uid(s: &UnixStream) -> io::Result<libc::uid_t> {
    let (mut uid, mut gid) = (0, 0);
    if unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

fn handle(state: &State, conn: UnixStream) -> io::Result<()> {
    let mut out = conn.try_clone()?;
    let mut r = BufReader::new(conn);
    loop {
        let mut line = String::new();
        let n = (&mut r).take(MAX_LINE as u64 + 1).read_line(&mut line)?;
        if n == 0 {
            return Ok(());
        }
        if !line.ends_with('\n') {
            out.write_all(b"error line too long\n")?;
            return Ok(());
        }
        let answer = request(state, line.trim_end_matches(['\n', '\r']));
        if answer.ends_with("error wrong token\n") {
            std::thread::sleep(Duration::from_secs(1));
        }
        out.write_all(answer.as_bytes())?;
    }
}

/// The answer to one request, every line of it.
pub fn request(state: &State, line: &str) -> String {
    let words: Vec<&str> = line.split(' ').collect();
    match answer(state, &words) {
        Ok(a) => a,
        Err(e) => format!("error {}\n", e.to_string().replace('\n', " ")),
    }
}

fn answer(state: &State, words: &[&str]) -> io::Result<String> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidInput, what.to_string());
    match words {
        ["paired"] => {
            let mut out = String::new();
            for (name, key) in state.pairs()? {
                out.push_str(&format!("paired {name} {key}"));
                for (k, v) in state.config(&name) {
                    out.push_str(&format!(" {k}={v}"));
                }
                out.push('\n');
            }
            Ok(out + "ok\n")
        }
        ["pending"] => {
            let mut out = String::new();
            for p in state.pending()? {
                out.push_str(&format!("pending {} {} {} {}\n", p.code, p.name, p.addr, hex(&p.key)));
            }
            Ok(out + "ok\n")
        }
        ["approve", code, token] => {
            if !state.is_owner_token(token) {
                eprintln!("neochumbyd: admin: approving {code}: wrong token");
                return Err(bad("wrong token"));
            }
            let name = state.approve(code)?;
            eprintln!("neochumbyd: admin: approved {name} ({code})");
            Ok(format!("ok {name}\n"))
        }
        ["check", token] => {
            if !state.is_owner_token(token) {
                return Err(bad("wrong token"));
            }
            Ok("ok\n".into())
        }
        ["refuse", code] => {
            state.refuse(code)?;
            eprintln!("neochumbyd: admin: refused {code}");
            Ok("ok\n".into())
        }
        ["forget", name, token] => {
            if !state.is_owner_token(token) {
                eprintln!("neochumbyd: admin: forgetting {name}: wrong token");
                return Err(bad("wrong token"));
            }
            state.forget(name)?;
            eprintln!("neochumbyd: admin: forgot {name}");
            Ok("ok\n".into())
        }
        ["set", name, setting] => {
            let (key, value) = setting.split_once('=').ok_or_else(|| bad("set NAME KEY=VALUE"))?;
            state.set(name, key, value)?;
            eprintln!("neochumbyd: admin: {name}: {key}={value}");
            Ok("ok\n".into())
        }
        [cmd, ..] if ["paired", "pending", "approve", "check", "refuse", "forget", "set"].contains(cmd) => {
            Err(bad(&format!("{cmd}: wrong number of words")))
        }
        _ => Err(bad("unknown command")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neochumby_proto::Identity;

    /// The setting that names a Chumby's home screen. Proposal 0002 may
    /// rename it; the tests and docs use it only through this.
    const HOME: &str = "home";

    fn temp_state(what: &str) -> (std::path::PathBuf, State) {
        let dir = std::env::temp_dir().join(format!("neochumbyd-admin-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (dir.clone(), State::open(dir).unwrap())
    }

    #[test]
    fn requests() {
        let (dir, st) = temp_state("requests");
        let token = st.owner_token().unwrap();
        let key = Identity::generate().unwrap().public();
        st.request("123456", "kitchen", &key, "192.0.2.7:4000".parse().unwrap()).unwrap();

        assert_eq!(request(&st, "paired"), "ok\n");
        assert_eq!(request(&st, "pending"), format!("pending 123456 kitchen 192.0.2.7 {}\nok\n", hex(&key)));
        assert_eq!(request(&st, "approve 123456 nope"), "error wrong token\n");
        assert_eq!(request(&st, "approve 123456"), "error approve: wrong number of words\n");
        assert_eq!(request(&st, "check nope"), "error wrong token\n");
        assert_eq!(request(&st, &format!("check {token}")), "ok\n");
        assert_eq!(request(&st, &format!("approve 123456 {token}")), "ok kitchen\n");
        assert_eq!(request(&st, &format!("approve 123456 {token}")), "error no such request\n");
        assert_eq!(request(&st, "pending"), "ok\n");

        assert_eq!(request(&st, &format!("set kitchen {HOME}=wx")), "ok\n");
        assert_eq!(request(&st, "set kitchen delivery=sync"), "ok\n");
        assert_eq!(request(&st, "paired"), format!("paired kitchen {} {HOME}=wx delivery=sync\nok\n", hex(&key)));
        assert_eq!(request(&st, "set kitchen delivery="), "ok\n");
        assert_eq!(request(&st, &format!("set kitchen {HOME}")), "error set NAME KEY=VALUE\n");
        assert!(request(&st, &format!("set kitchen {HOME}=a/../b c")).starts_with("error"));
        assert!(request(&st, &format!("set lounge {HOME}=wx")).starts_with("error no Chumby"));

        st.request("654321", "lounge", &key, "192.0.2.8:4000".parse().unwrap()).unwrap();
        assert_eq!(request(&st, "refuse 654321"), "ok\n");
        assert_eq!(request(&st, "refuse 12"), "error not a code\n");
        assert_eq!(request(&st, "pending"), "ok\n");

        assert_eq!(request(&st, "forget kitchen"), "error forget: wrong number of words\n");
        assert_eq!(request(&st, "forget kitchen nope"), "error wrong token\n");
        assert!(request(&st, "paired").starts_with("paired kitchen "), "still paired");
        assert_eq!(request(&st, &format!("forget kitchen {token}")), "ok\n");
        assert_eq!(request(&st, "paired"), "ok\n");
        assert_eq!(request(&st, ""), "error unknown command\n");
        assert_eq!(request(&st, "rm -rf /"), "error unknown command\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn socket() {
        let (dir, st) = temp_state("socket");
        let path = dir.join("admin.sock");
        let l = listen(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::thread::spawn(move || serve(l, st));
        assert!(listen(&path).is_err(), "one neochumbyd per socket");

        // Our own user is let in, and may send several requests.
        let c = UnixStream::connect(&path).unwrap();
        assert!(allowed(peer_uid(&c).unwrap()));
        let mut r = BufReader::new(c.try_clone().unwrap());
        let mut w = c;
        let mut line = String::new();
        w.write_all(b"paired\npending\nfrobnicate\n").unwrap();
        for want in ["ok\n", "ok\n", "error unknown command\n"] {
            line.clear();
            r.read_line(&mut line).unwrap();
            assert_eq!(line, want);
        }
        // A line with no end is cut off.
        w.write_all(&[b'x'; MAX_LINE + 10]).unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line, "error line too long\n");

        // A socket left behind by a neochumbyd that died is replaced.
        let stale = dir.join("stale.sock");
        drop(UnixListener::bind(&stale).unwrap());
        assert!(listen(&stale).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
