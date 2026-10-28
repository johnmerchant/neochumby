// Builds doomgeneric (C, third_party/) into the binary; our platform layer
// (src/platform.rs) provides the DG_* functions it calls.
const DOOM: &[&str] = &[
    "dummy",
    "am_map",
    "doomdef",
    "doomstat",
    "dstrings",
    "d_event",
    "d_items",
    "d_iwad",
    "d_loop",
    "d_main",
    "d_mode",
    "d_net",
    "f_finale",
    "f_wipe",
    "g_game",
    "hu_lib",
    "hu_stuff",
    "info",
    "i_cdmus",
    "i_endoom",
    "i_joystick",
    "i_scale",
    "i_sound",
    "i_system",
    "i_timer",
    "memio",
    "m_argv",
    "m_bbox",
    "m_cheat",
    "m_config",
    "m_controls",
    "m_fixed",
    "m_menu",
    "m_misc",
    "m_random",
    "p_ceilng",
    "p_doors",
    "p_enemy",
    "p_floor",
    "p_inter",
    "p_lights",
    "p_map",
    "p_maputl",
    "p_mobj",
    "p_plats",
    "p_pspr",
    "p_saveg",
    "p_setup",
    "p_sight",
    "p_spec",
    "p_switch",
    "p_telept",
    "p_tick",
    "p_user",
    "r_bsp",
    "r_data",
    "r_draw",
    "r_main",
    "r_plane",
    "r_segs",
    "r_sky",
    "r_things",
    "sha1",
    "sounds",
    "statdump",
    "st_lib",
    "st_stuff",
    "s_sound",
    "tables",
    "v_video",
    "wi_stuff",
    "w_checksum",
    "w_file",
    "w_main",
    "w_wad",
    "z_zone",
    "w_file_stdc",
    "i_input",
    "i_video",
    "doomgeneric",
];

fn main() {
    let dir = "third_party/doomgeneric/doomgeneric";
    let mut build = cc::Build::new();
    for f in DOOM {
        build.file(format!("{dir}/{f}.c"));
    }
    build
        // The Chumby's screen: Doom's 320x200 centred in 320x240, unscaled.
        .define("DOOMGENERIC_RESX", "320")
        .define("DOOMGENERIC_RESY", "240")
        .define("NORMALUNIX", None)
        .define("LINUX", None)
        .define("_DEFAULT_SOURCE", None)
        .opt_level(2)
        // 1990s C: warnings are upstream's, not ours to fix.
        .warnings(false)
        .flag_if_supported("-w")
        .compile("doomgeneric");
    println!("cargo:rerun-if-changed={dir}");
}
