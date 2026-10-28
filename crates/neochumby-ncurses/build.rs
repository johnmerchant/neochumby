// Builds ncurses (third_party/ncurses) with its own configure and make, as
// one static library, libncursesw, for the C compiler cargo is given
// (tools/zigcc for the Chumby). There is no terminfo database on the
// Chumby: xterm-256color, which neochumby's terminal is, is compiled in
// (a "fallback"), and nothing else. Compiling it takes ncurses' tic and
// infocmp, which are built for the host first: the host's own may be too
// old to read ncurses' terminfo.src (macOS's is).
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

fn main() {
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
            "--with-fallbacks=xterm-256color",
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
    }
    run(&mut cfg);
    let lib = out.join("target");
    make(&lib, &["libs"]);

    // Its headers, as it would install them, for the crates that link it
    // (DEP_NCURSESW_INCLUDE).
    let include = out.join("include");
    std::fs::create_dir_all(&include).unwrap();
    for h in ["curses.h", "ncurses_dll.h", "term.h", "termcap.h", "unctrl.h"] {
        std::fs::copy(lib.join("include").join(h), include.join(h)).unwrap();
    }
    std::fs::copy(include.join("curses.h"), include.join("ncurses.h")).unwrap();
    println!("cargo:include={}", include.display());
    println!("cargo:rustc-link-search=native={}", lib.join("lib").display());
    println!("cargo:rustc-link-lib=static=ncursesw");
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed=third_party/ncurses/ncurses");
    println!("cargo:rerun-if-changed=third_party/ncurses/include");
}
