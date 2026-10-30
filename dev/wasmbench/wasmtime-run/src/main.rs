//! wasmtime-run: Wasmtime's Pulley interpreter for the wasm benchmark.
//!
//!   wasmtime-run compile IN.wasm OUT.cwasm   (host build, --features compile)
//!   wasmtime-run [-t] MODULE.cwasm [ARG ...]
//!
//! The run side is the runtime alone, deserializing pulley32 bytecode
//! compiled ahead on the host, with the same minimal WASI as wasmi-run.

use std::io::Write;
use std::time::Instant;

use wasmtime::{Caller, Config, Engine, Extern, Linker, Memory, Module, Store};

const WASI: &str = "wasi_snapshot_preview1";

struct Ctx {
    args: Vec<Vec<u8>>,
}

#[derive(Debug)]
struct Exit(i32);
impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}
impl std::error::Error for Exit {}

fn mem(c: &mut Caller<'_, Ctx>) -> Memory {
    match c.get_export("memory") {
        Some(Extern::Memory(m)) => m,
        _ => panic!("no memory"),
    }
}

fn put(c: &mut Caller<'_, Ctx>, at: i32, b: &[u8]) -> bool {
    let m = mem(c);
    m.write(c, at as u32 as usize, b).is_ok()
}

fn get(c: &mut Caller<'_, Ctx>, at: u32, b: &mut [u8]) -> bool {
    let m = mem(c);
    m.read(c, at as usize, b).is_ok()
}

fn u32_at(c: &mut Caller<'_, Ctx>, at: u32) -> u32 {
    let mut b = [0; 4];
    get(c, at, &mut b);
    u32::from_le_bytes(b)
}

fn config() -> Config {
    let mut c = Config::new();
    c.target("pulley32").expect("pulley32");
    c.memory_reservation(0);
    c.memory_guard_size(0);
    c.memory_reservation_for_growth(1 << 20);
    c.signals_based_traps(false);
    c
}

fn wasi(l: &mut Linker<Ctx>) -> wasmtime::Result<()> {
    l.func_wrap(WASI, "args_sizes_get", |mut c: Caller<'_, Ctx>, argc: i32, size: i32| -> i32 {
        let n = c.data().args.len() as u32;
        let s: u32 = c.data().args.iter().map(|a| a.len() as u32 + 1).sum();
        if put(&mut c, argc, &n.to_le_bytes()) && put(&mut c, size, &s.to_le_bytes()) { 0 } else { 21 }
    })?;
    l.func_wrap(WASI, "args_get", |mut c: Caller<'_, Ctx>, argv: i32, buf: i32| -> i32 {
        let args = c.data().args.clone();
        let mut at = buf as u32;
        for (i, a) in args.iter().enumerate() {
            let mut z = a.clone();
            z.push(0);
            put(&mut c, argv + 4 * i as i32, &at.to_le_bytes());
            put(&mut c, at as i32, &z);
            at += z.len() as u32;
        }
        0
    })?;
    l.func_wrap(WASI, "clock_time_get", |mut c: Caller<'_, Ctx>, id: i32, _p: i64, out: i32| -> i32 {
        let clock = if id == 0 { libc::CLOCK_REALTIME } else { libc::CLOCK_MONOTONIC };
        let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime(2) writes into `t`, a live timespec.
        unsafe { libc::clock_gettime(clock, &mut t) };
        let ns = t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64;
        put(&mut c, out, &ns.to_le_bytes());
        0
    })?;
    l.func_wrap(WASI, "fd_write", |mut c: Caller<'_, Ctx>, fd: i32, iovs: i32, n: i32, written: i32| -> i32 {
        let mut out = Vec::new();
        for i in 0..n as u32 {
            let p = u32_at(&mut c, iovs as u32 + 8 * i);
            let len = u32_at(&mut c, iovs as u32 + 8 * i + 4);
            let s = out.len();
            out.resize(s + len as usize, 0);
            get(&mut c, p, &mut out[s..]);
        }
        let _ = match fd {
            1 => std::io::stdout().write_all(&out),
            2 => std::io::stderr().write_all(&out),
            _ => return 8,
        };
        put(&mut c, written, &(out.len() as u32).to_le_bytes());
        0
    })?;
    l.func_wrap(WASI, "fd_fdstat_get", |mut c: Caller<'_, Ctx>, fd: i32, out: i32| -> i32 {
        if !(0..=2).contains(&fd) {
            return 8;
        }
        let mut st = [0u8; 24];
        st[0] = 2;
        st[8..16].copy_from_slice(&(1u64 << 6).to_le_bytes());
        put(&mut c, out, &st);
        0
    })?;
    l.func_wrap(WASI, "fd_seek", |_: Caller<'_, Ctx>, _fd: i32, _o: i64, _w: i32, _out: i32| -> i32 { 70 })?;
    l.func_wrap(WASI, "fd_close", |_: Caller<'_, Ctx>, _fd: i32| -> i32 { 0 })?;
    l.func_wrap(WASI, "proc_exit", |_: Caller<'_, Ctx>, code: i32| -> wasmtime::Result<()> {
        Err(wasmtime::Error::new(Exit(code)))
    })?;
    Ok(())
}

fn main() -> wasmtime::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(feature = "compile")]
    if args.first().map(String::as_str) == Some("compile") {
        let engine = Engine::new(&config())?;
        let wasm = std::fs::read(&args[1])?;
        std::fs::write(&args[2], engine.precompile_module(&wasm)?)?;
        return Ok(());
    }
    let timing = args.first().map(String::as_str) == Some("-t");
    if timing {
        args.remove(0);
    }
    let t0 = Instant::now();
    let engine = Engine::new(&config())?;
    // SAFETY: the .cwasm was made by this program's compile, from the same
    // Wasmtime; deserializing trusts it.
    let module = unsafe { Module::deserialize_file(&engine, &args[0])? };
    let t1 = Instant::now();
    let mut store = Store::new(&engine, Ctx { args: args.iter().map(|a| a.as_bytes().to_vec()).collect() });
    let mut linker = Linker::new(&engine);
    wasi(&mut linker)?;
    let instance = linker.instantiate(&mut store, &module)?;
    let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
    let t2 = Instant::now();
    let code = match start.call(&mut store, ()) {
        Ok(()) => 0,
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(Exit(c)) => *c,
            None => {
                eprintln!("wasmtime-run: trap: {e:?}");
                1
            }
        },
    };
    let t3 = Instant::now();
    let _ = std::io::stdout().flush();
    if timing {
        let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
        eprintln!("wasmtime-run: load_ms={:.2} inst_ms={:.2} run_ms={:.2}", ms(t0, t1), ms(t1, t2), ms(t2, t3));
    }
    std::process::exit(code);
}
