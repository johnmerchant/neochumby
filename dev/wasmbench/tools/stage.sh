#!/bin/sh
# Fills dev/wasmbench/files/ (copied into the app's directory by tools/tree)
# from build/: the runtimes, stripped, the workloads in each form, and the
# runtimes and plan files the app reads. Run the other tools/*.sh first.
#   tools/stage.sh [quick]    quick: smaller runs, for the emulator
set -eu
. "$(dirname "$0")/env.sh"
f=$WB/files
rm -rf "$f"
mkdir -p "$f"
arm=$B/cargo/armv5te-unknown-linux-musleabi/release
cp "$B/wamr-aot/iwasm" "$f/iwasm-aot"
cp "$B/wamr-interp/iwasm" "$f/iwasm-interp"
cp "$B/wasm3/wasm3" "$f/wasm3"
cp "$arm/wasmi-run" "$f/wasmi-run"
[ ! -f "$arm/wasmtime-run" ] || cp "$arm/wasmtime-run" "$f/wasmtime-run"
for b in "$f"/*; do llvm-strip "$b"; done
cp -R "$B/out/native" "$B/out/wasm" "$B/out/aot" "$B/out/aot-nobc" "$f/"
[ ! -d "$B/out/cwasm" ] || cp -R "$B/out/cwasm" "$f/"
cat > "$f/runtimes" <<'EOF'
# NAME ARGV ({w}: the workload)
native ./native/{w}
wamr-aot ./iwasm-aot ./aot/{w}.aot
wamr-aot-nobc ./iwasm-aot ./aot-nobc/{w}.aot
wamr-interp ./iwasm-interp ./wasm/{w}.wasm
wasmi ./wasmi-run ./wasm/{w}.wasm
wasm3 ./wasm3 ./wasm/{w}.wasm
EOF
[ ! -f "$f/wasmtime-run" ] || echo 'wasmtime-pulley ./wasmtime-run ./cwasm/{w}.cwasm' >> "$f/runtimes"
[ ! -f "$B/wamr-classic/iwasm" ] || { cp "$B/wamr-classic/iwasm" "$f/iwasm-classic" && llvm-strip "$f/iwasm-classic"; }
if [ "${1:-}" = interp ]; then
  # The WAMR interpreters alone: the fast one's SIGILL on the device, and
  # the classic one beside it.
  cat > "$f/runtimes" <<'EOF'
wamr-interp ./iwasm-interp ./wasm/{w}.wasm
wamr-classic ./iwasm-classic ./wasm/{w}.wasm
EOF
  cat > "$f/plan" <<'EOF'
float 3 5000 20
fb 3 100
coremark 3
EOF
elif [ "${1:-}" = quick ]; then
  cat > "$f/plan" <<'EOF'
# WORKLOAD RUNS ARGS (quick: functional checks)
empty 1
startup 1
float 1 200 2
fb 1 5
coremark 1
EOF
else
  cat > "$f/plan" <<'EOF'
# WORKLOAD RUNS ARGS
empty 5
startup 5
float 3 5000 20
fb 3 100
coremark 3
EOF
fi
du -sh "$f"
ls -l "$f"
