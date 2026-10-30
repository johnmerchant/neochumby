//! wasmi-run: runs a WASI preview 1 command under wasmi, with only the WASI
//! calls the benchmark's workloads import (args, clock_time_get, fd_write to
//! stdout and stderr, fd_fdstat_get, fd_seek, fd_close, proc_exit): no
//! files, no preopens, no environment.
//!
//!   wasmi-run [-t] [-e] MODULE.wasm [ARG ...]
//!
//! -t prints, to stderr, how long the load (parse, validate, translate),
//! the instantiation and the run took. -e translates eagerly, every function
//! before the run, instead of wasmi's default, lazily on first call.

use std::io::Write;
use std::time::Instant;

use wasmi::{Caller, CompilationMode, Config, Engine, Error, Extern, Linker, Memory, Module, Store};

const WASI: &str = "wasi_snapshot_preview1";
const ESUCCESS: i32 = 0;
const EBADF: i32 = 8;
const EFAULT: i32 = 21;
const EINVAL: i32 = 28;
const ESPIPE: i32 = 70;

struct Ctx {
    args: Vec<Vec<u8>>,
}

fn memory(c: &Caller<'_, Ctx>) -> Option<Memory> {
    c.get_export("memory").and_then(Extern::into_memory)
}

fn put(c: &mut Caller<'_, Ctx>, at: i32, b: &[u8]) -> bool {
    match memory(c) {
        Some(m) => m.write(c, at as u32 as usize, b).is_ok(),
        None => false,
    }
}

fn get(c: &Caller<'_, Ctx>, at: i32, b: &mut [u8]) -> bool {
    match memory(c) {
        Some(m) => m.read(c, at as u32 as usize, b).is_ok(),
        None => false,
    }
}

fn u32_at(c: &Caller<'_, Ctx>, at: u32) -> Option<u32> {
    let mut b = [0; 4];
    get(c, at as i32, &mut b).then(|| u32::from_le_bytes(b))
}

fn wasi(linker: &mut Linker<Ctx>) -> Result<(), wasmi::Error> {
    linker.func_wrap(WASI, "args_sizes_get", |mut c: Caller<'_, Ctx>, argc: i32, size: i32| -> i32 {
        let n = c.data().args.len() as u32;
        let s: u32 = c.data().args.iter().map(|a| a.len() as u32 + 1).sum();
        if put(&mut c, argc, &n.to_le_bytes()) && put(&mut c, size, &s.to_le_bytes()) { ESUCCESS } else { EFAULT }
    })?;
    linker.func_wrap(WASI, "args_get", |mut c: Caller<'_, Ctx>, argv: i32, buf: i32| -> i32 {
        let args = c.data().args.clone();
        let mut at = buf as u32;
        for (i, a) in args.iter().enumerate() {
            let mut z = a.clone();
            z.push(0);
            if !put(&mut c, argv + 4 * i as i32, &at.to_le_bytes()) || !put(&mut c, at as i32, &z) {
                return EFAULT;
            }
            at += z.len() as u32;
        }
        ESUCCESS
    })?;
    linker.func_wrap(WASI, "clock_time_get", |mut c: Caller<'_, Ctx>, id: i32, _prec: i64, out: i32| -> i32 {
        let clock = match id {
            0 => libc::CLOCK_REALTIME,
            1 => libc::CLOCK_MONOTONIC,
            _ => return EINVAL,
        };
        let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: clock_gettime(2) writes the time into `t`, a live timespec.
        unsafe { libc::clock_gettime(clock, &mut t) };
        let ns = t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64;
        if put(&mut c, out, &ns.to_le_bytes()) { ESUCCESS } else { EFAULT }
    })?;
    linker.func_wrap(WASI, "fd_write", |mut c: Caller<'_, Ctx>, fd: i32, iovs: i32, n: i32, written: i32| -> i32 {
        let mut out: Vec<u8> = Vec::new();
        for i in 0..n as u32 {
            let (Some(p), Some(l)) = (u32_at(&c, iovs as u32 + 8 * i), u32_at(&c, iovs as u32 + 8 * i + 4)) else {
                return EFAULT;
            };
            let s = out.len();
            out.resize(s + l as usize, 0);
            if !get(&c, p as i32, &mut out[s..]) {
                return EFAULT;
            }
        }
        let r = match fd {
            1 => std::io::stdout().write_all(&out),
            2 => std::io::stderr().write_all(&out),
            _ => return EBADF,
        };
        if r.is_err() {
            return EBADF;
        }
        if put(&mut c, written, &(out.len() as u32).to_le_bytes()) { ESUCCESS } else { EFAULT }
    })?;
    linker.func_wrap(WASI, "fd_fdstat_get", |mut c: Caller<'_, Ctx>, fd: i32, out: i32| -> i32 {
        if !(0..=2).contains(&fd) {
            return EBADF;
        }
        // A character device; fd_write (1 << 6) its only right.
        let mut st = [0u8; 24];
        st[0] = 2;
        st[8..16].copy_from_slice(&(1u64 << 6).to_le_bytes());
        if put(&mut c, out, &st) { ESUCCESS } else { EFAULT }
    })?;
    linker.func_wrap(WASI, "fd_seek", |_: Caller<'_, Ctx>, fd: i32, _off: i64, _wh: i32, _out: i32| -> i32 {
        if (0..=2).contains(&fd) { ESPIPE } else { EBADF }
    })?;
    linker.func_wrap(WASI, "fd_close", |_: Caller<'_, Ctx>, _fd: i32| -> i32 { ESUCCESS })?;
    linker.func_wrap(WASI, "proc_exit", |_: Caller<'_, Ctx>, code: i32| -> Result<(), Error> {
        Err(Error::i32_exit(code))
    })?;
    Ok(())
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut timing = false;
    let mut eager = false;
    while let Some(a) = args.first() {
        match a.as_str() {
            "-t" => timing = true,
            "-e" => eager = true,
            _ => break,
        }
        args.remove(0);
    }
    let Some(path) = args.first().cloned() else {
        eprintln!("usage: wasmi-run [-t] [-e] MODULE.wasm [ARG ...]");
        std::process::exit(2);
    };
    let t0 = Instant::now();
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("wasmi-run: {path}: {e}");
        std::process::exit(1)
    });
    let mut config = Config::default();
    config.compilation_mode(if eager { CompilationMode::Eager } else { CompilationMode::LazyTranslation });
    let engine = Engine::new(&config);
    let module = Module::new(&engine, &bytes[..]).unwrap_or_else(|e| {
        eprintln!("wasmi-run: {path}: {e}");
        std::process::exit(1)
    });
    let t1 = Instant::now();
    let ctx = Ctx { args: args.iter().map(|a| a.as_bytes().to_vec()).collect() };
    let mut store = Store::new(&engine, ctx);
    let mut linker = Linker::new(&engine);
    wasi(&mut linker).expect("WASI functions");
    let instance = linker.instantiate_and_start(&mut store, &module).unwrap_or_else(|e| {
        eprintln!("wasmi-run: instantiating: {e}");
        std::process::exit(1)
    });
    let start = instance.get_typed_func::<(), ()>(&store, "_start").unwrap_or_else(|e| {
        eprintln!("wasmi-run: _start: {e}");
        std::process::exit(1)
    });
    let t2 = Instant::now();
    let code = match start.call(&mut store, ()) {
        Ok(()) => 0,
        Err(e) => match e.i32_exit_status() {
            Some(c) => c,
            None => {
                eprintln!("wasmi-run: trap: {e}");
                1
            }
        },
    };
    let t3 = Instant::now();
    let _ = std::io::stdout().flush();
    if timing {
        let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
        eprintln!("wasmi-run: load_ms={:.2} inst_ms={:.2} run_ms={:.2}", ms(t0, t1), ms(t1, t2), ms(t2, t3));
    }
    std::process::exit(code);
}
