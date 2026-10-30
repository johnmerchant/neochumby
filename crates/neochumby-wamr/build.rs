// Builds WAMR (third_party/wamr, a submodule) into a static library: the
// runtime only, with its AOT loader and its fast interpreter, and none of
// its own WASI, threads, SIMD, JIT or multi-module support. The host API
// is ours (neochumby-wasm), registered as raw native functions.
//
// For the Chumby (armv5te), the AOT loader is built for ARMV5TE, as the
// spike measured it (docs/proposals/0003-wasm-apps.md): WAMR's ARM target
// assumes ARMv7 instructions. Elsewhere (the host, for tests) only the
// interpreter, which needs no assembly: invokeNative's portable C.
//
// WAMR is read from the submodule as it is, pinned to a release; nothing is
// downloaded. The one change it needs here, patches/0001-*.patch (the fast
// interpreter's loader, alignment-safe on ARMv5), is applied to a copy of
// the file it changes, in OUT_DIR, which is compiled in its place: the
// submodule itself is never touched.

use std::path::{Path, PathBuf};

const WAMR: &str = "third_party/wamr";

/// The runtime's C, under core/.
const COMMON: &[&str] = &[
    "iwasm/common/wasm_application.c",
    "iwasm/common/wasm_blocking_op.c",
    "iwasm/common/wasm_c_api.c",
    "iwasm/common/wasm_exec_env.c",
    "iwasm/common/wasm_loader_common.c",
    "iwasm/common/wasm_memory.c",
    "iwasm/common/wasm_native.c",
    "iwasm/common/wasm_runtime_common.c",
    "iwasm/interpreter/wasm_interp_fast.c",
    "iwasm/interpreter/wasm_runtime.c",
    "shared/mem-alloc/mem_alloc.c",
    "shared/mem-alloc/ems/ems_alloc.c",
    "shared/mem-alloc/ems/ems_gc.c",
    "shared/mem-alloc/ems/ems_hmu.c",
    "shared/mem-alloc/ems/ems_kfc.c",
    "shared/platform/common/posix/posix_blocking_op.c",
    "shared/platform/common/posix/posix_malloc.c",
    "shared/platform/common/posix/posix_memmap.c",
    "shared/platform/common/posix/posix_sleep.c",
    "shared/platform/common/posix/posix_thread.c",
    "shared/platform/common/posix/posix_time.c",
    "shared/utils/bh_assert.c",
    "shared/utils/bh_bitmap.c",
    "shared/utils/bh_common.c",
    "shared/utils/bh_hashmap.c",
    "shared/utils/bh_leb128.c",
    "shared/utils/bh_list.c",
    "shared/utils/bh_log.c",
    "shared/utils/bh_queue.c",
    "shared/utils/bh_vector.c",
    "shared/utils/runtime_timer.c",
];

/// The AOT loader, for the Chumby only.
const AOT: &[&str] = &[
    "iwasm/aot/aot_intrinsic.c",
    "iwasm/aot/aot_loader.c",
    "iwasm/aot/aot_runtime.c",
    "iwasm/aot/arch/aot_reloc_arm.c",
    "iwasm/common/arch/invokeNative_arm.s",
];

const INCLUDES: &[&str] = &[
    "iwasm/include",
    "iwasm/common",
    "iwasm/interpreter",
    "iwasm/aot",
    "shared/platform/include",
    "shared/mem-alloc",
    "shared/utils",
];

/// What every build has on, and off.
const DEFINES: &[(&str, &str)] = &[
    ("WASM_ENABLE_INTERP", "1"),
    ("WASM_ENABLE_FAST_INTERP", "1"),
    ("WASM_ENABLE_BULK_MEMORY", "1"),
    ("WASM_ENABLE_BULK_MEMORY_OPT", "1"),
    ("WASM_ENABLE_REF_TYPES", "1"),
    ("WASM_ENABLE_CALL_INDIRECT_OVERLONG", "1"),
    ("WASM_ENABLE_SHRUNK_MEMORY", "1"),
    ("WASM_ENABLE_MODULE_INST_CONTEXT", "1"),
    ("WASM_ENABLE_LOG", "1"),
    ("WASM_ENABLE_LIBC_WASI", "0"),
    ("WASM_ENABLE_LIBC_BUILTIN", "0"),
    ("WASM_ENABLE_LIB_PTHREAD", "0"),
    ("WASM_ENABLE_LIB_WASI_THREADS", "0"),
    ("WASM_ENABLE_THREAD_MGR", "0"),
    ("WASM_ENABLE_SHARED_MEMORY", "0"),
    ("WASM_ENABLE_SIMD", "0"),
    ("WASM_ENABLE_JIT", "0"),
    ("WASM_ENABLE_FAST_JIT", "0"),
    ("WASM_ENABLE_LAZY_JIT", "0"),
    ("WASM_ENABLE_MULTI_MODULE", "0"),
    ("WASM_ENABLE_MINI_LOADER", "0"),
    ("WASM_ENABLE_APP_FRAMEWORK", "0"),
    ("WASM_ENABLE_BASE_LIB", "0"),
    ("WASM_ENABLE_EXTENDED_CONST_EXPR", "0"),
    ("WASM_ENABLE_WORD_ALIGN_READ", "0"),
    ("WASM_ENABLE_OPCODE_COUNTER", "0"),
    ("WASM_ENABLE_FUZZ_TEST", "0"),
    ("BH_ENABLE_GC_VERIFY", "0"),
    ("BH_ENABLE_GC_CORRUPTION_CHECK", "0"),
    ("BH_MALLOC", "wasm_runtime_malloc"),
    ("BH_FREE", "wasm_runtime_free"),
    ("_GNU_SOURCE", "1"),
];

/// The patches, in order, each changing one file of WAMR's.
const PATCHES: &[&str] = &["patches/0001-fast-interp-loader-store-handler-addresses-unaligned-safely.patch"];

/// Applies each of [`PATCHES`] to a copy of the file it changes, in `out`:
/// the copies, by their paths under the submodule. A patch that no longer
/// applies (upstream moved) fails the build.
fn patched(out: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    for p in PATCHES {
        let text = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}"));
        println!("cargo:rerun-if-changed={p}");
        // The DEP-3 header comes before the diff, which starts at "--- a/".
        let at = text.find("\n--- a/").unwrap_or_else(|| panic!("{p}: no diff")) + 1;
        let diff = &text[at..];
        let file = diff.lines().next().and_then(|l| l.strip_prefix("--- a/")).expect("a file").trim().to_string();
        let patch = diffy::Patch::from_str(diff).unwrap_or_else(|e| panic!("{p}: {e}"));
        let original = std::fs::read_to_string(Path::new(WAMR).join(&file)).expect("the patched file");
        let fixed = diffy::apply(&original, &patch).unwrap_or_else(|e| panic!("{p} doesn't apply to {file}: {e}"));
        let dst = out.join(file.replace('/', "_"));
        std::fs::write(&dst, fixed).expect("writing a patched file");
        files.push((file, dst));
    }
    files
}

/// WAMR's version, from core/version.h, and the submodule's commit when git
/// can say: what an .aot is compiled for (the AOT format can change between
/// commits of one release).
fn version(core: &Path) -> String {
    let commit = std::process::Command::new("git")
        .args(["-C", WAMR, "rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|c| !c.is_empty());
    let release = release(core);
    match commit {
        Some(c) => format!("{release}-{c}"),
        None => release,
    }
}

/// core/version.h's MAJOR.MINOR.PATCH.
fn release(core: &Path) -> String {
    let h = std::fs::read_to_string(core.join("version.h")).expect("version.h");
    let part = |name: &str| {
        h.lines()
            .find_map(|l| l.strip_prefix(&format!("#define WAMR_VERSION_{name} ")))
            .map(str::trim)
            .expect("a version part")
            .to_string()
    };
    format!("{}.{}.{}", part("MAJOR"), part("MINOR"), part("PATCH"))
}

fn main() {
    let core = Path::new(WAMR).join("core");
    assert!(
        core.join("version.h").exists(),
        "{WAMR} is empty: git submodule update --init crates/neochumby-wamr/third_party/wamr"
    );
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let target = std::env::var("TARGET").unwrap();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let chumby = target.starts_with("armv5te-");

    let mut b = cc::Build::new();
    for f in COMMON {
        b.file(core.join(f));
    }
    // wasm_loader.c, patched; its includes are found by INCLUDES below.
    for (file, copy) in patched(&out) {
        assert!(file.ends_with("wasm_loader.c"), "{file}: only the loader is patched");
        b.file(copy);
    }
    for d in INCLUDES {
        b.include(core.join(d));
    }
    for (k, v) in DEFINES {
        b.define(k, *v);
    }
    let platform = if os == "macos" { "darwin" } else { "linux" };
    b.include(core.join("shared/platform").join(platform));
    b.file(core.join("shared/platform").join(platform).join("platform_init.c"));
    if os == "macos" {
        // No mremap(2) there: WAMR's own.
        b.define("BH_PLATFORM_DARWIN", None).define("WASM_HAVE_MREMAP", "0");
        b.file(core.join("shared/platform/common/memory/mremap.c"));
    } else {
        b.define("BH_PLATFORM_LINUX", None).define("WASM_HAVE_MREMAP", "1");
    }
    if chumby {
        for f in AOT {
            b.file(core.join(f));
        }
        b.define("BUILD_TARGET_ARM", None)
            .define("BUILD_TARGET", "\"ARMV5TE\"")
            .define("WASM_ENABLE_AOT", "1")
            .define("WASM_ENABLE_AOT_INTRINSICS", "1")
            .define("WASM_ENABLE_QUICK_AOT_ENTRY", "1")
            .flag("-marm");
    } else {
        let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
        let (target_define, name) = match arch.as_str() {
            "x86_64" => ("BUILD_TARGET_X86_64", "X86_64"),
            "aarch64" => ("BUILD_TARGET_AARCH64", "AARCH64"),
            a => panic!("neochumby-wamr: no WAMR target for {a}"),
        };
        // Software bounds checks, as on the Chumby (32-bit, no room for a
        // guard region), so that the tests check the same thing, and the
        // runner can limit its address space.
        b.define("WASM_DISABLE_HW_BOUND_CHECK", "1").define("WASM_DISABLE_STACK_HW_BOUND_CHECK", "1");
        b.file(core.join("iwasm/common/arch/invokeNative_general.c"))
            .define(target_define, None)
            .define("BUILD_TARGET", format!("\"{name}\"").as_str())
            .define("WASM_ENABLE_AOT", "0");
    }
    b.flag_if_supported("-std=gnu99")
        .flag_if_supported("-ffunction-sections")
        .flag_if_supported("-fdata-sections")
        .define("NDEBUG", None)
        .opt_level(3)
        // Upstream's warnings are upstream's.
        .warnings(false)
        .flag_if_supported("-w")
        .compile("wamr");

    println!("cargo:rustc-env=NEOCHUMBY_WAMR_VERSION={}", version(&core));
    println!("cargo:rerun-if-changed={WAMR}/core");
    println!("cargo:rerun-if-changed=build.rs");
}
