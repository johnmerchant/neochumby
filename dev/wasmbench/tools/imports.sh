#!/bin/sh
# The WASI imports each built module needs.
set -eu
. "$(dirname "$0")/env.sh"
wt=$(nix build --no-link --print-out-paths nixpkgs#wasm-tools)/bin/wasm-tools
for f in "$B"/out/wasm/*.wasm; do
  echo "== $(basename "$f")"
  "$wt" print "$f" | grep '(import' | sed 's/ (func.*//; s/^ *//'
done
