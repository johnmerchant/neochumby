//! Hostile modules, run by the runner itself (on the host, its interpreter
//! and the host kernel's seccomp and Landlock): each must be refused, trap,
//! or be contained, and never reach past what it was granted.
//!
//! The same suite runs on the emulated Chumby (dev/emulate-wasm), with the
//! session, the jail and the Chumby's kernels.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// An app directory with `manifest` and `app.wasm` (from WAT), in a fresh
/// temporary directory.
fn app(test: &str, manifest: &str, wat: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("neochumby-wasm-{}-{test}", std::process::id())).join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest"), format!("wasm=app.wasm\n{manifest}")).unwrap();
    std::fs::write(dir.join("app.wasm"), wat::parse_str(wat).unwrap()).unwrap();
    dir
}

/// The runner, to run the app in `dir`. With NEOCHUMBY_WASM_STAGE=TREE, the
/// app is also copied to TREE/wasmtest/, as it is run here, for
/// dev/emulate-wasm to run on the emulated Chumby.
fn runner(dir: &Path) -> Command {
    if let Some(tree) = std::env::var_os("NEOCHUMBY_WASM_STAGE") {
        let to = Path::new(&tree).join("wasmtest").join(dir.file_name().unwrap());
        let _ = std::fs::remove_dir_all(&to);
        std::fs::create_dir_all(&to).unwrap();
        for f in std::fs::read_dir(dir).unwrap() {
            let f = f.unwrap().path();
            std::fs::copy(&f, to.join(f.file_name().unwrap())).unwrap();
        }
    }
    let mut c = Command::new(env!("CARGO_BIN_EXE_neochumby-wasm"));
    c.env("NEOCHUMBY_WASM", dir.join("app.wasm")).stdin(Stdio::null());
    c
}

fn run(dir: &Path) -> Output {
    runner(dir).output().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const HELLO: &str = r#"(module
  (import "wasi_snapshot_preview1" "fd_write" (func $w (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 8) "\10\00\00\00\06\00\00\00")
  (data (i32.const 16) "hello\n")
  (func (export "_start") (drop (call $w (i32.const 1) (i32.const 8) (i32.const 1) (i32.const 32)))))"#;

#[test]
fn a_text_app_writes_to_its_terminal() {
    let o = run(&app("hello", "", HELLO));
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(o.stdout, b"hello\n");
    assert!(stderr(&o).contains("the interpreter runs it"), "{}", stderr(&o));
}

#[test]
fn access_not_granted_is_refused_before_it_runs() {
    let wat = r#"(module
      (import "neochumby" "fb_fill" (func (param i32 i32 i32 i32 i32)))
      (func (export "_start")))"#;
    let o = run(&app("ungranted", "access=speaker\n", wat));
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("neochumby.fb_fill, which needs \"fb\""), "{}", stderr(&o));
    // And what the host hasn't at all: the file system, sockets.
    for (n, import) in [
        (
            "path",
            r#"(import "wasi_snapshot_preview1" "path_open" (func (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))"#,
        ),
        ("sock", r#"(import "wasi_snapshot_preview1" "sock_accept" (func (param i32 i32 i32) (result i32)))"#),
        ("env", r#"(import "env" "system" (func (param i32) (result i32)))"#),
    ] {
        let o = run(&app(n, "access=speaker\n", &format!("(module {import} (func (export \"_start\")))")));
        assert_eq!(o.status.code(), Some(2), "{n}: {}", stderr(&o));
        assert!(stderr(&o).contains("which the host doesn't have"), "{n}: {}", stderr(&o));
    }
}

#[test]
fn out_of_bounds_traps() {
    let wat = r#"(module
      (import "wasi_snapshot_preview1" "fd_write" (func $w (param i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 1)
      (func (export "_start")
        ;; An iovec pointing past the end of memory: the host checks it.
        (i32.store (i32.const 0) (i32.const 65000))
        (i32.store (i32.const 4) (i32.const 4096))
        (drop (call $w (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 16)))))"#;
    let o = run(&app("oob-host", "", wat));
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("out of bounds"), "{}", stderr(&o));
    assert!(o.stdout.is_empty());
    let wat =
        r#"(module (memory (export "memory") 1) (func (export "_start") (drop (i64.load (i32.const 0xfffffff0)))))"#;
    let o = run(&app("oob-load", "", wat));
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("out of bounds"), "{}", stderr(&o));
}

#[test]
fn a_forged_aot_is_refused() {
    // A compiled module beside the bytecode, with no record, a record for
    // another .wasm, or its own record and other bytes: the bytecode runs.
    let dir = app("aot", "aot=app.aot\naot-key=sha256:00/wamr:0/x\naot-sha256=00\n", HELLO);
    std::fs::write(dir.join("app.aot"), b"\0aot\x03\0\0\0native code").unwrap();
    let o = run(&dir);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(o.stdout, b"hello\n");
    assert!(stderr(&o).contains("compiled from another .wasm or for another runner"), "{}", stderr(&o));
    // Compiled code where the bytecode should be.
    let dir = app("aot-as-wasm", "", HELLO);
    std::fs::write(dir.join("app.wasm"), b"\0aot\x03\0\0\0native code").unwrap();
    let o = run(&dir);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("isn't WebAssembly bytecode"), "{}", stderr(&o));
}

#[test]
fn memory_is_capped() {
    // Growing past 16 MiB fails inside the module, as wasm says it does.
    let wat = r#"(module (memory (export "memory") 1)
      (func (export "_start")
        (if (i32.ne (memory.grow (i32.const 255)) (i32.const 1)) (then unreachable))
        (if (i32.ne (memory.grow (i32.const 1)) (i32.const -1)) (then unreachable))))"#;
    let o = run(&app("cap", "", wat));
    assert!(o.status.success(), "{}", stderr(&o));
    // Asking for more than the cap to start with: refused, or given no
    // more than the cap (the module traps if it got more).
    let wat = r#"(module (memory (export "memory") 300)
      (func (export "_start") (if (i32.gt_u (memory.size) (i32.const 256)) (then unreachable))))"#;
    let o = run(&app("cap-initial", "", wat));
    assert!(matches!(o.status.code(), Some(0 | 2)), "{:?}: {}", o.status, stderr(&o));
}

#[test]
fn a_catalog_app_never_gets_system_terminal_root_or_network() {
    for w in ["system", "terminal", "root", "network"] {
        let o = run(&app(&format!("catalog-{w}"), &format!("source=catalog\naccess={w}\n"), HELLO));
        assert_eq!(o.status.code(), Some(2), "{w}");
        assert!(stderr(&o).contains(&format!("never granted \"{w}\"")), "{w}: {}", stderr(&o));
    }
}

#[test]
fn a_runaway_loop_is_contained() {
    // No metering: it runs until it is ended, as the escape hatch ends it,
    // and takes nothing else with it.
    let dir = app("runaway", "", r#"(module (memory (export "memory") 1) (func (export "_start") (loop (br 0))))"#);
    let mut child = runner(&dir).stderr(Stdio::null()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(child.try_wait().unwrap().is_none(), "it should still be looping");
    child.kill().unwrap();
    let t = Instant::now();
    let status = child.wait().unwrap();
    assert!(t.elapsed() < Duration::from_secs(2));
    assert!(!status.success());
}

#[cfg(target_os = "linux")]
#[test]
fn seccomp_kills_a_forbidden_call() {
    use std::os::unix::process::ExitStatusExt;
    let o = Command::new(env!("CARGO_BIN_EXE_neochumby-wasm")).arg("--selftest-seccomp").output().unwrap();
    if o.status.code() == Some(77) {
        eprintln!("no seccomp here; skipped");
        return;
    }
    assert_eq!(o.status.signal(), Some(31), "SIGSYS: {}", stderr(&o));
    assert!(!stderr(&o).contains("FAILED"));
}

#[cfg(target_os = "linux")]
#[test]
fn the_runner_locks_itself_down_before_the_module_runs() {
    let o = run(&app("locked", "", HELLO));
    assert!(o.status.success(), "{}", stderr(&o));
    let e = stderr(&o);
    assert!(e.contains("locked down: "), "{e}");
    // On a kernel with seccomp, the module ran under it.
    if !e.contains("neither seccomp nor Landlock") {
        assert!(e.contains("seccomp"), "{e}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_catalog_app_runs_only_under_seccomp_unless_trusted() {
    let dir = app("catalog", "source=catalog\n", HELLO);
    let o = run(&dir);
    if stderr(&o).contains("neither seccomp nor Landlock") {
        // A kernel without them: refused, unless trusted.
        assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
        assert!(stderr(&o).contains("only on a kernel with seccomp"), "{}", stderr(&o));
        let o = runner(&dir).env("NEOCHUMBY_WASM_TRUSTED", "1").output().unwrap();
        assert!(o.status.success(), "{}", stderr(&o));
    } else {
        assert!(o.status.success(), "{}", stderr(&o));
        assert_eq!(o.stdout, b"hello\n");
    }
}
