#!/bin/sh
# Fetches the third-party source the spike measured into build/src, each at
# the commit its numbers (docs/proposals/0003-wasm-apps.md) came from:
#
#   wamr       WebAssembly Micro Runtime (2.4.3 and a little after), iwasm
#              and wamrc
#   wasm3      wasm3's interpreter and CLI
#
# CoreMark, the one C workload, is a submodule (third_party/coremark),
# pinned to the commit the numbers came from.
#
# Then, in order: tools/wamr.sh, tools/wamrc.sh, tools/wasm3.sh,
# tools/wasmi.sh, tools/workloads.sh, tools/wasmtime.sh (which compiles the
# workloads), tools/stage.sh and tools/tree.sh; tools/emu.sh runs it on the
# emulator, and on a Chumby apps/wasmbench of build/tree is opened from
# home.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
src=$here/build/src
mkdir -p "$src"
get() {
  name=$1 url=$2 rev=$3
  d=$src/$name
  if [ ! -d "$d" ]; then
    git init -q "$d"
    git -C "$d" remote add origin "$url"
  fi
  git -C "$d" fetch -q --depth 1 origin "$rev"
  git -C "$d" checkout -q --detach FETCH_HEAD
  echo "$name: $(git -C "$d" rev-parse HEAD)"
}
get wamr https://github.com/bytecodealliance/wasm-micro-runtime f8ed1f442969c806ced7b127d4e4d397bf051737
get wasm3 https://github.com/wasm3/wasm3 28ecb9af6d2040e474a70f7cb7f43666740141fb
