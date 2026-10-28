// asciiquarium-rs is a library and a binary, whose loop is in main.rs, and
// main is private. src/main.rs compiles main.rs in as a module, to call it,
// and include! won't take the inner doc comments that head the file: this
// writes it to OUT_DIR without those lines, the rest as it is.
use std::{env, fs, path::PathBuf};

fn main() {
    let src = "third_party/asciiquarium-rs/src/main.rs";
    println!("cargo:rerun-if-changed={src}");
    let text = fs::read_to_string(src).unwrap_or_else(|e| panic!("{src}: {e} (git submodule update --init)"));
    let rest: String = text.lines().filter(|l| !l.starts_with("//!")).map(|l| format!("{l}\n")).collect();
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("upstream.rs");
    fs::write(out, rest).expect("write upstream.rs");
}
