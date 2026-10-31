//! Wasm companions: a companion whose manifest names a `.wasm` file
//! (`companion=weather.wasm`) is run under Wasmtime (Cranelift), with WASI
//! preview 1, not executed. companion.rs starts it as any companion, but
//! the process is neochumbyd's own:
//!
//!   neochumbyd wasm-companion --home DIR --app NAME [--network HOSTS] FILE.wasm
//!
//! so the relay, the environment and the kill five seconds after stdin
//! closes are a native companion's, and a crash stays in that process. It
//! is given nothing but:
//!
//! - stdin and stdout, its frames from and to the app, and stderr, which
//!   is neochumbyd's log;
//! - its state directory (DIR, `<state>/apps/<app>`), preopened read-write
//!   as `.`: relative paths are in it, and `..`, absolute paths, and
//!   symlinks out of it fail. No other file;
//! - `NEOCHUMBY_CHUMBY`, and no other environment, nor any argument;
//! - no sockets, but [`https_get`] (`neochumby` `https_get`), to the hosts
//!   its owner approved (HOSTS, `network=host1,host2` in its manifest,
//!   written by the catalog's installer) and none other, and only where
//!   they resolve to public addresses (egress.rs): never the server
//!   itself or its network;
//! - [`MEMORY`] of linear memory: `memory.grow` past it fails in the
//!   module;
//! - [`BUSY`] of running without calling the host: a runaway loop traps.
//!   Time spent in a host call (waiting on stdin, say) doesn't count.
//!
//! A module that imports anything else is refused before it runs. A trap,
//! or `proc_exit`, ends the process; a trap is logged with its backtrace.
//!
//! The process itself runs as any companion does (companion.rs): in the
//! system's sandbox, and under the app's own uid with `--app-uids`. So
//! Wasmtime is the inner wall, not the only one.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::egress;

use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{Connector as _, RustlsConnector};
use wasmtime::{
    CallHook, Caller, Config, Engine, Extern, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline,
};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::{FsPerms, I32Exit, WasiCtxBuilder};

/// The most linear memory a companion may have.
pub const MEMORY: usize = 64 << 20;
/// How long a companion may run without calling the host.
pub const BUSY: Duration = Duration::from_secs(5);
/// How often the engine's epoch ticks, and so how often a companion's
/// time running is looked at.
const TICK: Duration = Duration::from_millis(100);
/// The most a response's body may be, whatever the buffer offered.
pub const MAX_BODY: usize = 1 << 20;
/// How long a GET may take to connect.
const CONNECT: Duration = Duration::from_secs(10);
/// How long a GET may take in all, its body included.
const GET: Duration = Duration::from_secs(30);

/// What [`https_get`] returns in place of a status.
pub mod get {
    /// A pointer or length outside the module's memory, no exported
    /// `memory`, or a URL that isn't UTF-8 or isn't `https://host...`.
    pub const BAD: i32 = -1;
    /// The URL's host isn't one the owner approved.
    pub const REFUSED: i32 = -2;
    /// Name lookup, connecting, TLS or HTTP failed, or a timeout.
    pub const FAILED: i32 = -3;
    /// The body is longer than the buffer, or than [`super::MAX_BODY`].
    pub const TOO_LARGE: i32 = -4;
}

/// What a companion is granted.
pub struct Grant {
    /// Its state directory, preopened as `.`.
    pub home: PathBuf,
    /// The Chumby's name, its `NEOCHUMBY_CHUMBY`.
    pub chumby: String,
    /// The hosts it may GET from, lowercase.
    pub hosts: Vec<String>,
    /// The roots its GETs' certificates must chain to: Mozilla's
    /// (webpki-roots), except in tests, which run their own server.
    pub roots: RootCerts,
    /// Which addresses its GETs may reach: public ones only, except that
    /// tests may reach their own server on 127.0.0.1.
    pub rule: egress::Rule,
    /// [`MEMORY`], except in tests.
    pub memory: usize,
    /// [`BUSY`], except in tests.
    pub busy: Duration,
}

impl Grant {
    /// A companion's grant: `network` is the manifest's comma-separated
    /// list.
    pub fn new(home: PathBuf, chumby: String, network: &str) -> Grant {
        Grant {
            home,
            chumby,
            hosts: hosts(network),
            roots: RootCerts::WebPki,
            rule: egress::Rule::default(),
            memory: MEMORY,
            busy: BUSY,
        }
    }
}

/// `network=`'s hosts, lowercase, without blanks.
pub fn hosts(network: &str) -> Vec<String> {
    network.split(',').map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect()
}

/// How a companion ended without trapping.
#[derive(Debug, PartialEq, Eq)]
pub enum End {
    /// `_start` returned.
    Returned,
    /// It called `proc_exit` with this.
    Exited(i32),
}

/// What a running companion's store holds.
struct Ctx {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
    /// When it last returned from an import, or started.
    since: Instant,
    /// Whether the host call under way is an import's: WASI's or
    /// [`https_get`], not a libcall such as `memory.grow`'s.
    import: bool,
    /// [`Grant::busy`].
    busy: Duration,
    /// [`Grant::hosts`].
    hosts: Vec<String>,
    /// What its GETs go through.
    agent: ureq::Agent,
}

/// The trap of a companion that ran [`Grant::busy`] without calling the
/// host.
#[derive(Debug)]
struct Runaway(Duration);

impl std::fmt::Display for Runaway {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "ran {:?} without calling the host", self.0)
    }
}

impl std::error::Error for Runaway {}

/// Ticks `engine`'s epoch until dropped.
struct Ticker(Arc<AtomicBool>);

impl Ticker {
    /// Starts ticking, every [`TICK`].
    fn start(engine: &Engine) -> Ticker {
        let stop = Arc::new(AtomicBool::new(false));
        let (engine, flag) = (engine.clone(), stop.clone());
        std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                engine.increment_epoch();
            }
        });
        Ticker(stop)
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Runs the module `wasm` with `grant` until it ends. `stdio` gives it its
/// stdin, stdout and stderr: the process's, or a test's pipes. A load
/// error, a refused import or a trap is the error.
pub fn run(wasm: &[u8], grant: &Grant, stdio: impl FnOnce(&mut WasiCtxBuilder)) -> wasmtime::Result<End> {
    let mut config = Config::new();
    config.epoch_interruption(true);
    config.wasm_component_model(false);
    let engine = Engine::new(&config)?;
    let module = Module::new(&engine, wasm)?;

    let mut linker: Linker<Ctx> = Linker::new(&engine);
    // Every WASI import reaches its context through this, and nothing else
    // does: so it says that the host call under way is an import's.
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |c: &mut Ctx| {
        c.import = true;
        &mut c.wasi
    })?;
    linker.func_wrap("neochumby", "https_get", https_get)?;

    let mut wasi = WasiCtxBuilder::new();
    stdio(&mut wasi);
    wasi.env("NEOCHUMBY_CHUMBY", &grant.chumby)
        .preopened_dir(&grant.home, ".", FsPerms::ReadWrite)?
        .allow_tcp(false)
        .allow_udp(false)
        .allow_ip_name_lookup(false);
    let limits = StoreLimitsBuilder::new()
        .memory_size(grant.memory)
        .table_elements(100_000)
        .instances(1)
        .memories(1)
        .tables(4)
        .build();
    let ctx = Ctx {
        wasi: wasi.build_p1(),
        limits,
        since: Instant::now(),
        import: false,
        busy: grant.busy,
        hosts: grant.hosts.clone(),
        agent: agent(grant.roots.clone(), grant.rule),
    };
    let mut store = Store::new(&engine, ctx);
    store.limiter(|c| &mut c.limits);
    // Its time running starts again whenever it comes back from an import:
    // wasm only checks the epoch as it runs, so the time an import took
    // (waiting on stdin, say) is never counted, however many ticks it was.
    // Libcalls are host calls too, but a loop of `memory.grow 0` mustn't
    // run forever, so they don't count as returning.
    store.call_hook(|mut s, hook| {
        let c = s.data_mut();
        match hook {
            CallHook::CallingWasm => c.since = Instant::now(),
            CallHook::ReturningFromHost if c.import => {
                c.since = Instant::now();
                c.import = false;
            }
            _ => {}
        }
        Ok(())
    });
    store.epoch_deadline_callback(|s| {
        let c = s.data();
        if c.since.elapsed() > c.busy { Err(Runaway(c.busy).into()) } else { Ok(UpdateDeadline::Continue(1)) }
    });
    store.set_epoch_deadline(1);
    let _ticker = Ticker::start(&engine);

    let instance = linker.instantiate(&mut store, &module)?;
    let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
    match start.call(&mut store, ()) {
        Ok(()) => Ok(End::Returned),
        Err(e) => match e.downcast_ref::<I32Exit>() {
            Some(I32Exit(n)) => Ok(End::Exited(*n)),
            None => Err(e),
        },
    }
}

/// The agent a companion's GETs go through: HTTPS only, through no proxy
/// (the environment's is neochumbyd's, not the companion's), following no
/// redirect (it could lead to a host not approved: the 3xx is the
/// companion's to follow), with timeouts, and only to public addresses,
/// checked as names are looked up and again once connected (egress.rs).
fn agent(roots: RootCerts, rule: egress::Rule) -> ureq::Agent {
    let tls = TlsConfig::builder().provider(TlsProvider::Rustls).root_certs(roots).build();
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT))
        .timeout_global(Some(GET))
        .tls_config(tls)
        .build();
    let connector = ().chain(egress::Connector { rule, timeout: CONNECT }).chain(RustlsConnector::default());
    let resolver = egress::Resolver { inner: DefaultResolver::default(), rule };
    ureq::Agent::with_parts(config, connector, resolver)
}

/// The host import a companion reaches the network by:
///
/// ```text
/// (import "neochumby" "https_get"
///   (func (param $url i32) (param $url_len i32)
///         (param $buf i32) (param $buf_len i32) (param $body_len i32)
///         (result i32)))
/// ```
///
/// GETs the UTF-8 `https://` URL at `url`, if its host is one approved,
/// and returns the response's status (redirects are not followed). Its
/// body goes into the `buf_len` bytes at `buf` and its length, a
/// little-endian u32, at `body_len`. Otherwise one of [`get`]'s errors,
/// and nothing is written.
fn https_get(mut caller: Caller<'_, Ctx>, url: i32, url_len: i32, buf: i32, buf_len: i32, body_len: i32) -> i32 {
    caller.data_mut().import = true;
    let Some(Extern::Memory(memory)) = caller.get_export("memory") else {
        return get::BAD;
    };
    let span = |p: i32, n: i32| Some((usize::try_from(p).ok()?, usize::try_from(n).ok()?));
    let (Some((url, url_len)), Some((buf, buf_len)), Some((body_len, _))) =
        (span(url, url_len), span(buf, buf_len), span(body_len, 0))
    else {
        return get::BAD;
    };
    let mut u = vec![0; url_len.min(8192)];
    if url_len > u.len() || memory.read(&caller, url, &mut u).is_err() {
        return get::BAD;
    }
    let Ok(u) = String::from_utf8(u) else {
        return get::BAD;
    };
    // The body must fit before anything is fetched: out of memory is BAD.
    if buf.checked_add(buf_len).is_none_or(|end| end > memory.data_size(&caller))
        || body_len.checked_add(4).is_none_or(|end| end > memory.data_size(&caller))
    {
        return get::BAD;
    }
    let (status, body) = match fetch(&caller.data().agent, &caller.data().hosts, &u, buf_len.min(MAX_BODY)) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let ok = memory.write(&mut caller, buf, &body).is_ok()
        && memory.write(&mut caller, body_len, &(body.len() as u32).to_le_bytes()).is_ok();
    if ok { i32::from(status) } else { get::BAD }
}

/// GETs `url` through `agent` if its host is one of `hosts`: its status
/// and body, the body at most `cap` bytes.
fn fetch(agent: &ureq::Agent, hosts: &[String], url: &str, cap: usize) -> Result<(u16, Vec<u8>), i32> {
    let uri: ureq::http::Uri = url.parse().map_err(|_| get::BAD)?;
    let authority = uri.authority().ok_or(get::BAD)?;
    // A user in the URL is refused rather than parsed past: the host is
    // what's after it, and the owner approved a host, not a credential.
    if uri.scheme_str() != Some("https") || authority.as_str().contains('@') {
        return Err(get::BAD);
    }
    let host = authority.host().to_ascii_lowercase();
    if !hosts.contains(&host) {
        return Err(get::REFUSED);
    }
    let mut resp = agent.get(uri).call().map_err(|_| get::FAILED)?;
    let status = resp.status().as_u16();
    match resp.body_mut().with_config().limit(cap as u64).read_to_vec() {
        Ok(body) => Ok((status, body)),
        Err(ureq::Error::BodyExceedsLimit(_)) => Err(get::TOO_LARGE),
        Err(_) => Err(get::FAILED),
    }
}

/// `neochumbyd wasm-companion`, given its arguments after the subcommand:
/// runs the module with the process's stdio and exits as it ends.
pub fn main(args: impl Iterator<Item = String>) -> ! {
    let usage = || -> ! {
        eprintln!("usage: neochumbyd wasm-companion --home DIR --app NAME [--network HOSTS] FILE.wasm");
        std::process::exit(2)
    };
    let (mut home, mut app, mut network, mut file) = (None, None, String::new(), None);
    let mut it = args;
    while let Some(arg) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--home" => home = Some(PathBuf::from(value())),
            "--app" => app = Some(value()),
            "--network" => network = value(),
            _ if arg.starts_with("--") || file.is_some() => usage(),
            _ => file = Some(PathBuf::from(arg)),
        }
    }
    let (Some(home), Some(app), Some(file)) = (home, app, file) else { usage() };
    let chumby = std::env::var("NEOCHUMBY_CHUMBY").unwrap_or_default();
    let who = format!("neochumbyd: {app} for {chumby}");
    let wasm = std::fs::read(&file).unwrap_or_else(|e| {
        eprintln!("{who}: {}: {e}", file.display());
        std::process::exit(1)
    });
    let grant = Grant::new(home, chumby, &network);
    let stdio = |w: &mut WasiCtxBuilder| {
        w.stdin(std::io::stdin()).stdout(std::io::stdout()).stderr(std::io::stderr());
    };
    match run(&wasm, &grant, stdio) {
        Ok(End::Returned) => std::process::exit(0),
        Ok(End::Exited(n)) => {
            if n != 0 {
                eprintln!("{who}: exited {n}");
            }
            std::process::exit(n)
        }
        Err(e) => {
            eprintln!("{who}: {e:?}");
            std::process::exit(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;
    use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};

    /// A fresh directory for one test's state.
    fn home(what: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("neochumbyd-wasm-{what}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        dir
    }

    fn grant(dir: &Path) -> Grant {
        Grant::new(dir.join("home"), "kitchen".into(), "")
    }

    /// Runs `wat` with `stdin`: how it ended, and what it wrote to stdout.
    fn go(wat: &str, grant: &Grant, stdin: &[u8]) -> (wasmtime::Result<End>, Vec<u8>) {
        let wasm = wat::parse_str(wat).unwrap();
        let out = MemoryOutputPipe::new(1 << 20);
        let (input, o) = (MemoryInputPipe::new(stdin.to_vec()), out.clone());
        let end = run(&wasm, grant, move |w| {
            w.stdin(input).stdout(o).inherit_stderr();
        });
        (end, out.contents().to_vec())
    }

    const WASI: &str = r#"
        (import "wasi_snapshot_preview1" "fd_read" (func $read (param i32 i32 i32 i32) (result i32)))
        (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
        (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
        (memory (export "memory") 1)
    "#;

    #[test]
    fn frames_echo_through() {
        // Copies stdin to stdout until it ends.
        let wat = format!(
            r#"(module {WASI}
                (func (export "_start")
                  (i32.store (i32.const 0) (i32.const 1024))
                  (i32.store (i32.const 4) (i32.const 4096))
                  (loop $l
                    (drop (call $read (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 16)))
                    (if (i32.eqz (i32.load (i32.const 16))) (then (return)))
                    (i32.store (i32.const 8) (i32.const 1024))
                    (i32.store (i32.const 12) (i32.load (i32.const 16)))
                    (drop (call $write (i32.const 1) (i32.const 8) (i32.const 1) (i32.const 20)))
                    (br $l))))"#
        );
        let mut frames = Vec::new();
        neochumby_proto::frame::write(&mut frames, b"hello").unwrap();
        neochumby_proto::frame::write(&mut frames, &[7; 3000]).unwrap();
        let dir = home("echo");
        let (end, out) = go(&wat, &grant(&dir), &frames);
        assert_eq!(end.unwrap(), End::Returned);
        let mut r = &out[..];
        assert_eq!(neochumby_proto::frame::read(&mut r).unwrap().unwrap(), b"hello");
        assert_eq!(neochumby_proto::frame::read(&mut r).unwrap().unwrap(), vec![7; 3000]);
        assert!(neochumby_proto::frame::read(&mut r).unwrap().is_none());
    }

    /// A module that creates `path` in its preopen (fd 3) and writes "hi"
    /// to it, exiting with path_open's errno.
    fn create(path: &str) -> String {
        format!(
            r#"(module
                (import "wasi_snapshot_preview1" "path_open" (func $open
                  (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
                {WASI}
                (data (i32.const 256) "{path}")
                (data (i32.const 128) "hi")
                (func (export "_start") (local $e i32)
                  (local.set $e (call $open (i32.const 3) (i32.const 0) (i32.const 256) (i32.const {len})
                    (i32.const 1) (i64.const 0x1fffffff) (i64.const 0x1fffffff) (i32.const 0) (i32.const 0)))
                  (if (local.get $e) (then (call $exit (local.get $e))))
                  (i32.store (i32.const 8) (i32.const 128))
                  (i32.store (i32.const 12) (i32.const 2))
                  (drop (call $write (i32.load (i32.const 0)) (i32.const 8) (i32.const 1) (i32.const 20)))
                  (call $exit (i32.const 0))))"#,
            len = path.len()
        )
    }

    #[test]
    fn writes_stay_in_its_state() {
        let dir = home("state");
        let (end, _) = go(&create("notes.txt"), &grant(&dir), b"");
        assert_eq!(end.unwrap(), End::Exited(0));
        assert_eq!(std::fs::read(dir.join("home/notes.txt")).unwrap(), b"hi");
    }

    #[test]
    fn nothing_outside_its_state() {
        let dir = home("escape");
        let outside = dir.join("outside");
        let absolute = outside.to_str().unwrap().to_string();
        for path in ["../outside", "a/../../outside", absolute.as_str(), "/tmp/outside"] {
            let (end, _) = go(&create(path), &grant(&dir), b"");
            assert!(matches!(end.unwrap(), End::Exited(n) if n != 0), "{path} opened");
        }
        assert!(!outside.exists());
    }

    #[test]
    fn no_args_and_only_its_chumby() {
        // Exits argc * 100 + the number of environment variables, then
        // writes the environment out.
        let wat = format!(
            r#"(module
                (import "wasi_snapshot_preview1" "args_sizes_get" (func $args (param i32 i32) (result i32)))
                (import "wasi_snapshot_preview1" "environ_sizes_get" (func $envs (param i32 i32) (result i32)))
                (import "wasi_snapshot_preview1" "environ_get" (func $env (param i32 i32) (result i32)))
                {WASI}
                (func (export "_start")
                  (drop (call $args (i32.const 0) (i32.const 4)))
                  (drop (call $envs (i32.const 8) (i32.const 12)))
                  (drop (call $env (i32.const 64) (i32.const 1024)))
                  (i32.store (i32.const 16) (i32.const 1024))
                  (i32.store (i32.const 20) (i32.sub (i32.load (i32.const 12)) (i32.const 1)))
                  (drop (call $write (i32.const 1) (i32.const 16) (i32.const 1) (i32.const 24)))
                  (call $exit (i32.add (i32.mul (i32.load (i32.const 0)) (i32.const 100))
                    (i32.load (i32.const 8))))))"#
        );
        let dir = home("env");
        let (end, out) = go(&wat, &grant(&dir), b"");
        assert_eq!(end.unwrap(), End::Exited(1));
        assert_eq!(out, b"NEOCHUMBY_CHUMBY=kitchen");
    }

    #[test]
    fn a_runaway_is_stopped() {
        let dir = home("runaway");
        let g = Grant { busy: Duration::from_millis(300), ..grant(&dir) };
        let started = Instant::now();
        // A loop, and one calling only libcalls (memory.grow), not imports.
        for body in ["(loop $l (br $l))", "(loop $l (drop (memory.grow (i32.const 0))) (br $l))"] {
            let wat = format!(r#"(module (memory 1) (func (export "_start") {body}))"#);
            let e = go(&wat, &g, b"").0.unwrap_err();
            assert!(format!("{e:?}").contains("without calling the host"), "{e:?}");
        }
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn time_in_the_host_isnt_counted() {
        // Sleeps 1 s in poll_oneoff, three times its time to run.
        let wat = format!(
            r#"(module
                (import "wasi_snapshot_preview1" "poll_oneoff" (func $poll (param i32 i32 i32 i32) (result i32)))
                {WASI}
                (func (export "_start")
                  (i32.store8 (i32.const 8) (i32.const 0))
                  (i32.store (i32.const 16) (i32.const 1))
                  (i64.store (i32.const 24) (i64.const 1000000000))
                  (call $exit (call $poll (i32.const 0) (i32.const 128) (i32.const 1) (i32.const 256)))))"#
        );
        let dir = home("sleep");
        let g = Grant { busy: Duration::from_millis(300), ..grant(&dir) };
        let started = Instant::now();
        let (end, _) = go(&wat, &g, b"");
        assert_eq!(end.unwrap(), End::Exited(0));
        assert!(started.elapsed() >= Duration::from_secs(1));
    }

    #[test]
    fn memory_is_capped() {
        // Growing to the cap fails; a little more is fine. Exits 10 if so.
        let wat = r#"(module
            (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
            (memory (export "memory") 1)
            (func (export "_start")
              (if (i32.ne (memory.grow (i32.const 1024)) (i32.const -1)) (then (call $exit (i32.const 1))))
              (if (i32.eq (memory.grow (i32.const 100)) (i32.const -1)) (then (call $exit (i32.const 2))))
              (call $exit (i32.const 10))))"#;
        let dir = home("memory");
        assert_eq!(go(wat, &grant(&dir), b"").0.unwrap(), End::Exited(10));
        // Nor may it start bigger.
        let big = r#"(module (memory 1025) (func (export "_start")))"#;
        assert!(go(big, &grant(&dir), b"").0.is_err());
    }

    #[test]
    fn an_unknown_import_is_refused() {
        let dir = home("import");
        for import in [r#""env" "system""#, r#""wasi_snapshot_preview1" "sock_open""#, r#""neochumby" "exec""#] {
            let wat = format!(r#"(module (import {import} (func)) (func (export "_start")))"#);
            let e = go(&wat, &grant(&dir), b"").0.unwrap_err();
            assert!(format!("{e:?}").contains("unknown import"), "{e:?}");
        }
    }

    /// A module that GETs `url` into a `cap`-byte buffer and writes the
    /// result (an i32), the body's length and the body to stdout.
    fn fetcher(url: &str, cap: usize) -> String {
        format!(
            r#"(module
                (import "neochumby" "https_get" (func $get (param i32 i32 i32 i32 i32) (result i32)))
                {WASI}
                (data (i32.const 256) "{url}")
                (func (export "_start")
                  (i32.store (i32.const 1024)
                    (call $get (i32.const 256) (i32.const {len}) (i32.const 1032) (i32.const {cap}) (i32.const 1028)))
                  (i32.store (i32.const 16) (i32.const 1024))
                  (i32.store (i32.const 20) (i32.add (i32.const 8) (i32.load (i32.const 1028))))
                  (drop (call $write (i32.const 1) (i32.const 16) (i32.const 1) (i32.const 32)))))"#,
            len = url.len()
        )
    }

    /// What `fetcher` wrote: the result, and the body.
    fn fetched(out: &[u8]) -> (i32, Vec<u8>) {
        (i32::from_le_bytes(out[..4].try_into().unwrap()), out[8..].to_vec())
    }

    #[test]
    fn only_approved_hosts() {
        // Each is refused before anything is looked up or connected to.
        let dir = home("refused");
        let cases = [
            ("https://example.com/", "", get::REFUSED),
            ("https://example.com/", "example.org", get::REFUSED),
            ("https://example.com.example.org/", "example.com", get::REFUSED),
            ("https://example.org@example.com/", "example.org", get::BAD),
            ("http://example.com/", "example.com", get::BAD),
            ("ftp://example.com/", "example.com", get::BAD),
            ("example.com", "example.com", get::BAD),
        ];
        for (url, network, want) in cases {
            let g = Grant { hosts: hosts(network), ..grant(&dir) };
            let (end, out) = go(&fetcher(url, 64), &g, b"");
            assert_eq!(end.unwrap(), End::Returned);
            assert_eq!(fetched(&out), (want, Vec::new()), "{url} with {network:?}");
        }
    }

    /// An HTTPS server on 127.0.0.1 that answers `n` GETs with `body`: its
    /// port, and the root its certificate chains to.
    fn server(body: &'static [u8], n: usize) -> (u16, RootCerts) {
        let ck = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).unwrap();
        let root = ureq::tls::Certificate::from_der(ck.cert.der()).to_owned();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(ck.signing_key.serialize_der().into());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![ck.cert.der().clone()], key)
            .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for tcp in listener.incoming().take(n) {
                let conn = rustls::ServerConnection::new(config.clone()).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, tcp.unwrap());
                let mut req = Vec::new();
                let mut b = [0; 1];
                while !req.ends_with(b"\r\n\r\n") && tls.read(&mut b).unwrap_or(0) == 1 {
                    req.push(b[0]);
                }
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = tls.write_all(head.as_bytes()).and_then(|_| tls.write_all(body));
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        });
        (port, RootCerts::new_with_certs(&[root]))
    }

    #[test]
    fn an_approved_host_answers() {
        let (port, roots) = server(b"sunny, 23C", 2);
        let dir = home("allowed");
        let url = format!("https://127.0.0.1:{port}/weather");
        let g = Grant { hosts: hosts("127.0.0.1"), roots: roots.clone(), rule: LOOPBACK, ..grant(&dir) };
        let (end, out) = go(&fetcher(&url, 64), &g, b"");
        assert_eq!(end.unwrap(), End::Returned);
        assert_eq!(fetched(&out), (200, b"sunny, 23C".to_vec()));
        // A body longer than the buffer isn't.
        let (end, out) = go(&fetcher(&url, 4), &g, b"");
        assert_eq!(end.unwrap(), End::Returned);
        assert_eq!(fetched(&out), (get::TOO_LARGE, Vec::new()));
    }

    /// Tests' own allowance: their server on 127.0.0.1.
    const LOOPBACK: egress::Rule = egress::Rule { loopback: true };

    #[test]
    fn an_approved_host_on_this_server_is_refused() {
        // The same server, its host approved, but a companion's rule: no
        // loopback. By address, and by a name that resolves to it.
        let (port, roots) = server(b"secret", 1);
        let dir = home("loopback");
        for host in ["127.0.0.1", "localhost"] {
            let g = Grant { hosts: hosts(host), roots: roots.clone(), ..grant(&dir) };
            let (end, out) = go(&fetcher(&format!("https://{host}:{port}/"), 64), &g, b"");
            assert_eq!(end.unwrap(), End::Returned);
            assert_eq!(fetched(&out), (get::FAILED, Vec::new()), "{host}");
        }
    }

    #[test]
    fn a_certificate_from_elsewhere_fails() {
        // The same server, checked against Mozilla's roots, as a
        // companion's GETs are.
        let (port, _) = server(b"sunny", 1);
        let dir = home("untrusted");
        let g = Grant { hosts: hosts("127.0.0.1"), rule: LOOPBACK, ..grant(&dir) };
        let (end, out) = go(&fetcher(&format!("https://127.0.0.1:{port}/"), 64), &g, b"");
        assert_eq!(end.unwrap(), End::Returned);
        assert_eq!(fetched(&out), (get::FAILED, Vec::new()));
    }
}
