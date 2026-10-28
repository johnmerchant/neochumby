// Builds the Quake engine (engine/, id's WinQuake, software renderer, the C
// paths rather than id's x86 assembly) and csrc/glue.c into the binary.

const ENGINE: &[&str] = &[
    "cl_demo", "cl_input", "cl_main", "cl_parse", "cl_tent", "chase", "cmd", "common", "console", "crc", "cvar",
    "draw", "d_edge", "d_fill", "d_init", "d_modech", "d_part", "d_polyse", "d_scan", "d_sky", "d_sprite", "d_surf",
    "d_vars", "d_zpoint", "host", "host_cmd", "keys", "menu", "mathlib", "model", "net_dgrm", "net_loop", "net_main",
    "net_vcr", "net_udp", "net_bsd", "nonintel", "pr_cmds", "pr_edict", "pr_exec", "r_aclip", "r_alias", "r_bsp",
    "r_light", "r_draw", "r_efrag", "r_edge", "r_misc", "r_main", "r_sky", "r_sprite", "r_surf", "r_part", "r_vars",
    "screen", "sbar", "sv_main", "sv_phys", "sv_move", "sv_user", "zone", "view", "wad", "world", "cd_null",
    "snd_null",
];

// Its float arithmetic is neochumby-softfloat's, through neochumby.
fn main() {
    let mut build = cc::Build::new();
    for f in ENGINE {
        build.file(format!("engine/{f}.c"));
    }
    build
        .file("csrc/glue.c")
        .include("engine")
        // 1996 C: implicit declarations and ints, globals defined in more
        // than one file (common symbols), type-punned pointers.
        .define("NEOCHUMBY_QUAKE_SPANS", None)
        .flag("-std=gnu89")
        .flag("-fcommon")
        .flag("-fno-strict-aliasing")
        .opt_level(2)
        .warnings(false)
        .flag_if_supported("-w")
        .compile("quake");
    println!("cargo:rerun-if-changed=engine");
    println!("cargo:rerun-if-changed=csrc");
}
