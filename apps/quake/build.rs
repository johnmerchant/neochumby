// Builds the Quake engine (third_party/quake/WinQuake, id's, a submodule,
// unchanged: software renderer, the C paths rather than id's x86 assembly)
// and csrc/varargs.c into the binary. Where the Chumby needs the engine to do
// otherwise, a file is built with a function renamed, so that ours takes
// its place.

const ENGINE: &[&str] = &[
    "cl_demo", "cl_input", "cl_main", "cl_parse", "cl_tent", "chase", "cmd", "common", "console", "crc", "cvar",
    "draw", "d_edge", "d_fill", "d_init", "d_modech", "d_part", "d_polyse", "d_sky", "d_sprite", "d_surf", "d_vars",
    "d_zpoint", "host", "host_cmd", "keys", "menu", "mathlib", "model", "net_dgrm", "net_loop", "net_main", "net_vcr",
    "net_bsd", "nonintel", "pr_cmds", "pr_edict", "pr_exec", "r_aclip", "r_alias", "r_bsp", "r_light", "r_draw",
    "r_efrag", "r_edge", "r_misc", "r_main", "r_sky", "r_sprite", "r_surf", "r_part", "r_vars", "screen", "sbar",
    "sv_main", "sv_phys", "sv_move", "sv_user", "zone", "view", "wad", "world", "cd_null", "snd_dma", "snd_mem",
    "snd_mix",
];

/// Engine files built with names defined to others.
const RENAMED: &[(&str, &[(&str, &str)])] = &[
    // The span drawers are src/span.rs (fixed point, no soft float in the
    // loop); the engine's own go unused.
    ("d_scan", &[("D_DrawSpans8", "D_DrawSpans8_c"), ("D_DrawZSpans", "D_DrawZSpans_c")]),
    // Its own address, from its host name: src/net.rs's lookup doesn't
    // fail on a Chumby whose name isn't in DNS, which crashed the engine.
    ("net_udp", &[("gethostbyname", "chum_gethostbyname")]),
];

const DIR: &str = "third_party/quake/WinQuake";

// 1996 C: implicit declarations and ints, globals defined in more than one
// file (common symbols), type-punned pointers.
fn engine() -> cc::Build {
    let mut b = cc::Build::new();
    b.include(DIR)
        .flag("-std=gnu89")
        .flag("-fcommon")
        .flag("-fno-strict-aliasing")
        .opt_level(2)
        .warnings(false)
        .flag_if_supported("-w");
    b
}

// Its float arithmetic is neochumby-softfloat's, through neochumby.
fn main() {
    if !std::path::Path::new(DIR).exists() {
        panic!("third_party/quake is empty: run `git submodule update --init`");
    }
    let mut build = engine();
    for f in ENGINE {
        build.file(format!("{DIR}/{f}.c"));
    }
    for (f, names) in RENAMED {
        let mut b = engine();
        for (from, to) in *names {
            b.define(from, *to);
        }
        build.objects(b.file(format!("{DIR}/{f}.c")).compile_intermediates());
    }
    build.file("csrc/varargs.c").compile("quake");
    println!("cargo:rerun-if-changed={DIR}");
    println!("cargo:rerun-if-changed=csrc");
}
