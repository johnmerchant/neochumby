#!/bin/sh
# Builds the workloads, each twice from the same C: natively for the Chumby
# (static musl, ARM926EJ-S, soft float) and to wasm32-wasip1 (zig's
# wasi-libc; no threads, no SIMD), then the .wasm files to WAMR AOT with
# wamrc ($WAMRC_FLAGS). Into build/out/{native,wasm,aot}.
#   coremark   CoreMark (build/src/coremark), ITERATIONS=0: it calibrates
#              itself to run for at least 10 s
#   float      bench/float.c, the soft-float kernel
#   fb         bench/fb.c, the framebuffer kernel
#   startup    bench/genstart.py's ~200 KB module, whose main does nothing
#   empty      an empty main, for the runtime's own start-up cost
set -eu
. "$(dirname "$0")/env.sh"
o=$B/out
mkdir -p "$o/native" "$o/wasm" "$o/aot" "$B/gen"
# Wasm features: the MVP plus what wasi-libc's build expects and every
# runtime here implements (no SIMD, no threads, no multivalue).
WASM_CPU=${WASM_CPU:-mvp+bulk_memory+sign_ext+mutable_globals+nontrapping_fptoint}
native() { "$WB/tools/cc" -O2 -s "$@"; }
# A 1 MiB stack, not zig's 16 MiB: initial memory is what some runtimes commit.
wasm() { zig cc -target wasm32-wasi -mcpu="$WASM_CPU" -O2 -s -Wl,-z,stack-size=1048576 "$@"; }

# CoreMark's `simple` port, with clock() (CPU time; and an emulated process
# clock under WASI) swapped for CLOCK_MONOTONIC in microseconds.
cm=$B/src/coremark
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

python3 -I "$WB/bench/genstart.py" "$B/gen/startup.c" "${STARTUP_FUNCS:-340}"
echo 'int main(void) { return 0; }' > "$B/gen/empty.c"
for w in "$WB/bench/float.c" "$WB/bench/fb.c" "$B/gen/startup.c" "$B/gen/empty.c"; do
  n=$(basename "$w" .c)
  native "$w" -o "$o/native/$n" -lm
  wasm "$w" -o "$o/wasm/$n.wasm" -lm
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
