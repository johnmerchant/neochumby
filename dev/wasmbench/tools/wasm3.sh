#!/bin/sh
# Cross-builds wasm3's CLI (platforms/app) for the Chumby, with its WASI, as
# its CMakeLists builds a release: -O3, no frame pointer, no unwind tables.
set -eu
. "$(dirname "$0")/env.sh"
s=$B/src/wasm3
mkdir -p "$B/wasm3"
# shellcheck disable=SC2046
"$WB/tools/cc" -O3 -s -fomit-frame-pointer -fno-stack-check -fno-stack-protector \
  -fno-asynchronous-unwind-tables -Wno-unused-function -Wno-date-time -D__DATE__=\"-\" -D__TIME__=\"-\" -Dd_m3HasWASI \
  -I"$s/source" $(ls "$s"/source/*.c) "$s/platforms/app/main.c" -lm -o "$B/wasm3/wasm3"
ls -l "$B/wasm3/wasm3"
