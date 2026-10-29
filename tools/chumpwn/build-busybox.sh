#!/bin/sh
# Build a static ARMv5TE BusyBox 1.36.1 for the initramfs.
#
#   ./build-busybox.sh
#
# Uses zig cc (nixpkgs#zig) rather than a Linux cross gcc.  Two small
# workarounds for zig's armv5te compiler-rt and musl headers are applied:
# the tc applet is disabled, and libbb gets the __sync_* helpers that
# compiler-rt references on ARMv5.  No binary is committed.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
BUILD=${BUILD:-$HERE/build}
TARBALL=${BUSYBOX_TARBALL:-$BUILD/busybox-1.36.1.tar.bz2}
SRC=${BUSYBOX_SRC:-$BUILD/busybox-1.36.1}
OUT=${OUT:-$BUILD/busybox}
URL=https://busybox.net/downloads/busybox-1.36.1.tar.bz2
SHA=b8cc24c9574d809e7279c3be349795c5d5ceb6fdf19ca709f80cde50e47de314
JOBS=${JOBS:-$( (command -v nproc >/dev/null && nproc) || (sysctl -n hw.ncpu) || echo 4 )}

mkdir -p "$BUILD"
if [ ! -d "$SRC" ]; then
    if [ ! -f "$TARBALL" ]; then
        echo "fetching $URL"
        curl -L --fail -o "$TARBALL" "$URL"
    fi
    if command -v sha256sum >/dev/null; then
        echo "$SHA  $TARBALL" | sha256sum -c -
    else
        echo "$SHA  $TARBALL" | shasum -a 256 -c -
    fi
    tar xjf "$TARBALL" -C "$BUILD"
fi

cd "$SRC"
make defconfig
python3 - "$SRC" "$HERE" <<'PY'
import os, sys
src, here = sys.argv[1:3]
p = os.path.join(src, ".config")
s = open(p).read()
s = s.replace("# CONFIG_STATIC is not set", "CONFIG_STATIC=y")
s = s.replace("CONFIG_TC=y", "# CONFIG_TC is not set")
s = s.replace('CONFIG_EXTRA_LDLIBS=""', 'CONFIG_EXTRA_LDLIBS=""')
open(p, "w").write(s)
# compiler-rt on armv5te references these; provide simple single-core
# implementations in libbb.
s = open(os.path.join(src, "libbb", "Kbuild.src")).read()
if "armv5_atomics.o" not in s:
    s = s.replace("lib-y:=\n", "lib-y:=\nlib-y += armv5_atomics.o\n")
    open(os.path.join(src, "libbb", "Kbuild.src"), "w").write(s)
# zig's lld does not accept the GNU ld diagnostics used by trylink.
s = open(os.path.join(src, "scripts", "trylink")).read()
s = s.replace('echo "-Wl,--warn-common -Wl,-Map,$EXE.map -Wl,--verbose"', 'echo ""')
open(os.path.join(src, "scripts", "trylink"), "w").write(s)
PY
cp "$HERE/armv5_atomics.S" libbb/armv5_atomics.S

yes "" | make oldconfig >/dev/null
nix shell nixpkgs#zig -c sh -c \
    "make -j$JOBS CC='zig cc -target arm-linux-musleabi -mcpu=arm926ej_s' HOSTCC=cc AR='zig ar' RANLIB=true busybox_unstripped"
cp busybox_unstripped "$OUT"
chmod 755 "$OUT"
file "$OUT"
if command -v sha256sum >/dev/null; then sha256sum "$OUT"; else shasum -a 256 "$OUT"; fi
