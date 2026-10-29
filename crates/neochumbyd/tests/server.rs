//! neochumbyd end to end, on loopback, with apps' server programs. One,
//! `web`, is allowed `pairing` by the owner: it approves a Chumby's pairing
//! and sets its start app through the admin socket, as the household web
//! page would. The other, `plain`, isn't, and gets no socket.
//!
//! Both are this test's own binary, copied into the apps' directories: run
//! with NEOCHUMBY_STATE set, it plays the app that directory names. Where
//! this system has a sandbox they run in it, and check that they can't
//! read neochumbyd's key or the owner's token, or write the tree. `web`'s
//! first run does the pairing and exits, so neochumbyd starts it again;
//! the second forgets the Chumby and stays until neochumbyd stops it.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use neochumby_proto::{Identity, Msg, Session};
use neochumby_sys::process::{self, Signal};

/// The setting that names the app a Chumby starts with. This test uses it
/// only through this, so a rename is one line.
const START: &str = "start";

fn main() -> ExitCode {
    if let Some(home) = std::env::var_os("NEOCHUMBY_STATE") {
        let home = PathBuf::from(home);
        let result = std::panic::catch_unwind(|| match home.file_name().and_then(|n| n.to_str()) {
            Some("web") => web(&home),
            Some("plain") => plain(&home),
            other => panic!("no such app: {other:?}"),
        });
        let msg = match result {
            Ok(()) => return ExitCode::SUCCESS,
            Err(e) => e.downcast_ref::<String>().cloned().or(e.downcast_ref::<&str>().map(|s| s.to_string())),
        };
        let _ = std::fs::write(home.join("failed"), msg.unwrap_or_default());
        return ExitCode::FAILURE;
    }
    end_to_end();
    println!("neochumbyd server end to end: ok");
    ExitCode::SUCCESS
}

/// Waits up to `secs` for `f` to give something.
fn wait_for<T>(secs: u64, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(t) = f() {
            return t;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The admin socket, one request at a time.
struct Admin(BufReader<UnixStream>);

impl Admin {
    /// The one this program was given, as descriptor 3.
    fn inherited() -> Admin {
        assert_eq!(std::env::var("NEOCHUMBY_ADMIN").as_deref(), Ok("3"));
        let fd = neochumby_sys::fd::handed_over(3).expect("descriptor 3");
        Admin(BufReader::new(UnixStream::from(fd)))
    }

    /// Every line of the answer, the last being `ok...` or `error...`.
    fn ask(&mut self, request: &str) -> Vec<String> {
        self.0.get_mut().write_all(format!("{request}\n").as_bytes()).unwrap();
        let mut lines = Vec::new();
        loop {
            let mut l = String::new();
            assert!(self.0.read_line(&mut l).unwrap() > 0, "socket closed");
            let l = l.trim_end().to_string();
            let last = l == "ok" || l.starts_with("ok ") || l.starts_with("error");
            lines.push(l);
            if last {
                return lines;
            }
        }
    }
}

/// What any app's program here should find: its directory, its HOME and
/// TMPDIR, only the environment it's given, and, sandboxed, no way to
/// neochumbyd's secrets or into the tree.
fn checks(home: &PathBuf) {
    assert_eq!(&std::env::current_dir().unwrap(), home, "runs in its own directory");
    assert_eq!(std::env::var_os("HOME").map(PathBuf::from).as_ref(), Some(home));
    assert_eq!(std::env::var_os("TMPDIR").map(PathBuf::from), Some(home.join("tmp")));
    assert!(std::env::var_os("SECRET_IN_NEOCHUMBYDS_SHELL").is_none(), "the environment wasn't cleared");
    assert_eq!(std::env::var("NEOCHUMBY_TEST_PASSED").as_deref(), Ok("yes"), "a NEOCHUMBY_ variable");
    let tree = PathBuf::from(std::env::var_os("NEOCHUMBY_TREE").expect("NEOCHUMBY_TREE"));
    assert!(tree.join("apps/web/manifest").exists(), "reads the tree");
    std::fs::write(home.join("tmp/scratch"), "x").expect("writes its own directory");
    if std::env::var("NEOCHUMBY_TEST_SANDBOX").as_deref() == Ok("1") {
        let state = home.join("../..");
        for secret in ["identity", "owner-token"] {
            assert!(std::fs::read(state.join(secret)).is_err(), "read {secret}");
            assert!(std::fs::OpenOptions::new().append(true).open(state.join(secret)).is_err(), "opened {secret}");
        }
        assert!(std::fs::write(tree.join("apps/web/manifest"), "x").is_err(), "wrote the tree");
        assert!(std::fs::write(state.join("paired/x"), "x").is_err(), "wrote the pairings");
    }
    if let Ok(p) = std::env::var("NEOCHUMBY_TEST_UNLISTED_PORT") {
        assert!(TcpListener::bind(format!("127.0.0.1:{p}")).is_err(), "bound a port it didn't list");
    }
}

/// The web page's part: pairing, then settings, then forgetting.
fn web(home: &PathBuf) {
    let runs = std::fs::read_to_string(home.join("runs")).unwrap_or_default() + "x";
    std::fs::write(home.join("runs"), &runs).unwrap();
    checks(home);
    let mut admin = Admin::inherited();
    if runs.len() > 1 {
        // Started again: forget the Chumby when told, then stay, until
        // neochumbyd stops us.
        std::fs::write(home.join("pid"), std::process::id().to_string()).unwrap();
        wait_for(30, "the word to forget", || home.join("forget-now").exists().then_some(()));
        let token = std::fs::read_to_string(home.join("typed")).unwrap();
        assert_eq!(admin.ask("forget kitchen 0123"), ["error wrong token"]);
        assert_eq!(admin.ask(&format!("forget kitchen {token}")), ["ok"]);
        assert_eq!(admin.ask("paired"), ["ok"]);
        std::fs::write(home.join("forgot"), "").unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    let port = std::env::var("NEOCHUMBY_TEST_LISTED_PORT").unwrap();
    drop(TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind the port it listed"));

    // Someone asks to pair; the page lists the request...
    let pending = wait_for(20, "a pairing request", || {
        let a = admin.ask("pending");
        (a.len() > 1).then(|| a[0].clone())
    });
    let w: Vec<&str> = pending.split(' ').collect();
    assert_eq!((w[0], w[2], w[3]), ("pending", "kitchen", "127.0.0.1"), "{pending}");
    let code = w[1];

    // ...and approves it only with the owner's token, which the owner types
    // into the page. Settings need it too.
    assert_eq!(admin.ask(&format!("approve {code} 0123")), ["error wrong token"]);
    let token = wait_for(20, "the owner's token", || std::fs::read_to_string(home.join("typed")).ok());
    assert_eq!(admin.ask(&format!("check {token}")), ["ok"]);
    assert_eq!(admin.ask(&format!("approve {code} {token}")), ["ok kitchen"]);
    assert_eq!(admin.ask(&format!("set kitchen {START}=wx 0123")), ["error wrong token"]);
    assert_eq!(admin.ask(&format!("set kitchen {START}=wx {token}")), ["ok"]);
    let paired = admin.ask("paired");
    assert_eq!(paired.len(), 2);
    assert!(paired[0].starts_with("paired kitchen ") && paired[0].ends_with(&format!(" {START}=wx")), "{paired:?}");
    std::fs::write(home.join("done"), "").unwrap();
}

/// An app the owner hasn't allowed `pairing`: no admin socket, by name or
/// by descriptor, and none to find by path.
fn plain(home: &PathBuf) {
    checks(home);
    assert!(std::env::var_os("NEOCHUMBY_ADMIN").is_none());
    assert!(neochumby_sys::fd::handed_over(3).is_none(), "descriptor 3 is open");
    std::fs::write(home.join("done"), "").unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// neochumbyd, stopped and its files removed even if the test fails:
/// SIGTERM first, so that it stops the server programs too.
struct Neochumbyd {
    child: std::process::Child,
    dir: PathBuf,
}

impl Drop for Neochumbyd {
    fn drop(&mut self) {
        let _ = process::kill(self.child.id(), Signal::Term);
        let deadline = Instant::now() + Duration::from_secs(10);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Waits for an app's program to say it's done, or why it failed.
fn done(home: &std::path::Path, file: &str) {
    wait_for(30, &format!("{} to finish", home.display()), || {
        if let Ok(why) = std::fs::read_to_string(home.join("failed")) {
            panic!("{} failed: {why}", home.display());
        }
        home.join(file).exists().then_some(())
    });
}

fn end_to_end() {
    let dir = std::env::temp_dir().join(format!("neochumbyd-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (state, tree) = (dir.join("state"), dir.join("tree"));
    let listed = free_port();
    for (app, manifest) in
        [("web", format!("server=web-server\nlisten={listed}\n")), ("plain", "server=plain-server\n".into())]
    {
        let d = tree.join("apps").join(app);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("manifest"), manifest).unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), d.join(format!("{app}-server"))).unwrap();
    }

    let sandbox = neochumby_sys::sandbox::detect();
    match &sandbox {
        Ok(s) => eprintln!("sandbox: {}", s.describe()),
        Err(why) if std::env::var_os("CI").is_some() && cfg!(target_os = "linux") => panic!("{why}"),
        Err(why) => eprintln!("no sandbox, so unsandboxed: {why}"),
    }
    // The owner allows the web page pairing.
    let ok = Command::new(env!("CARGO_BIN_EXE_neochumbyd"))
        .args(["allow", "web", "pairing", "--state", state.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(ok.success());
    let api = format!("127.0.0.1:{}", free_port());
    let nfs = format!("127.0.0.1:{}", free_port());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_neochumbyd"));
    cmd.args(["serve", "--state", state.to_str().unwrap(), "--tree", tree.to_str().unwrap()])
        .args(["--api", &api, "--nfs", &nfs])
        .env("SECRET_IN_NEOCHUMBYDS_SHELL", "x")
        .env("NEOCHUMBY_TEST_PASSED", "yes")
        .env("NEOCHUMBY_TEST_LISTED_PORT", listed.to_string())
        .env("NEOCHUMBY_TEST_SANDBOX", if sandbox.is_ok() { "1" } else { "0" });
    if sandbox.is_err() {
        cmd.arg("--unsandboxed");
    }
    if let Ok(neochumby_sys::sandbox::Sandbox::Landlock(abi)) = sandbox
        && abi >= 4
    {
        cmd.env("NEOCHUMBY_TEST_UNLISTED_PORT", free_port().to_string());
    }
    let mut d = Neochumbyd { child: cmd.spawn().unwrap(), dir: dir.clone() };
    let home = state.join("apps/web");
    wait_for(20, "the owner's token", || state.join("owner-token").exists().then_some(()));
    let token = std::fs::read_to_string(state.join("owner-token")).unwrap().trim().to_string();

    // A Chumby asks to pair, and waits for the answer.
    let chumby = Identity::generate().unwrap();
    let tcp = wait_for(20, "the API", || TcpStream::connect(&api).ok());
    // Not the ten minutes neochumbyd gives a request, if the page fails.
    tcp.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut s = Session::connect(tcp, &chumby).unwrap();
    s.send(&Msg::Pair { name: "kitchen".into() }).unwrap();
    // The owner, having read the code on both screens, gives the page the
    // token.
    wait_for(20, "the web page", || home.exists().then_some(()));
    std::fs::write(home.join("typed"), &token).unwrap();
    match s.recv() {
        Ok(Msg::Paired { nfs_port, .. }) => assert_eq!(nfs_port.to_string(), nfs.rsplit(':').next().unwrap()),
        r => {
            done(&home, "done");
            panic!("not paired: {r:?}");
        }
    }
    drop(s);
    done(&home, "done");
    done(&state.join("apps/plain"), "done");
    // Apps find neochumbyd's admin protocol by no path.
    assert!(!state.join("admin.sock").exists());

    // The Chumby, paired, gets the start app the page chose.
    let mut s = Session::connect(TcpStream::connect(&api).unwrap(), &chumby).unwrap();
    s.send(&Msg::Hello { name: "kitchen".into() }).unwrap();
    assert_eq!(s.recv().unwrap(), Msg::Welcome);
    assert_eq!(s.recv().unwrap(), Msg::Config(vec![(START.into(), "wx".into())]));
    drop(s);

    // The command line sees the same.
    let out = Command::new(env!("CARGO_BIN_EXE_neochumbyd"))
        .args(["pairing", "--state", state.to_str().unwrap()])
        .output()
        .unwrap();
    let out = String::from_utf8(out.stdout).unwrap();
    let first = out.lines().next().unwrap_or_default();
    assert!(first.starts_with("paired   kitchen  ") && first.ends_with(&format!("{START}=wx")), "{out}");

    // The first run exited; neochumbyd started it again, with a socket of
    // its own, and it forgets the Chumby.
    let pid: u32 = wait_for(20, "the web page to start again", || {
        std::fs::read_to_string(home.join("pid")).ok().and_then(|p| p.trim().parse().ok())
    });
    std::fs::write(home.join("forget-now"), "").unwrap();
    done(&home, "forgot");

    // Stopping neochumbyd stops the server programs.
    process::kill(d.child.id(), Signal::Term).unwrap();
    let status = wait_for(20, "neochumbyd to stop", || d.child.try_wait().unwrap());
    assert!(status.success(), "{status}");
    assert!(!process::alive(pid), "the web page is still running");
}
