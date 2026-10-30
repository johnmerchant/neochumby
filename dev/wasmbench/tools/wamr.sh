#!/bin/sh
# Cross-builds WAMR's iwasm for the Chumby (static musl, zig, ARM926EJ-S) in
# two variants: AOT only, and the fast interpreter only. No threads, no SIMD.
# Run under:
#   nix shell nixpkgs#zig nixpkgs#cmake nixpkgs#ninja --command dev/wasmbench/tools/wamr.sh
set -eu
. "$(dirname "$0")/env.sh"
here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/../.." && pwd)
b=$here/build
src=$b/src/wamr/product-mini/platforms/linux
common="-DWAMR_BUILD_PLATFORM=linux -DWAMR_BUILD_TARGET=${WAMR_TARGET:-ARMV5TE}
  -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=arm
  -DCMAKE_C_COMPILER=$here/tools/cc -DCMAKE_ASM_COMPILER=$here/tools/cc
  -DCMAKE_AR=$root/tools/zigar -DCMAKE_RANLIB=$here/tools/true
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_FLAGS=-O2
  -DWAMR_BUILD_JIT=0 -DWAMR_BUILD_FAST_JIT=0
  -DWAMR_BUILD_LIBC_WASI=1 -DWAMR_BUILD_LIBC_BUILTIN=0
  -DWAMR_BUILD_LIB_PTHREAD=0 -DWAMR_BUILD_LIB_WASI_THREADS=0
  -DWAMR_BUILD_THREAD_MGR=0 -DWAMR_BUILD_SHARED_MEMORY=0
  -DWAMR_BUILD_SIMD=0 -DWAMR_BUILD_MULTI_MODULE=0 -DWAMR_BUILD_MINI_LOADER=0
  -DWAMR_BUILD_BULK_MEMORY=1 -DWAMR_BUILD_REF_TYPES=1"
variant() {
  name=$1
  shift
  # shellcheck disable=SC2086
  cmake -S "$src" -B "$b/wamr-$name" -G Ninja $common "$@"
  ninja -C "$b/wamr-$name" iwasm
}
python3 -I "$here/tools/patch-wamr.py" "$b/src/wamr"
variant aot -DWAMR_BUILD_AOT=1 -DWAMR_BUILD_INTERP=0 -DWAMR_BUILD_FAST_INTERP=0
variant interp -DWAMR_BUILD_AOT=0 -DWAMR_BUILD_INTERP=1 -DWAMR_BUILD_FAST_INTERP=1
variant classic -DWAMR_BUILD_AOT=0 -DWAMR_BUILD_INTERP=1 -DWAMR_BUILD_FAST_INTERP=0
ls -l "$b"/wamr-*/iwasm
