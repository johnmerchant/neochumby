//! neochumbyd end to end, on loopback, with apps' server programs. A Chumby's
//! key is registered as a stick's is (`neochumbyd add`), and its settings set
//! on the command line. Two apps have server programs: `web` is started again
//! when it exits, and `plain` gets no admin socket or descriptor, as no
//! program does. A third, `echo`, has a companion: the Chumby opens it after
//! its settings, as every Chumby does, and it answers sandboxed too.
//!
//! All are this test's own binary, copied into the apps' directories: run
//! with NEOCHUMBY_STATE set, it plays the app that directory names. Where
//! this system has a sandbox they run in it, and check that they can't
//! read neochumbyd's key or the owner's token, or write the tree.

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use neochumby_proto::settings::key::START;
use neochumby_proto::{Identity, Msg, Session, hex};
use neochumby_sys::process::{self, Signal};

fn main() -> ExitCode {
    if let Some(home) = std::env::var_os("NEOCHUMBY_STATE") {
        let home = PathBuf::from(home);
        let result = std::panic::catch_unwind(|| match home.file_name().and_then(|n| n.to_str()) {
            Some("web") => web(&home),
            Some("plain") => plain(&home),
            Some("echo") => echo(&home),
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

/// What any app's program here should find: its directory, its HOME and
/// TMPDIR, only the environment it's given, and, sandboxed, no way to
/// neochumbyd's secrets or into the tree.
fn checks(home: &PathBuf, server: bool) {
    assert_eq!(&std::env::current_dir().unwrap(), home, "runs in its own directory");
    assert_eq!(std::env::var_os("HOME").map(PathBuf::from).as_ref(), Some(home));
    assert_eq!(std::env::var_os("TMPDIR").map(PathBuf::from), Some(home.join("tmp")));
    assert!(std::env::var_os("SECRET_IN_NEOCHUMBYDS_SHELL").is_none(), "the environment wasn't cleared");
    assert_eq!(std::env::var("NEOCHUMBY_TEST_PASSED").as_deref(), Ok("yes"), "a NEOCHUMBY_ variable");
    // A server program is told where the tree is; a companion isn't.
    let tree = if server { "NEOCHUMBY_TREE" } else { "NEOCHUMBY_TEST_TREE" };
    let tree = PathBuf::from(std::env::var_os(tree).expect(tree));
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
    if let Ok(p) = std::env::var("NEOCHUMBY_TEST_UNLISTED_PORT")
        && server
    {
        assert!(TcpListener::bind(format!("127.0.0.1:{p}")).is_err(), "bound a port it didn't list");
    }
}

/// A server program: its checks and its port, then it exits, so neochumbyd
/// starts it again; the second run stays until neochumbyd stops it.
fn web(home: &PathBuf) {
    let runs = std::fs::read_to_string(home.join("runs")).unwrap_or_default() + "x";
    std::fs::write(home.join("runs"), &runs).unwrap();
    checks(home, true);
    if runs.len() > 1 {
        std::fs::write(home.join("pid"), std::process::id().to_string()).unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    let port = std::env::var("NEOCHUMBY_TEST_LISTED_PORT").unwrap();
    drop(TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind the port it listed"));
    std::fs::write(home.join("done"), "").unwrap();
}

/// No admin socket, by name or by descriptor, and none to find by path.
fn plain(home: &PathBuf) {
    checks(home, true);
    assert!(std::env::var_os("NEOCHUMBY_ADMIN").is_none());
    assert!(neochumby_sys::fd::handed_over(3).is_none(), "descriptor 3 is open");
    std::fs::write(home.join("done"), "").unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// A companion: its checks, then each frame back with `echo:` before it.
fn echo(home: &PathBuf) {
    assert_eq!(std::env::var("NEOCHUMBY_CHUMBY").as_deref(), Ok("kitchen"));
    checks(home, false);
    let (mut stdin, mut stdout) = (std::io::stdin().lock(), std::io::stdout().lock());
    while let Some(f) = neochumby_proto::frame::read(&mut stdin).unwrap() {
        neochumby_proto::frame::write(&mut stdout, &[b"echo:".as_slice(), &f].concat()).unwrap();
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
    let d = tree.join("apps/echo");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("manifest"), "companion=echo-companion\nneochumbyd=required\n").unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), d.join("echo-companion")).unwrap();

    let sandbox = neochumby_sys::sandbox::detect();
    match &sandbox {
        Ok(s) => eprintln!("sandbox: {}", s.describe()),
        Err(why) if std::env::var_os("CI").is_some() && cfg!(target_os = "linux") => panic!("{why}"),
        Err(why) => eprintln!("no sandbox, so unsandboxed: {why}"),
    }
    let api = format!("127.0.0.1:{}", free_port());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_neochumbyd"));
    cmd.args(["serve", "--state", state.to_str().unwrap(), "--tree", tree.to_str().unwrap()])
        .args(["--api", &api])
        .env("SECRET_IN_NEOCHUMBYDS_SHELL", "x")
        .env("NEOCHUMBY_TEST_PASSED", "yes")
        .env("NEOCHUMBY_TEST_TREE", tree.canonicalize().unwrap())
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
    wait_for(20, "the owner's token", || state.join("owner-token").exists().then_some(()));

    let nd = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_neochumbyd"))
            .args(args)
            .args(["--state", state.to_str().unwrap()])
            .output()
            .unwrap();
        (out.status.success(), String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap())
    };
    let chumby = Identity::generate().unwrap();
    let key = hex(&chumby.public());

    // Not registered, it is turned away, and asking gets it nothing.
    wait_for(20, "the API", || TcpStream::connect(&api).ok());
    let (_, added, _) = nd(&["add", "kitchen", &key, "--address", "192.0.2.1", "--api", "0.0.0.0:7341"]);
    let mut lines = added.lines();
    assert_eq!(lines.next(), Some("server=192.0.2.1"));
    assert_eq!(lines.next(), Some("api_port=7341"));
    let server_key = neochumby_proto::unhex_key(lines.next().unwrap().strip_prefix("server_key=").unwrap()).unwrap();
    let stranger = Identity::generate().unwrap();
    let mut s = Session::connect(TcpStream::connect(&api).unwrap(), &stranger, &server_key).unwrap();
    s.send(&Msg::Hello { name: "kitchen".into() }).unwrap();
    assert_eq!(s.recv().unwrap(), Msg::Unpaired);
    drop(s);

    // Another key under its name only with --replace.
    let (ok, _, why) = nd(&["add", "kitchen", &hex(&stranger.public())]);
    assert!(!ok && why.contains("--replace"), "{why}");
    let (ok, _, why) = nd(&["set", "kitchen", &format!("{START}=wx")]);
    assert!(ok, "{why}");

    let home = state.join("apps/web");
    done(&home, "done");
    done(&state.join("apps/plain"), "done");
    // Apps find neochumbyd's admin protocol by no path.
    assert!(!state.join("admin.sock").exists());

    // The Chumby gets the start app the owner chose.
    let mut s = Session::connect(TcpStream::connect(&api).unwrap(), &chumby, &server_key).unwrap();
    s.send(&Msg::Hello { name: "kitchen".into() }).unwrap();
    assert_eq!(s.recv().unwrap(), Msg::Welcome);
    match s.recv().unwrap() {
        Msg::Config(pairs) => assert!(pairs.contains(&(START.into(), "wx".into())), "{pairs:?}"),
        m => panic!("not its settings: {m:?}"),
    }
    drop(s);

    // It opens a companion, after its settings, and the companion answers
    // from its sandbox (or says what failed there).
    let mut s = Session::connect(TcpStream::connect(&api).unwrap(), &chumby, &server_key).unwrap();
    s.send(&Msg::Hello { name: "kitchen".into() }).unwrap();
    assert_eq!(s.recv().unwrap(), Msg::Welcome);
    assert!(matches!(s.recv().unwrap(), Msg::Config(_)));
    s.send(&Msg::Open { app: "echo".into() }).unwrap();
    s.send(&Msg::Data(b"hi".to_vec())).unwrap();
    let got = wait_for(20, "the companion's answer", || match s.recv_timeout(Duration::from_millis(100)).unwrap() {
        Some(Msg::Data(d)) => Some(Ok(d)),
        Some(Msg::Closed) => {
            Some(Err(std::fs::read_to_string(state.join("apps/echo/failed")).unwrap_or("closed".into())))
        }
        _ => None,
    });
    assert_eq!(got, Ok(b"echo:hi".to_vec()));
    drop(s);

    // The command line sees the same.
    let (_, out, _) = nd(&["list"]);
    let first = out.lines().next().unwrap_or_default();
    assert!(first.starts_with("chumby   kitchen  ") && first.ends_with(&format!("{START}=wx")), "{out}");

    // Forgotten, it is turned away at once.
    assert!(nd(&["forget", "kitchen"]).0);
    let mut s = Session::connect(TcpStream::connect(&api).unwrap(), &chumby, &server_key).unwrap();
    s.send(&Msg::Hello { name: "kitchen".into() }).unwrap();
    assert_eq!(s.recv().unwrap(), Msg::Unpaired);
    drop(s);

    // The first run exited; neochumbyd started it again.
    let pid: u32 = wait_for(20, "the web page to start again", || {
        std::fs::read_to_string(home.join("pid")).ok().and_then(|p| p.trim().parse().ok())
    });

    // Stopping neochumbyd stops the server programs.
    process::kill(d.child.id(), Signal::Term).unwrap();
    let status = wait_for(20, "neochumbyd to stop", || d.child.try_wait().unwrap());
    assert!(status.success(), "{status}");
    assert!(!process::alive(pid), "the web page is still running");
}
