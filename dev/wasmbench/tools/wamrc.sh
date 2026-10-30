#!/bin/sh
# Builds wamrc (WAMR's AOT compiler) for this machine against nixpkgs' LLVM 18,
# the version WAMR's own build_llvm.py pins. Run under:
#   nix shell nixpkgs#cmake nixpkgs#ninja nixpkgs#llvm_18 --command dev/wasmbench/tools/wamrc.sh
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
b=$here/build
dev=$(nix build --no-link --print-out-paths nixpkgs#llvm_18.dev)
pp=
for p in zlib.dev zstd.dev libxml2.dev; do
  pp="$pp;$(nix build --no-link --print-out-paths nixpkgs#$p)"
done
mkdir -p "$b/wamrc"
cmake -S "$b/src/wamr/wamr-compiler" -B "$b/wamrc" -G Ninja \
  -DWAMR_BUILD_WITH_CUSTOM_LLVM=1 -DLLVM_DIR="$dev/lib/cmake/llvm" \
  -DCMAKE_PREFIX_PATH="${pp#;}" -DCMAKE_BUILD_TYPE=Release
ninja -C "$b/wamrc"
