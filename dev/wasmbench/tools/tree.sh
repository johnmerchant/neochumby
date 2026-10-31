#!/bin/sh
# tools/tree with zig from nixpkgs: the session, home and this app, in
# build/tree (the tree for the emulator; on a Chumby only apps/wasmbench is
# copied).
set -eu
. "$(dirname "$0")/env.sh"
rm -rf "$B/tree"
cd "$ROOT"
tools/tree "$B/tree" ./dev/wasmbench
du -sh "$B/tree"
ls "$B/tree" "$B/tree/apps" "$B/tree/apps/wasmbench"
cat "$B/tree/apps/wasmbench/manifest"
