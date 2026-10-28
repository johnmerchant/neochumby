// Builds fastfetch (third_party/fastfetch) into the binary, through
// csrc/CMakeLists.txt, with the C compiler cargo is given (tools/zigcc for
// the Chumby). Every optional library is off: fastfetch would dlopen them,
// and a static binary can't.
const OFF: &[&str] = &[
    "VULKAN",
    "WAYLAND",
    "XCB_RANDR",
    "XRANDR",
    "DRM",
    "VADRM",
    "VAX11",
    "VDPAU",
    "GIO",
    "DCONF",
    "EET",
    "DBUS",
    "SQLITE3",
    "RPM",
    "EGL",
    "GLX",
    "OPENCL",
    "FREETYPE",
    "ELF",
    "IMAGE_LOGO",
    "IMAGEMAGICK7",
    "IMAGEMAGICK6",
    "SIXEL",
    "CHAFA",
    "ZLIB",
    "LUA",
    "LIBZFS",
    // One core: threads would only cost.
    "THREADS",
    // Its objects go into a Rust binary, whose linker can't read C's LTO
    // bitcode.
    "LTO",
];

fn main() {
    if !std::path::Path::new("third_party/fastfetch/CMakeLists.txt").exists() {
        panic!("third_party/fastfetch is empty: run `git submodule update --init`");
    }
    let mut cfg = cmake::Config::new("csrc");
    // The archiver cargo is given (tools/zigar), which also writes the
    // archive's index: CMake would pick the host's ar and ranlib, which on
    // macOS index no ELF symbols.
    let ar = cc::Build::new().get_archiver();
    cfg.define("CMAKE_AR", ar.get_program()).define("CMAKE_C_ARCHIVE_FINISH", "<CMAKE_AR> s <TARGET>");
    for o in OFF {
        cfg.define(format!("ENABLE_{o}"), "OFF");
    }
    let dst = cfg
        .define("IS_MUSL", "ON")
        .define("BUILD_FLASHFETCH", "OFF")
        .define("SET_TWEAK", "OFF")
        // Its configure checks link test programs, which fail for the
        // reason in csrc/CMakeLists.txt: compile them only. Everything they
        // look for, musl has.
        .define("CMAKE_TRY_COMPILE_TARGET_TYPE", "STATIC_LIBRARY")
        // zig makes __DATE__ (fastfetch's version string) an error.
        .cflag("-Wno-date-time")
        .profile("Release")
        .build_target("neochumby_fastfetch")
        .build();
    println!("cargo:rustc-link-search=native={}/build", dst.display());
    println!("cargo:rustc-link-lib=static=neochumby_fastfetch");
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed=third_party/fastfetch/src");
}
