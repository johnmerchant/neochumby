// Builds procps' top (C, GPL-2.0-or-later, third_party/procps) and the
// library it reads /proc with (libproc2, LGPL-2.1-or-later) into the
// binary, without its configure: csrc/config.h is what configure would
// write, and TOP and LIBRARY are the sources its Makefile.am builds them
// from. main() is top_main, which src/main.rs calls. curses is
// neochumby-ncurses'. The calls Linux 2.6.16 lacks on ARM are
// neochumby-sys' stand-ins (compat26), which the binary links in place of
// musl's with --wrap.
const TOP: &[&str] = &["src/top/top", "src/top/top_nls", "local/fileutils", "local/signals", "local/strutils"];

const LIBRARY: &[&str] = &[
    "capname",
    "devname",
    "diskstats",
    "escape",
    "meminfo",
    "namespace",
    "numa",
    "pids",
    "pwcache",
    "readproc",
    "signames",
    "slabinfo",
    "sort",
    "stat",
    "sysinfo",
    "uptime",
    "version",
    "vmstat",
    "wchan",
];

fn main() {
    let dir = "third_party/procps";
    if !std::path::Path::new(dir).join("src/top/top.c").exists() {
        panic!("{dir} is empty: run `git submodule update --init`");
    }
    // Its version is its tag's (configure asks git too): v4.0.7.
    let version = std::process::Command::new("git")
        .args(["-C", dir, "describe", "--tags", "--abbrev=0"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|v| v.trim().trim_start_matches('v').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".into());
    let mut build = cc::Build::new();
    for f in TOP {
        build.file(format!("{dir}/{f}.c"));
    }
    for f in LIBRARY {
        build.file(format!("{dir}/library/{f}.c"));
    }
    build
        .include("csrc")
        .include(format!("{dir}/local"))
        .include(format!("{dir}/library/include"))
        .include(std::env::var("DEP_NCURSESW_INCLUDE").unwrap())
        // As Makefile.am has it: not every file includes it.
        .flag("-include")
        .flag("config.h")
        .define("PROCPS_VERSION", format!("\"{version}\"").as_str())
        .define("_GNU_SOURCE", None)
        .define("main", "top_main")
        .opt_level(2)
        // Upstream's warnings, not ours to fix.
        .warnings(false)
        .compile("top");
    // top's calls to these reach neochumby_sys::compat26's __wrap_ functions,
    // which call musl's own as __real_; only for the Chumby's ARM musl, where
    // those are built.
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.starts_with("arm") && target.contains("musl") {
        for name in ["openat", "readlinkat", "__fstatat_time64", "__pselect_time64"] {
            println!("cargo:rustc-link-arg-bins=-Wl,--wrap={name}");
        }
    }
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed={dir}/src/top");
    println!("cargo:rerun-if-changed={dir}/library");
}
