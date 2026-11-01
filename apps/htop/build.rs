// Builds htop (C, GPL-2.0-or-later, third_party/htop) into the binary for
// Linux, without its configure: csrc/config.h is what configure would
// write, and HTOP are the sources its Makefile.am builds for Linux, less
// those for the optional libraries (libnl, libunwind). main() is
// htop_main, which src/main.rs calls. curses is neochumby-ncurses'.
const HTOP: &[&str] = &[
    "htop",
    "Action",
    "Affinity",
    "AffinityPanel",
    "AvailableColumnsPanel",
    "AvailableMetersPanel",
    "BatteryMeter",
    "CategoriesPanel",
    "ColorsPanel",
    "ColumnsPanel",
    "CommandLine",
    "CommandScreen",
    "CPUMeter",
    "CRT",
    "DateTimeMeter",
    "DiskIOMeter",
    "DisplayOptionsPanel",
    "DynamicColumn",
    "DynamicMeter",
    "DynamicScreen",
    "EnvScreen",
    "FileDescriptorMeter",
    "FunctionBar",
    "Hashtable",
    "Header",
    "HeaderOptionsPanel",
    "HostnameMeter",
    "History",
    "IncSet",
    "InfoScreen",
    "LineEditor",
    "ListItem",
    "LoadAverageMeter",
    "Machine",
    "MainPanel",
    "MemoryMeter",
    "MemorySwapMeter",
    "Meter",
    "MetersPanel",
    "NetworkIOMeter",
    "Object",
    "OpenFilesScreen",
    "OptionItem",
    "Panel",
    "Process",
    "ProcessLocksScreen",
    "ProcessTable",
    "Row",
    "RichString",
    "Scheduling",
    "ScreenManager",
    "ScreensPanel",
    "ScreenTabsPanel",
    "Settings",
    "SignalsPanel",
    "SwapMeter",
    "SysArchMeter",
    "Table",
    "TasksMeter",
    "TraceScreen",
    "UptimeMeter",
    "UsersTable",
    "Vector",
    "XUtils",
    // Linux's.
    "GPUMeter",
    "generic/gettime",
    "generic/hostname",
    "generic/uname",
    "linux/CGroupUtils",
    "linux/Compat",
    "linux/GPU",
    "linux/HugePageMeter",
    "linux/IOPriorityPanel",
    "linux/LibSensors",
    "linux/LinuxMachine",
    "linux/LinuxProcess",
    "linux/LinuxProcessTable",
    "linux/OpenRCMeter",
    "linux/Platform",
    "linux/PressureStallMeter",
    "linux/SELinuxMeter",
    "linux/SystemdMeter",
    "linux/ZramMeter",
    "linux/ZswapMeter",
    "zfs/ZfsArcMeter",
    "zfs/ZfsCompressedArcMeter",
];

fn main() {
    let dir = "third_party/htop";
    let Ok(configure) = std::fs::read_to_string(format!("{dir}/configure.ac")) else {
        panic!("{dir} is empty: run `git submodule update --init`");
    };
    // m4_define([htop_release_version], [3.5.3])
    let version = configure
        .lines()
        .find_map(|l| l.strip_prefix("m4_define([htop_release_version], ["))
        .and_then(|v| v.strip_suffix("])"))
        .expect("htop's version in configure.ac");
    let mut build = cc::Build::new();
    for f in HTOP {
        build.file(format!("{dir}/{f}.c"));
    }
    build
        .include("csrc")
        .include(dir)
        // The platform's headers, as Makefile.am has them.
        .include(format!("{dir}/linux"))
        .include(std::env::var("DEP_NCURSESW_INCLUDE").unwrap())
        .define("HAVE_CONFIG_H", None)
        .define("HTOP_VERSION", format!("\"{version}\"").as_str())
        .define("NDEBUG", None)
        .define("_GNU_SOURCE", None)
        .define("main", "htop_main")
        .flag("-std=c99")
        .opt_level(2)
        // Upstream's warnings, not ours to fix.
        .warnings(false)
        .compile("htop");
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed={dir}");
}
