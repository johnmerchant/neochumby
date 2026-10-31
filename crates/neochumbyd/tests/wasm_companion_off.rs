//! A neochumbyd built without the `wasm-companions` feature: its
//! `wasm-companion` process says so, and runs nothing.
#![cfg(not(feature = "wasm-companions"))]

use std::process::Command;

#[test]
fn without_the_feature_the_process_refuses() {
    let out = Command::new(env!("CARGO_BIN_EXE_neochumbyd"))
        .args(["wasm-companion", "--home", "/nonexistent", "--app", "weather", "/nonexistent/companion.wasm"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("built without wasm companions") && err.contains("`wasm-companions`"), "{err}");
}
