#!/bin/sh
# Wasmtime with Pulley: compiles the .wasm workloads to pulley32 .cwasm with
# the host build (Cranelift), and cross-builds the runtime-only run side
# for the Chumby.
set -eu
. "$(dirname "$0")/env.sh"
m=$WB/wasmtime-run/Cargo.toml
cargo build -q --release --features compile --manifest-path "$m" --target-dir "$B/cargo-host"
mkdir -p "$B/out/cwasm"
for f in "$B"/out/wasm/*.wasm; do
  "$B/cargo-host/release/wasmtime-run" compile "$f" "$B/out/cwasm/$(basename "$f" .wasm).cwasm"
done
ls -l "$B/out/cwasm"
export CARGO_TARGET_ARMV5TE_UNKNOWN_LINUX_MUSLEABI_LINKER=$ROOT/tools/zigcc
export CARGO_TARGET_ARMV5TE_UNKNOWN_LINUX_MUSLEABI_RUSTFLAGS="-C link-self-contained=no -C target-cpu=arm926ej-s"
export CC_armv5te_unknown_linux_musleabi=$WB/tools/cc AR_armv5te_unknown_linux_musleabi=$ROOT/tools/zigar
cargo build --release --manifest-path "$m" --target armv5te-unknown-linux-musleabi --target-dir "$B/cargo"
ls -l "$B/cargo/armv5te-unknown-linux-musleabi/release/wasmtime-run"
