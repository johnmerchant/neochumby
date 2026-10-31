//! The admin socket: how an app's server program, such as the household
//! web page, pairs Chumbys and sets their settings. It does what the
//! command line does, through the same functions (state.rs).
//!
//! It has no path, so no program can find it. Only an app whose owner
//! allows it `pairing` (`neochumbyd allow <app> pairing`) gets one: a
//! connected Unix socket, made for each run of its server program and
//! inherited by it as descriptor 3 (`NEOCHUMBY_ADMIN=3`), with neochumbyd's
//! end answered by [`serve`] until the program closes it, or the owner
//! disallows it (`neochumbyd disallow <app> pairing`). The program should
//! mark descriptor 3 close-on-exec as it starts, so that programs it runs
//! don't get it. The command line's pairing commands work on the state
//! directory itself; its catalog commands (`install`, `uninstall`,
//! `catalog auto-update`) speak this protocol with the owner's token, over
//! a connected pair of its own ([`local`]).
//!
//! The protocol is lines of text. A request is a command and its words,
//! separated by spaces, ending in a newline, at most 1024 bytes. The answer
//! is any number of lines of data, then one line that is `ok`, perhaps with
//! a word, or `error` and a message. A connection takes any number of
//! requests, one after another. No value holds a space: names, codes, keys,
//! addresses and settings are all single words.
//!
//!   paired                    paired NAME KEY [K=V ...]   each Chumby
//!                             ok                          paired, and its
//!                                                         settings
//!   pending                   pending CODE NAME ADDRESS KEY
//!                             ok                          each asking
//!   approve CODE TOKEN        ok NAME     pairs the Chumby showing CODE
//!   replace CODE TOKEN        ok NAME     the same, unpairing one already
//!                                         paired under its name with
//!                                         another key (`approve --replace`)
//!   refuse CODE TOKEN         ok
//!   forget NAME TOKEN         ok          unpairs NAME
//!   set NAME KEY=VALUE TOKEN  ok          one of a Chumby's settings;
//!                                         KEY= removes it
//!   check TOKEN               ok          whether TOKEN is the owner's,
//!                                         for the page's own login
//!   requests                  request CODE CHUMBY WHAT...
//!                             ok          each Chumby's GET APPS asking
//!                                         to install or remove an app
//!   approve CODE TOKEN        summary ... a request's CODE (ABCD-EF23)
//!   refuse CODE TOKEN         ok CHUMBY   approves or refuses it
//!   install SPEC ASKS TOKEN   summary ... installs or updates the catalog
//!                             ok APP      app SPEC (crate:NAME@VERSION),
//!                                         checked as `neochumbyd install`
//!                                         does, if it asks for exactly
//!                                         ASKS: its access words and its
//!                                         companion's hosts, joined by
//!                                         commas (`none`); the summary's
//!                                         lines, then its directory
//!   uninstall APP TOKEN       ok          removes a catalog app
//!   auto-update               ok on|off   whether a catalog app updates
//!                                         by itself to a version asking
//!                                         for nothing new
//!   auto-update on|off TOKEN  ok
//!
//! TOKEN is the owner's (`neochumbyd token`). Every change needs it; only
//! reading (`paired`, `pending`, `requests`, `auto-update`) and `check`
//! don't. Typing the code from
//! the Chumby's screen is what pairing's security rests on, so the person
//! approving must be the owner, not just someone on the network who can
//! reach the web page; and settings include remote input (`dev=on`) and
//! where a Chumby's apps come from. So nothing depends on the page putting
//! a change behind its own login. A wrong token is answered after a
//! second, to slow guessing through the page.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use neochumby_proto::hex;

use crate::state::State;

/// The longest request line.
const MAX_LINE: usize = 1024;

/// The commands that change something, and so need the owner's token
/// (their last word), with how many words each takes.
const CHANGES: &[(&str, usize)] = &[
    ("approve", 3),
    ("replace", 3),
    ("refuse", 3),
    ("forget", 3),
    ("set", 4),
    ("install", 4),
    ("uninstall", 3),
    ("auto-update", 3),
];

/// Answers `app`'s requests on `conn` until it closes it, or its owner no
/// longer allows it `pairing`.
pub fn serve(state: &State, conn: UnixStream, app: &str) -> io::Result<()> {
    answer_on(
        state,
        conn,
        || state.allowed(app).iter().any(|w| w == crate::state::PAIRING),
        &format!("{app} isn't allowed pairing"),
    )
}

/// Sends `line` as the command line, over a connected pair whose other end
/// this process answers: the answer's data lines and the word after `ok`
/// (if any), or the error. The owner's token, if the request needs it, is
/// in `line`, as from anyone else.
pub fn local(state: &State, line: &str) -> Result<Vec<String>, String> {
    let (ours, theirs) = UnixStream::pair().map_err(|e| e.to_string())?;
    let st = state.clone();
    let server = std::thread::spawn(move || answer_on(&st, theirs, || true, ""));
    let mut w = ours.try_clone().map_err(|e| e.to_string())?;
    w.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for l in BufReader::new(ours).lines() {
        let l = l.map_err(|e| e.to_string())?;
        if let Some(e) = l.strip_prefix("error ") {
            return Err(e.to_string());
        }
        if l == "ok" || l.starts_with("ok ") {
            if let Some(word) = l.strip_prefix("ok ") {
                out.push(word.to_string());
            }
            // Closed, so that its answering ends.
            let _ = w.shutdown(std::net::Shutdown::Both);
            let _ = server.join();
            return Ok(out);
        }
        out.push(l);
    }
    Err("the admin protocol closed without an answer".into())
}

/// Answers requests on `conn` while `allowed()` (else says `refused` and
/// stops), until it closes.
fn answer_on(state: &State, conn: UnixStream, allowed: impl Fn() -> bool, refused: &str) -> io::Result<()> {
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
        if !allowed() {
            out.write_all(format!("error {refused}\n").as_bytes())?;
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
        ["check", token] => {
            if !state.is_owner_token(token) {
                return Err(bad("wrong token"));
            }
            Ok("ok\n".into())
        }
        ["requests"] => {
            let mut out = String::new();
            for r in crate::catalog::chumby::requests(state.dir()) {
                out.push_str(&format!("request {} {} {}\n", r.code, r.chumby, r.what.replace('\n', " ")));
            }
            Ok(out + "ok\n")
        }
        ["auto-update"] => Ok(format!("ok {}\n", if crate::catalog::auto_updates(state.dir()) { "on" } else { "off" })),
        [cmd, ..] if CHANGES.iter().any(|&(c, n)| c == *cmd && n != words.len()) => {
            Err(bad(&format!("{cmd}: wrong number of words")))
        }
        [cmd, .., token] if CHANGES.iter().any(|&(c, _)| c == *cmd) && !state.is_owner_token(token) => {
            eprintln!("neochumbyd: admin: {cmd}: wrong token");
            Err(bad("wrong token"))
        }
        [cmd @ ("approve" | "refuse"), code, _] if crate::catalog::chumby::parse_code(code).is_some() => {
            let yes = *cmd == "approve";
            let r = crate::catalog::chumby::decide(state.dir(), code, yes).map_err(io::Error::other)?;
            eprintln!(
                "neochumbyd: admin: {} {}: {} asked to {}",
                if yes { "approved" } else { "refused" },
                r.code,
                r.chumby,
                r.what
            );
            let summary: String = r.summary.lines().map(|l| format!("summary {}\n", l.trim_start())).collect();
            Ok(format!("{summary}ok {}\n", r.chumby))
        }
        [cmd @ ("approve" | "replace"), code, _] => {
            let name = state.approve(code, *cmd == "replace")?;
            eprintln!(
                "neochumbyd: admin: approved {name} ({code}{})",
                if *cmd == "replace" { ", replacing" } else { "" }
            );
            Ok(format!("ok {name}\n"))
        }
        ["refuse", code, _] => {
            state.refuse(code)?;
            eprintln!("neochumbyd: admin: refused {code}");
            Ok("ok\n".into())
        }
        ["forget", name, _] => {
            state.forget(name)?;
            eprintln!("neochumbyd: admin: forgot {name}");
            Ok("ok\n".into())
        }
        ["install", spec, asks, _] => {
            let mut out = String::new();
            let lines = crate::catalog::admin_install(state.dir(), spec, asks).map_err(io::Error::other)?;
            let (app, summary) = lines.split_last().ok_or_else(|| io::Error::other("nothing installed"))?;
            for l in summary {
                out.push_str(&format!("{l}\n"));
            }
            Ok(format!("{out}ok {app}\n"))
        }
        ["uninstall", app, _] => {
            crate::catalog::admin_uninstall(state.dir(), app).map_err(io::Error::other)?;
            Ok("ok\n".into())
        }
        ["auto-update", on @ ("on" | "off"), _] => {
            crate::catalog::set_auto_updates(state.dir(), *on == "on")?;
            Ok("ok\n".into())
        }
        ["auto-update", ..] => Err(bad("auto-update [on|off TOKEN]")),
        ["set", name, setting, _] => {
            let (key, value) = setting.split_once('=').ok_or_else(|| bad("set NAME KEY=VALUE TOKEN"))?;
            state.set(name, key, value)?;
            eprintln!("neochumbyd: admin: {name}: {key}={value}");
            Ok("ok\n".into())
        }
        ["paired" | "pending" | "requests" | "check", ..] => Err(bad(&format!("{}: wrong number of words", words[0]))),
        _ => Err(bad("unknown command")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neochumby_proto::Identity;

    use neochumby_proto::settings::key::{DELIVERY, DEV, START};

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
        let code = |n: u8| format!("{n:02x}00-0000-0000-0000-0000-0000-0000-0000");
        let (c1, c2, c3) = (code(1), code(2), code(3));
        st.request(&c1, "kitchen", &key, "192.0.2.7").unwrap();

        assert_eq!(request(&st, "paired"), "ok\n");
        assert_eq!(request(&st, "pending"), format!("pending {c1} kitchen 192.0.2.7 {}\nok\n", hex(&key)));
        assert_eq!(request(&st, &format!("approve {c1} nope")), "error wrong token\n");
        assert_eq!(request(&st, &format!("approve {c1}")), "error approve: wrong number of words\n");
        assert_eq!(request(&st, "check nope"), "error wrong token\n");
        assert_eq!(request(&st, &format!("check {token}")), "ok\n");
        assert_eq!(request(&st, &format!("approve {c1} {token}")), "ok kitchen\n");
        assert_eq!(request(&st, &format!("approve {c1} {token}")), "error no Chumby is asking with that code\n");
        assert_eq!(request(&st, "pending"), "ok\n");

        // Every change needs the token.
        assert_eq!(request(&st, &format!("set kitchen {START}=wx")), "error set: wrong number of words\n");
        assert_eq!(request(&st, &format!("set kitchen {START}=wx nope")), "error wrong token\n");
        assert_eq!(request(&st, &format!("set kitchen {DEV}=on nope")), "error wrong token\n");
        assert_eq!(request(&st, "paired"), format!("paired kitchen {}\nok\n", hex(&key)));
        assert_eq!(request(&st, &format!("set kitchen {START}=wx {token}")), "ok\n");
        assert_eq!(request(&st, &format!("set kitchen {DELIVERY}=sync {token}")), "ok\n");
        assert_eq!(request(&st, "paired"), format!("paired kitchen {} {START}=wx {DELIVERY}=sync\nok\n", hex(&key)));
        // Main's Settings::check, through State::set.
        assert!(request(&st, &format!("set kitchen {DELIVERY}=carrier-pigeon {token}")).starts_with("error"));
        assert!(
            request(&st, &format!("set kitchen {START}=No.App {token}"))
                .starts_with(&format!("error {START} names an app"))
        );
        assert_eq!(request(&st, &format!("set kitchen {DELIVERY}= {token}")), "ok\n");
        assert_eq!(request(&st, &format!("set kitchen {START} {token}")), "error set NAME KEY=VALUE TOKEN\n");
        assert!(request(&st, &format!("set kitchen {START}=a!b {token}")).starts_with("error"));
        assert!(request(&st, &format!("set lounge {START}=wx {token}")).starts_with("error no Chumby"));

        st.request(&c2, "lounge", &key, "192.0.2.8").unwrap();
        assert_eq!(request(&st, &format!("refuse {c2}")), "error refuse: wrong number of words\n");
        assert_eq!(request(&st, &format!("refuse {c2} nope")), "error wrong token\n");
        assert_eq!(request(&st, "pending").lines().count(), 2, "still asking");
        assert_eq!(request(&st, &format!("refuse {c2} {token}")), "ok\n");
        assert!(request(&st, &format!("refuse 12 {token}")).starts_with("error not a pairing code"));
        assert_eq!(request(&st, "pending"), "ok\n");

        assert_eq!(request(&st, "forget kitchen"), "error forget: wrong number of words\n");
        assert_eq!(request(&st, "forget kitchen nope"), "error wrong token\n");
        assert!(request(&st, "paired").starts_with("paired kitchen "), "still paired");
        // Another key under its name replaces it only with `replace`.
        let other = Identity::generate().unwrap().public();
        st.request(&c3, "kitchen", &other, "192.0.2.9").unwrap();
        assert!(request(&st, &format!("approve {c3} {token}")).starts_with("error kitchen is already paired"));
        assert_eq!(request(&st, &format!("replace {c3} nope")), "error wrong token\n");
        assert_eq!(request(&st, &format!("replace {c3} {token}")), "ok kitchen\n");
        assert_eq!(st.paired(&other).as_deref(), Some("kitchen"));
        assert_eq!(st.paired(&key), None);

        assert_eq!(request(&st, &format!("forget kitchen {token}")), "ok\n");
        assert_eq!(request(&st, "paired"), "ok\n");
        assert_eq!(request(&st, "paired x"), "error paired: wrong number of words\n");
        assert_eq!(request(&st, ""), "error unknown command\n");
        assert_eq!(request(&st, "rm -rf /"), "error unknown command\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Over a connected pair, as a server program gets it: several requests,
    /// and a line too long cut off.
    #[test]
    fn socket() {
        let (dir, st) = temp_state("socket");
        let (ours, theirs) = UnixStream::pair().unwrap();
        st.allow("web", crate::state::PAIRING, true).unwrap();
        let st2 = st.clone();
        std::thread::spawn(move || serve(&st2, ours, "web"));
        let mut r = BufReader::new(theirs.try_clone().unwrap());
        let mut w = theirs;
        let mut line = String::new();
        w.write_all(b"paired\npending\nfrobnicate\n").unwrap();
        for want in ["ok\n", "ok\n", "error unknown command\n"] {
            line.clear();
            r.read_line(&mut line).unwrap();
            assert_eq!(line, want);
        }
        w.write_all(&[b'x'; MAX_LINE + 10]).unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line, "error line too long\n");

        // Disallowed while connected: the next request ends it.
        let (ours, theirs) = UnixStream::pair().unwrap();
        let st2 = st.clone();
        std::thread::spawn(move || serve(&st2, ours, "web"));
        let mut r = BufReader::new(theirs.try_clone().unwrap());
        let mut w = theirs;
        st.allow("web", crate::state::PAIRING, false).unwrap();
        w.write_all(b"paired\n").unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line, "error web isn't allowed pairing\n");
        line.clear();
        assert_eq!(r.read_line(&mut line).unwrap(), 0, "closed");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
