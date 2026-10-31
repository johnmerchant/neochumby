# CMake's toolchain for the Chumby (armv5te, static musl), with zig as the
# compiler, as tools/zigcc is cargo's: for C built by CMake from a crate's
# build script (wamr-sys builds WAMR so). zig can't link an executable for
# this target on its own (its compiler-rt lacks what Rust's
# compiler_builtins gives every binary), so CMake's checks compile a static
# library instead of linking a program.
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR arm)
get_filename_component(NEOCHUMBY_TOOLS "${CMAKE_CURRENT_LIST_DIR}" ABSOLUTE)
set(CMAKE_C_COMPILER "${NEOCHUMBY_TOOLS}/zigcc")
set(CMAKE_ASM_COMPILER "${NEOCHUMBY_TOOLS}/zigcc")
set(CMAKE_AR "${NEOCHUMBY_TOOLS}/zigar")
# zig ar indexes the archive itself (its `s`); there is no zig ranlib.
set(CMAKE_C_ARCHIVE_FINISH "<CMAKE_AR> s <TARGET>")
set(CMAKE_ASM_ARCHIVE_FINISH "<CMAKE_AR> s <TARGET>")
set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)
