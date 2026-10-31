#!/bin/sh
# Cross-builds wasmi-run for the Chumby, with the toolchain tools/tree uses.
set -eu
. "$(dirname "$0")/env.sh"
export CARGO_TARGET_ARMV5TE_UNKNOWN_LINUX_MUSLEABI_LINKER=$ROOT/tools/zigcc
export CARGO_TARGET_ARMV5TE_UNKNOWN_LINUX_MUSLEABI_RUSTFLAGS="-C link-self-contained=no -C target-cpu=arm926ej-s"
export CC_armv5te_unknown_linux_musleabi=$ROOT/tools/zigcc AR_armv5te_unknown_linux_musleabi=$ROOT/tools/zigar
cargo build --release --manifest-path "$WB/wasmi-run/Cargo.toml" --target armv5te-unknown-linux-musleabi \
  --target-dir "$B/cargo"
ls -l "$B/cargo/armv5te-unknown-linux-musleabi/release/wasmi-run"
