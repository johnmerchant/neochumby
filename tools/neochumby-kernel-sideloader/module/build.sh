#!/bin/sh
# Build the 2.6.16 module that hands off to the 5.9.16 kernel.
#
#   KDIR=/path/to/linux-2.6.16 ./module/build.sh
#
# KDIR must be a 2.6.16 tree configured like the stock Chumby kernel.  The
# stock kernel's vermagic is 2.6.16-csb ARMv5 gcc-4.3; a newer compiler's
# differs, so after the build the string is set to the stock one.
#
# CROSS_COMPILE (default arm-none-eabi-) and TARGET_VERMAGIC override the
# defaults.  The result is module/sideload.ko.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
KDIR=${KDIR:?set KDIR to a configured Linux 2.6.16 tree}
CROSS_COMPILE=${CROSS_COMPILE:-arm-none-eabi-}
TARGET_VERMAGIC=${TARGET_VERMAGIC:-"2.6.16-csb ARMv5 gcc-4.3"}
JOBS=${JOBS:-1}

make -C "$KDIR" ARCH=arm CROSS_COMPILE="$CROSS_COMPILE" \
    M="$HERE" -j"$JOBS" modules

python3 - "$HERE/sideload.ko" "$TARGET_VERMAGIC" <<'PY'
import re
import sys

path, target = sys.argv[1], sys.argv[2].encode()
data = open(path, "rb").read()
found = re.search(rb"vermagic=([^\0]*)", data)
if not found:
    sys.exit(f"{path}: no vermagic")
old = found.group(1)
if old == target:
    print(f"vermagic: {old.decode()}")
    sys.exit(0)
if len(target) > len(old):
    sys.exit(f"{path}: target vermagic {target!r} is longer than {old!r}")
data = data[:found.start(1)] + target + b"\0" * (len(old) - len(target)) + data[found.end(1):]
with open(path, "wb") as f:
    f.write(data)
print(f"vermagic: {old.decode()} -> {target.decode()}")
PY

if command -v modinfo >/dev/null 2>&1; then
    modinfo "$HERE/sideload.ko" | grep '^vermagic:'
else
    strings "$HERE/sideload.ko" | grep '^vermagic='
fi
ls -l "$HERE/sideload.ko"
