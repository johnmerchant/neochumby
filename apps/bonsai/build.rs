// Builds cbonsai (C, GPL-3.0, third_party/cbonsai) into the binary. Its
// Makefile is just cc on the one file, so there is no config.h. main() is
// bonsai_main, which src/main.rs calls. curses and panel are
// neochumby-ncurses'.
fn main() {
    let dir = "third_party/cbonsai";
    if !std::path::Path::new(dir).join("cbonsai.c").exists() {
        panic!("{dir} is empty: run `git submodule update --init`");
    }
    cc::Build::new()
        .file(format!("{dir}/cbonsai.c"))
        .include(std::env::var("DEP_NCURSESW_INCLUDE").unwrap())
        .define("_GNU_SOURCE", None)
        .define("main", "bonsai_main")
        .flag("-std=c99")
        .opt_level(2)
        // Upstream's warnings, not ours to fix.
        .warnings(false)
        .compile("cbonsai");
    println!("cargo:rerun-if-changed={dir}");
}
