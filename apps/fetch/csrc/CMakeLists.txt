# fastfetch (third_party/fastfetch, a submodule, unchanged) as one static
# library for neochumby-fetch to link: its own objects and its main(),
# renamed fastfetch_main. Linked into the Rust binary rather than built as
# a program of its own: zig's runtime for ARMv5 needs __sync_* atomics,
# which only Rust's compiler-builtins provides.
cmake_minimum_required(VERSION 3.21.0)
project(neochumby_fetch C)
# As fastfetch's own directory sets for its sources.
set(CMAKE_C_STANDARD 23)

add_subdirectory(../third_party/fastfetch fastfetch EXCLUDE_FROM_ALL)

add_library(neochumby_fastfetch STATIC ../third_party/fastfetch/src/fastfetch.c)
target_compile_definitions(neochumby_fastfetch PRIVATE FASTFETCH_TARGET_BINARY_NAME=fastfetch main=fastfetch_main)
# Linking an object library puts its objects in the archive.
target_link_libraries(neochumby_fastfetch PRIVATE libfastfetch)
