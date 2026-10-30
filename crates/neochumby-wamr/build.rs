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
// The fast interpreter's loader, wasm_loader.c, is compiled from a copy with
// one fix (fixed_loader, below) until upstream has it.

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

/// wasm_loader.c with its three handler-address stores made alignment-safe.
///
/// Rewriting `select` for i64 and f64 operands, the fast interpreter's
/// loader patches the handler's address into compiled code with a plain
/// `*(uint32 *)(p_code_compiled_tmp - sizeof(uint32)) = ...`, but that code
/// is only 2-byte aligned. An ARMv5 (the Chumby's ARM926EJ-S) ignores the
/// low bits of an unaligned STR's address, so the address lands 2 bytes off
/// and a later dispatch jumps to garbage (SIGILL, in Thumb state). QEMU does
/// the store as an ARMv6 would, so the emulator never shows it. The stores
/// go through memcpy instead, which is byte stores where the alignment
/// isn't known. The build fails if upstream's code has moved and the three
/// aren't found as they were.
fn fixed_loader(core: &Path, out: &Path) -> PathBuf {
    let src = core.join("iwasm/interpreter/wasm_loader.c");
    let s = std::fs::read_to_string(&src).expect("wasm_loader.c");
    let mut fixed = String::with_capacity(s.len() + 256);
    let mut rest = s.as_str();
    let mut sites = 0;
    let head = "*(uint32 *)(p_code_compiled_tmp";
    while let Some(i) = rest.find(head) {
        let (before, at) = rest.split_at(i);
        let end = at.find(';').expect("a statement");
        let stmt = &at[..end];
        let (lhs, rhs) = stmt.split_once('=').expect("an assignment");
        let ptr = lhs.trim().trim_start_matches("*(uint32 *)").trim();
        let ptr = ptr.strip_prefix('(').and_then(|p| p.strip_suffix(')')).expect("a parenthesised pointer");
        fixed.push_str(before);
        fixed.push_str(&format!("NEOCHUMBY_PUT_U32({}, {})", ptr.trim(), rhs.trim()));
        rest = &at[end..];
        sites += 1;
    }
    fixed.push_str(rest);
    assert_eq!(sites, 3, "wasm_loader.c: expected the 3 unaligned handler stores, found {sites}");
    let include = "#include \"wasm_loader.h\"";
    assert!(fixed.contains(include), "wasm_loader.c: no #include \"wasm_loader.h\"");
    let put = "#define NEOCHUMBY_PUT_U32(p, v) \\\n    do { uint32 put_v_ = (uint32)(v); memcpy((p), &put_v_, sizeof put_v_); } while (0)";
    let fixed = fixed.replacen(include, &format!("{include}\n{put}"), 1);
    let dst = out.join("wasm_loader.c");
    std::fs::write(&dst, fixed).expect("writing wasm_loader.c");
    dst
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
    b.file(fixed_loader(&core, &out));
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
