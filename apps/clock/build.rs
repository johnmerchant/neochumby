// Builds tty-clock (C, BSD-3-Clause, third_party/tty-clock) into the binary
// for Linux. It has no configure: its Makefile compiles ttyclock.c alone.
// main() is clock_main, which src/main.rs calls. curses is
// neochumby-ncurses'.
fn main() {
    let dir = "third_party/tty-clock";
    if !std::path::Path::new(&format!("{dir}/ttyclock.c")).exists() {
        panic!("{dir} is empty: run `git submodule update --init`");
    }
    cc::Build::new()
        .file(format!("{dir}/ttyclock.c"))
        .include(dir)
        .include(std::env::var("DEP_NCURSESW_INCLUDE").unwrap())
        .define("_GNU_SOURCE", None)
        .define("main", "clock_main")
        .opt_level(2)
        // Upstream's warnings, not ours to fix.
        .warnings(false)
        .compile("tty-clock");
    println!("cargo:rerun-if-changed={dir}");
}
