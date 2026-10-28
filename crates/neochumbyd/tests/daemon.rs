//! neochumbyd end to end, on loopback, with an app's daemon: the daemon
//! approves a Chumby's pairing and sets its home screen through the admin
//! socket, as the household web page would.
//!
//! The daemon is this test's own binary, copied into the app's directory:
//! run with NEOCHUMBY_ADMIN set, it plays the daemon instead. Its first run
//! does the work and exits, so neochumbyd starts it again; the second stays
//! until neochumbyd stops it.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use neochumby_proto::{Identity, Msg, Session};
use neochumby_sys::process::{self, Signal};

/// The setting that names the app a Chumby starts with. This test uses it
/// only through this, so a rename is one line.
const START: &str = "start";

fn main() -> ExitCode {
    if std::env::var_os("NEOCHUMBY_ADMIN").is_some() {
        return as_daemon();
    }
    end_to_end();
    println!("neochumbyd daemon end to end: ok");
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

/// A connection to the admin socket, one request at a time.
struct Admin(BufReader<UnixStream>);

impl Admin {
    fn connect(path: &Path) -> Admin {
        Admin(BufReader::new(UnixStream::connect(path).expect("admin socket")))
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

/// The daemon's half: the web page's part in pairing.
fn as_daemon() -> ExitCode {
    let home = PathBuf::from(std::env::var_os("NEOCHUMBY_STATE").expect("NEOCHUMBY_STATE"));
    let runs = std::fs::read_to_string(home.join("runs")).unwrap_or_default() + "x";
    std::fs::write(home.join("runs"), &runs).unwrap();
    if runs.len() > 1 {
        // Started again: stay, until neochumbyd stops us.
        std::fs::write(home.join("pid"), std::process::id().to_string()).unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    let result = std::panic::catch_unwind(|| {
        let here = std::env::current_dir().unwrap();
        assert_eq!(here.canonicalize().unwrap(), home.canonicalize().unwrap(), "runs in its state directory");
        let tree = PathBuf::from(std::env::var_os("NEOCHUMBY_TREE").expect("NEOCHUMBY_TREE"));
        assert!(tree.join("apps/web/manifest").exists());
        let mut admin = Admin::connect(Path::new(&std::env::var_os("NEOCHUMBY_ADMIN").unwrap()));

        // Someone asks to pair; the page lists the request...
        let pending = wait_for(20, "a pairing request", || {
            let a = admin.ask("pending");
            (a.len() > 1).then(|| a[0].clone())
        });
        let w: Vec<&str> = pending.split(' ').collect();
        assert_eq!((w[0], w[2], w[3]), ("pending", "kitchen", "127.0.0.1"), "{pending}");
        let code = w[1];

        // ...and approves it only with the owner's token, which the owner
        // types into the page.
        assert_eq!(admin.ask(&format!("approve {code} 0123")), ["error wrong token"]);
        let token = wait_for(20, "the owner's token", || std::fs::read_to_string(home.join("typed")).ok());
        assert_eq!(admin.ask(&format!("check {token}")), ["ok"]);
        assert_eq!(admin.ask(&format!("approve {code} {token}")), ["ok kitchen"]);
        assert_eq!(admin.ask(&format!("set kitchen {START}=wx")), ["ok"]);
        let paired = admin.ask("paired");
        assert_eq!(paired.len(), 2);
        assert!(paired[0].starts_with("paired kitchen ") && paired[0].ends_with(&format!(" {START}=wx")), "{paired:?}");
    });
    match result {
        Ok(()) => {
            std::fs::write(home.join("done"), "").unwrap();
            ExitCode::SUCCESS
        }
        Err(e) => {
            let msg = e.downcast_ref::<String>().cloned().or(e.downcast_ref::<&str>().map(|s| s.to_string()));
            std::fs::write(home.join("failed"), msg.unwrap_or_default()).unwrap();
            ExitCode::FAILURE
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// neochumbyd, stopped and its files removed even if the test fails:
/// SIGTERM first, so that it stops its daemon too.
struct Daemon {
    child: std::process::Child,
    dir: PathBuf,
}

impl Drop for Daemon {
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

fn end_to_end() {
    let dir = std::env::temp_dir().join(format!("neochumbyd-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (state, tree) = (dir.join("state"), dir.join("tree"));
    let app = tree.join("apps/web");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("manifest"), "daemon=web-daemon\n").unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), app.join("web-daemon")).unwrap();

    let api = format!("127.0.0.1:{}", free_port());
    let nfs = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_neochumbyd"))
        .args(["serve", "--state", state.to_str().unwrap(), "--tree", tree.to_str().unwrap()])
        .args(["--api", &api, "--nfs", &nfs])
        .spawn()
        .unwrap();
    let mut d = Daemon { child, dir: dir.clone() };
    let home = state.join("apps/web");
    let socket = state.join("admin.sock");
    wait_for(20, "the admin socket", || socket.exists().then_some(()));
    let token = std::fs::read_to_string(state.join("owner-token")).unwrap().trim().to_string();

    // A Chumby asks to pair, and waits for the answer.
    let chumby = Identity::generate().unwrap();
    let tcp = wait_for(20, "the API", || TcpStream::connect(&api).ok());
    // Not the ten minutes neochumbyd gives a request, if the daemon fails.
    tcp.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut s = Session::connect(tcp, &chumby).unwrap();
    s.send(&Msg::Pair { name: "kitchen".into() }).unwrap();
    // The owner, having read the code on both screens, gives the page the
    // token.
    std::fs::write(home.join("typed"), &token).unwrap();
    match s.recv().unwrap() {
        Msg::Paired { nfs_port, .. } => assert_eq!(nfs_port.to_string(), nfs.rsplit(':').next().unwrap()),
        m => panic!("not paired: {m:?}"),
    }
    drop(s);
    wait_for(20, "the daemon", || {
        if let Ok(why) = std::fs::read_to_string(home.join("failed")) {
            panic!("the daemon failed: {why}");
        }
        home.join("done").exists().then_some(())
    });

    // The Chumby, paired, gets the home screen the page chose.
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

    // The first run exited; neochumbyd started it again.
    let pid: u32 = wait_for(20, "the daemon to start again", || {
        std::fs::read_to_string(home.join("pid")).ok().and_then(|p| p.trim().parse().ok())
    });
    let mut admin = Admin::connect(&socket);
    assert_eq!(admin.ask("forget kitchen 0123"), ["error wrong token"]);
    assert_eq!(admin.ask(&format!("forget kitchen {token}")), ["ok"]);
    assert_eq!(admin.ask("paired"), ["ok"]);
    drop(admin);

    // Stopping neochumbyd stops the daemon, and removes the socket.
    process::kill(d.child.id(), Signal::Term).unwrap();
    let status = wait_for(20, "neochumbyd to stop", || d.child.try_wait().unwrap());
    assert!(status.success(), "{status}");
    assert!(!process::alive(pid), "the daemon is still running");
    assert!(!socket.exists(), "the admin socket is still there");
}
