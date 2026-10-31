#!/bin/sh
# Runs BENCH on the emulated Chumby (Linux 2.6.16, dev/emulate's classic
# profile unless NEOCHUMBY_EMU_PROFILE says otherwise, with no network):
# boots build/tree, opens BENCH from home and waits for it to finish.
# Timings there mean nothing; it shows what runs.
#   tools/emu.sh [WAIT_SECS]     the log in build/emu.log, the screen in
#                                build/emu.png
set -eu
. "$(dirname "$0")/env.sh"
export NEOCHUMBY_EMU_PROFILE=${NEOCHUMBY_EMU_PROFILE:-classic}
# This checkout's own, so that two checkouts' runs don't share a machine.
state=$B/emu
emu() { "$ROOT/dev/emulate" --state "$state" "$@"; }
emu stop >/dev/null 2>&1 || true
# A stopped machine can leave its control socket behind.
rm -rf "$state"
emu start "$B/tree"
emu wait log 'neochumby: opening home' 300
emu wait screen 30 || true
# BENCH is home's first row (the tree has no other app).
emu tap 160 62
emu wait log 'wasmbench: running' 60
r=0
emu wait log 'wasmbench: done' "${1:-3600}" || r=$?
emu shot "$B/emu.png" || true
emu log > "$B/emu.log" || true
emu stop || true
grep 'wasmbench' "$B/emu.log" | grep -v '|' | tail -80
exit "$r"
