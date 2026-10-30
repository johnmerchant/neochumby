// Builds htop (C, GPL-2.0-or-later, third_party/htop) into the binary for
// Linux, without its configure: csrc/config.h is what configure would
// write, and HTOP are the sources its Makefile.am builds for Linux, less
// those for the optional libraries (libnl, libunwind). main() is
// htop_main, which src/main.rs calls. curses is neochumby-ncurses'.
// linux/LinuxProcessTable.c is built with PATCH applied.
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

// When htop shows threads, it reads a process's main thread's
// /proc/PID/task/PID/stat, and skips the process if it can't. The Chumby's
// 2.6.16 kernel denies task/ (EACCES) to a reader chrooted away from the
// process's root, as an app's jail is, so the patch falls back to the
// process's own /proc/PID/stat, which it can read.
const PATCH: (&str, &str) = (
    "   ssize_t r = Compat_readfileat(procFd, path, buf, sizeof(buf));
   if (r < 0)
      return false;
",
    "   ssize_t r = Compat_readfileat(procFd, path, buf, sizeof(buf));
   /* neochumby: build.rs's PATCH. */
   if (r < 0 && scanMainThread)
      r = Compat_readfileat(procFd, \"stat\", buf, sizeof(buf));
   if (r < 0)
      return false;
",
);

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
    let table = format!("{dir}/linux/LinuxProcessTable.c");
    let source = std::fs::read_to_string(&table).unwrap();
    assert_eq!(source.matches(PATCH.0).count(), 1, "PATCH doesn't apply to {table}");
    let patched = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("LinuxProcessTable.c");
    std::fs::write(&patched, source.replace(PATCH.0, PATCH.1)).unwrap();
    build.file(patched);
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
