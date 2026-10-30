#!/bin/sh
# Builds the workloads, each twice from the same source: natively for the
# Chumby (static musl, ARM926EJ-S, soft float) and to wasm32-wasip1 (no
# threads, no SIMD), then the .wasm files to WAMR AOT with wamrc
# ($WAMRC_FLAGS). Into build/out/{native,wasm,aot}.
#   coremark   CoreMark, in C (the submodule third_party/coremark),
#              ITERATIONS=0: it calibrates itself to run for at least 10 s
#   float      workloads/ (Rust), the soft-float kernel
#   fb         workloads/ (Rust), the framebuffer kernel
#   startup    workloads/ (Rust), a ~200 KB module whose main does nothing
#   empty      workloads/ (Rust), an empty main, for the runtime's own
#              start-up cost
set -eu
. "$(dirname "$0")/env.sh"
o=$B/out
mkdir -p "$o/native" "$o/wasm" "$o/aot" "$B/gen"
# Wasm features: the MVP plus what wasi-libc's build expects and every
# runtime here implements (no SIMD, no threads, no multivalue, no reference
# types), in zig's names and in rustc's.
WASM_CPU=${WASM_CPU:-mvp+bulk_memory+sign_ext+mutable_globals+nontrapping_fptoint}
WASM_RUST_FEATURES=${WASM_RUST_FEATURES:-+bulk-memory,+sign-ext,+mutable-globals,+nontrapping-fptoint}
native() { "$WB/tools/cc" -O2 -s "$@"; }
# A 1 MiB stack, not zig's 16 MiB: initial memory is what some runtimes commit.
wasm() { zig cc -target wasm32-wasi -mcpu="$WASM_CPU" -O2 -s -Wl,-z,stack-size=1048576 "$@"; }

# CoreMark's `simple` port, with clock() (CPU time; and an emulated process
# clock under WASI) swapped for CLOCK_MONOTONIC in microseconds.
cm=$WB/third_party/coremark
if [ ! -f "$cm/core_main.c" ]; then
  echo "$0: CoreMark is a submodule and isn't checked out:" >&2
  echo "  git submodule update --init dev/wasmbench/third_party/coremark" >&2
  exit 1
fi
port=$B/gen/coremark
mkdir -p "$port"
cp "$cm/simple/core_portme.h" "$port/"
sed -e 's|^#define NSECS_PER_SEC .*|#define NSECS_PER_SEC 1000000|' \
  -e 's|^#define CORETIMETYPE .*|#define CORETIMETYPE unsigned long long|' \
  -e 's|^#define GETMYTIME(_t) .*|#define GETMYTIME(_t) (*_t = now_us())|' \
  -e 's|^#include "coremark.h"|#include "coremark.h"\n#include <time.h>\nstatic unsigned long long now_us(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, \&t); return t.tv_sec * 1000000ULL + t.tv_nsec / 1000; }|' \
  "$cm/simple/core_portme.c" > "$port/core_portme.c"
cmsrc="$cm/core_list_join.c $cm/core_main.c $cm/core_matrix.c $cm/core_state.c $cm/core_util.c $port/core_portme.c"
cmdef="-I$cm -I$port -DITERATIONS=0 -DPERFORMANCE_RUN=1"
# shellcheck disable=SC2086
native $cmdef -DFLAGS_STR='"-O2 native"' $cmsrc -o "$o/native/coremark"
# shellcheck disable=SC2086
wasm $cmdef -DFLAGS_STR='"-O2 wasm32-wasip1"' $cmsrc -o "$o/wasm/coremark.wasm"

# The Rust workloads: natively with the toolchain tools/tree uses, and to
# wasm32-wasip1 with the features above and a 1 MiB stack. Rust's std for
# wasm32-wasip1 comes built with reference types, whose call_indirect names
# its table in a 5-byte LEB that an MVP decoder refuses;
# --compress-relocations writes it as the MVP's one zero byte.
# STARTUP_FUNCS sets startup's size (289, the default: about 200 KB).
wl=$WB/workloads/Cargo.toml
CARGO_TARGET_ARMV5TE_UNKNOWN_LINUX_MUSLEABI_LINKER=$ROOT/tools/zigcc \
  CARGO_TARGET_ARMV5TE_UNKNOWN_LINUX_MUSLEABI_RUSTFLAGS="-C link-self-contained=no -C target-cpu=arm926ej-s" \
  cargo build --release --manifest-path "$wl" --target armv5te-unknown-linux-musleabi --target-dir "$B/cargo"
CARGO_TARGET_WASM32_WASIP1_RUSTFLAGS="-C target-cpu=mvp -C target-feature=$WASM_RUST_FEATURES -C link-arg=-zstack-size=1048576 -C link-arg=--compress-relocations" \
  cargo build --release --manifest-path "$wl" --target wasm32-wasip1 --target-dir "$B/cargo"
for n in float fb startup empty; do
  cp "$B/cargo/armv5te-unknown-linux-musleabi/release/$n" "$o/native/$n"
  cp "$B/cargo/wasm32-wasip1/release/$n.wasm" "$o/wasm/$n.wasm"
done

# Soft float, ARMv5TE, the ARM926EJ-S: the runtime is built for the same.
WAMRC_FLAGS=${WAMRC_FLAGS:---target=armv5te --target-abi=eabi --cpu=arm926ej-s}
for f in "$o"/wasm/*.wasm; do
  n=$(basename "$f" .wasm)
  # shellcheck disable=SC2086
  "$B/wamrc/wamrc" $WAMRC_FLAGS -o "$o/aot/$n.aot" "$f" > "$B/gen/wamrc-$n.log" 2>&1 \
    || { cat "$B/gen/wamrc-$n.log"; exit 1; }
done
# Without software bounds checks: unsafe on 32-bit ARM, which has no guard
# pages to catch what they would; only to measure what they cost.
mkdir -p "$o/aot-nobc"
for n in fb float coremark; do
  # shellcheck disable=SC2086
  "$B/wamrc/wamrc" $WAMRC_FLAGS --bounds-checks=0 -o "$o/aot-nobc/$n.aot" "$o/wasm/$n.wasm" > "$B/gen/wamrc-nobc-$n.log" 2>&1 \
    || { cat "$B/gen/wamrc-nobc-$n.log"; exit 1; }
done
ls -l "$o"/native "$o"/wasm "$o"/aot
