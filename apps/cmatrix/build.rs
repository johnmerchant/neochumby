// Builds cmatrix (C, GPL-3.0-or-later, third_party/cmatrix) into the binary
// for Linux, without its configure: csrc/config.h is what configure would
// write. main() is cmatrix_main, which src/main.rs calls. curses is
// neochumby-ncurses'.
fn main() {
    let dir = "third_party/cmatrix";
    let Ok(configure) = std::fs::read_to_string(format!("{dir}/configure.ac")) else {
        panic!("{dir} is empty: run `git submodule update --init`");
    };
    // AC_INIT([cmatrix], [2.0], [...])
    let version = configure
        .lines()
        .find_map(|l| l.strip_prefix("AC_INIT([cmatrix], ["))
        .and_then(|v| v.split(']').next())
        .expect("cmatrix's version in configure.ac");
    cc::Build::new()
        .file(format!("{dir}/cmatrix.c"))
        .include("csrc")
        .include(std::env::var("DEP_NCURSESW_INCLUDE").unwrap())
        .define("VERSION", format!("\"{version}\"").as_str())
        .define("_GNU_SOURCE", None)
        .define("main", "cmatrix_main")
        .opt_level(2)
        // Upstream's warnings, not ours to fix.
        .warnings(false)
        .compile("cmatrix");
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed={dir}");
}
