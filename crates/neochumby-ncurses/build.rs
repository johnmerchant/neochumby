// ncurses for the apps, one of two ways, as NEOCHUMBY_NCURSES says (tools/tree
// sets it from the Linux the tree is for, --linux):
//
// - `source` (or unset), for the stock 2.6: builds ncurses
//   (third_party/ncurses) with its own configure and make, as static
//   libraries, libncursesw and libpanelw (which cbonsai uses), for the C
//   compiler cargo is given (tools/zigcc for the Chumby). There is no
//   terminfo database on the stock firmware: linux, which neochumby's
//   terminal is, and xterm-256color, which it was, are compiled in
//   ("fallbacks"), and nothing else. Compiling them takes ncurses' tic and
//   infocmp, which are built for the host first: the host's own may be too
//   old to read ncurses' terminfo.src (macOS's is).
// - `system`, for the dist's Linux (5.9 and later): links the image's own
//   libncursesw and libpanelw, dynamically, found with pkg-config in the
//   image's sysroot (PKG_CONFIG_SYSROOT_DIR and PKG_CONFIG_LIBDIR, which
//   tools/tree points at it). The image has the terminfo database.
//
// Either way the crates that build C against it find its headers, and only
// ncurses' own, in DEP_NCURSESW_INCLUDE.
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(cmd: &mut Command) {
    match cmd.status() {
        Ok(s) if s.success() => {}
        r => panic!("{cmd:?}: {r:?}"),
    }
}

/// `src`'s configure, run in `dir`, a fresh directory.
fn configure(src: &Path, dir: &Path) -> Command {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let mut cmd = Command::new("sh");
    cmd.arg(src.join("configure")).current_dir(dir).args([
        "--without-shared",
        "--without-cxx",
        "--without-cxx-binding",
        "--without-ada",
        "--without-tests",
        "--without-manpages",
        "--without-debug",
        "--without-gpm",
        "--disable-db-install",
        "--disable-pc-files",
        "--prefix=/nonexistent",
    ]);
    cmd
}

fn make(dir: &Path, args: &[&str]) {
    let jobs = std::env::var("NUM_JOBS").unwrap_or_else(|_| "4".into());
    run(Command::new("make").arg(format!("-j{jobs}")).args(args).current_dir(dir));
}

/// ncurses' headers, as it installs them, which the crates that link it
/// include.
const HEADERS: [&str; 6] = ["curses.h", "ncurses_dll.h", "panel.h", "term.h", "termcap.h", "unctrl.h"];

/// Copies ncurses' headers from `from` to OUT_DIR/include, curses.h as
/// ncurses.h too, and says where they are (DEP_NCURSESW_INCLUDE).
fn headers(from: &Path) {
    let include = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("include");
    std::fs::create_dir_all(&include).unwrap();
    for h in HEADERS {
        std::fs::copy(from.join(h), include.join(h)).unwrap_or_else(|e| panic!("{}: {e}", from.join(h).display()));
    }
    std::fs::copy(include.join("curses.h"), include.join("ncurses.h")).unwrap();
    println!("cargo:include={}", include.display());
}

/// What pkg-config says of ncursesw and panelw, with `flag`, split.
fn pkg_config(flag: &str) -> Vec<String> {
    let out = Command::new("pkg-config")
        .args([flag, "ncursesw", "panelw"])
        .output()
        .unwrap_or_else(|e| panic!("pkg-config: {e} (NEOCHUMBY_NCURSES=system needs it)"));
    if !out.status.success() {
        panic!("pkg-config {flag} ncursesw panelw: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    String::from_utf8_lossy(&out.stdout).split_whitespace().map(str::to_string).collect()
}

/// The image's ncurses, linked dynamically.
fn system() {
    for v in ["PKG_CONFIG_SYSROOT_DIR", "PKG_CONFIG_LIBDIR", "PKG_CONFIG_PATH"] {
        println!("cargo:rerun-if-env-changed={v}");
    }
    let dirs: Vec<PathBuf> =
        pkg_config("--cflags-only-I").iter().filter_map(|f| f.strip_prefix("-I")).map(PathBuf::from).collect();
    let found = dirs.iter().find(|d| d.join("curses.h").exists());
    // With no -I, the headers are in the sysroot's usr/include.
    let sysroot = std::env::var("PKG_CONFIG_SYSROOT_DIR").map(PathBuf::from).unwrap_or_default();
    let fallback = sysroot.join("usr/include");
    headers(found.unwrap_or(&fallback));
    for dir in pkg_config("--libs-only-L").iter().filter_map(|f| f.strip_prefix("-L")) {
        println!("cargo:rustc-link-search=native={dir}");
    }
    for lib in pkg_config("--libs-only-l").iter().filter_map(|f| f.strip_prefix("-l")) {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=NEOCHUMBY_NCURSES");
    match std::env::var("NEOCHUMBY_NCURSES").as_deref() {
        Ok("system") => system(),
        Ok("source") | Err(_) => source(),
        Ok(other) => panic!("NEOCHUMBY_NCURSES={other}: `source` (built here, static) or `system` (the image's)"),
    }
}

/// ncurses built here, static.
fn source() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("third_party/ncurses");
    if !src.join("configure").exists() {
        panic!("third_party/ncurses is empty: run `git submodule update --init`");
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let host = std::env::var("HOST").unwrap();
    let target = std::env::var("TARGET").unwrap();

    // tic and infocmp, for the host.
    let tools = out.join("host");
    if !tools.join("progs/infocmp").exists() {
        let cc = cc::Build::new().target(&host).get_compiler();
        run(configure(&src, &tools).env("CC", cc.path()));
        make(&tools, &["libs"]);
        make(&tools, &["-C", "progs", "tic", "infocmp"]);
    }

    // The library, for the target.
    let build = cc::Build::new();
    let cc = build.get_compiler();
    let mut cfg = configure(&src, &out.join("target"));
    cfg.arg(format!("--host={target}"))
        .arg(format!("--with-build-cc={}", cc::Build::new().target(&host).get_compiler().path().display()))
        .arg(format!("--with-tic-path={}", tools.join("progs/tic").display()))
        .arg(format!("--with-infocmp-path={}", tools.join("progs/infocmp").display()))
        .args([
            "--without-progs",
            "--without-profile",
            "--disable-database",
            "--with-fallbacks=linux,xterm-256color",
            "--disable-home-terminfo",
            "--enable-widec",
        ])
        .env("CC", cc.path())
        .env("CFLAGS", cc.cflags_env())
        .env("AR", build.get_archiver().get_program())
        // Which writes the archive's index itself.
        .env("RANLIB", "true");
    if target.starts_with("arm") {
        // What configure's link tests need, as csrc/probe.S says.
        cc::Build::new().file("csrc/probe.S").cargo_metadata(false).compile("probe");
        cfg.env("LIBS", out.join("libprobe.a"));
        // Optimised whatever cargo's profile: at -O0 zig's runtime keeps all
        // of its atomics, and configure's link tests would need every
        // __sync_ helper they call, not just probe.S's.
        let mut cflags = cc.cflags_env();
        cflags.push(" -O2");
        cfg.env("CFLAGS", cflags);
    }
    run(&mut cfg);
    let lib = out.join("target");
    // "libs" makes libpanelw too, in the same lib directory.
    make(&lib, &["libs"]);

    headers(&lib.join("include"));
    println!("cargo:rustc-link-search=native={}", lib.join("lib").display());
    println!("cargo:rustc-link-lib=static=ncursesw");
    println!("cargo:rustc-link-lib=static=panelw");
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed=third_party/ncurses/ncurses");
    println!("cargo:rerun-if-changed=third_party/ncurses/include");
}
