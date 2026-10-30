# Sourced by the build scripts: zig, cmake, ninja and LLVM from nixpkgs on PATH.
for p in zig cmake ninja llvm_18; do
  PATH=$(nix build --no-link --print-out-paths "nixpkgs#$p" | head -n 1)/bin:$PATH
done
export PATH
WB=$(cd "$(dirname "$0")/.." && pwd)
ROOT=$(cd "$WB/../.." && pwd)
B=$WB/build
