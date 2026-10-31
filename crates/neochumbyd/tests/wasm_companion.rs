//! `neochumbyd wasm-companion` as companion.rs runs it: frames in on its
//! stdin and out on its stdout, as they come, and an exit when stdin
//! closes.

use std::io::Write;
use std::process::{Command, Stdio};

use neochumby_proto::frame;

/// Copies stdin to stdout, as it comes, until it ends.
const ECHO: &str = r#"(module
    (import "wasi_snapshot_preview1" "fd_read" (func $read (param i32 i32 i32 i32) (result i32)))
    (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
    (memory (export "memory") 1)
    (func (export "_start")
      (i32.store (i32.const 0) (i32.const 1024))
      (i32.store (i32.const 4) (i32.const 4096))
      (loop $l
        (drop (call $read (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 16)))
        (if (i32.eqz (i32.load (i32.const 16))) (then (return)))
        (i32.store (i32.const 8) (i32.const 1024))
        (i32.store (i32.const 12) (i32.load (i32.const 16)))
        (drop (call $write (i32.const 1) (i32.const 8) (i32.const 1) (i32.const 20)))
        (br $l))))"#;

#[test]
fn frames_echo_through_the_process() {
    let dir = std::env::temp_dir().join(format!("neochumbyd-wasm-process-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let file = dir.join("echo.wasm");
    std::fs::write(&file, wat::parse_str(ECHO).unwrap()).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_neochumbyd"))
        .args(["wasm-companion", "--home"])
        .arg(dir.join("home"))
        .args(["--app", "echo", "--network", "example.com"])
        .arg(&file)
        .env("NEOCHUMBY_CHUMBY", "kitchen")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    // Each frame comes back before the next is sent: nothing waits in a
    // buffer for more.
    for f in [&b"hello"[..], &[9; 5000][..], b""] {
        frame::write(&mut stdin, f).unwrap();
        assert_eq!(frame::read(&mut stdout).unwrap().unwrap(), f);
    }
    stdin.flush().unwrap();
    drop(stdin);
    assert!(frame::read(&mut stdout).unwrap().is_none());
    assert!(child.wait().unwrap().success());
}

#[test]
fn a_trap_ends_the_process() {
    let dir = std::env::temp_dir().join(format!("neochumbyd-wasm-trap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let file = dir.join("trap.wasm");
    std::fs::write(&file, wat::parse_str(r#"(module (func (export "_start") unreachable))"#).unwrap()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_neochumbyd"))
        .args(["wasm-companion", "--home"])
        .arg(dir.join("home"))
        .args(["--app", "trap"])
        .arg(&file)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(log.contains("neochumbyd: trap for") && log.contains("unreachable"), "{log}");
}
