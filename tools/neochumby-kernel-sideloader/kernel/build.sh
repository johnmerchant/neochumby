#!/bin/sh
# Build the Linux 5.9.16 target kernel for the 2.6.16 module handoff.
#
#   ./kernel/build.sh
#   FRAGMENT=sideload-only.config ./kernel/build.sh
#
# The default builds the Chumby Classic's display kernel: it downloads
# linux-5.9.16.tar.xz (or uses LINUX_TARBALL), applies the board patch series
# from dist/patches/linux/5.9.16, adds chumby-classic-sideload.config, and
# writes build/linux-5.9.16-chumby-zImage.  It checks the handoff's limits:
# zImage < 4 MiB, decompressed Image < 8 MiB.
#
# The cross compiler is arm-none-eabi-gcc 13.3 from nixpkgs 24.11.  If
# arm-none-eabi-gcc is already on PATH, set CROSS_COMPILE=arm-none-eabi- and
# the script uses it directly.  On macOS the script also supplies the host
# ELF header and a uuid_t workaround for scripts/mod/file2alias.c.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
REPO=$(cd "$HERE/../.." && pwd)
BUILD=${BUILD:-$ROOT/build}
FRAGMENT=${FRAGMENT:-chumby-classic-sideload.config}
TARBALL=${LINUX_TARBALL:-$BUILD/linux-5.9.16.tar.xz}
URL=https://cdn.kernel.org/pub/linux/kernel/v5.x/linux-5.9.16.tar.xz
SHA=b0d7abae88e5f91893627c645e680a95c818defd1b4fcaf3e2afb4b2b6b4ab86
NIXPKGS=${NIXPKGS:-github:NixOS/nixpkgs/nixos-24.11}
JOBS=${JOBS:-$( (command -v nproc >/dev/null && nproc) || (sysctl -n hw.ncpu) || echo 4 )}

case "$FRAGMENT" in
chumby-classic-sideload.config)
    SUFFIX=-chumby
    OUT=${OUT:-$BUILD/linux-5.9.16-chumby-zImage}
    DIST_PATCHES=$REPO/dist/patches/linux/5.9.16
    ;;
sideload-only.config)
    SUFFIX=
    OUT=${OUT:-$BUILD/linux-5.9.16-mx21-sideload-only-zImage}
    DIST_PATCHES=
    ;;
*)
    echo "build: FRAGMENT must be chumby-classic-sideload.config or sideload-only.config" >&2
    exit 2
    ;;
esac
SRC=${LINUX_SRC:-$BUILD/linux-5.9.16$SUFFIX}

mkdir -p "$BUILD" "$BUILD/host-include"
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
    mkdir -p "$SRC"
    tar xJf "$TARBALL" -C "$SRC" --strip-components=1
fi

if [ -n "$DIST_PATCHES" ]; then
    if [ ! -f "$SRC/arch/arm/mach-imx/mach-chumby-classic.c" ]; then
        for p in "$DIST_PATCHES"/*.patch; do
            echo "applying $(basename "$p")"
            (cd "$SRC" && patch -p1 -N < "$p") || exit 1
        done
    fi
else
    # No NAND platform device on MX21ADS when the NAND driver is absent.
    if ! grep -q "CONFIG_MTD_NAND_MXC" "$SRC/arch/arm/mach-imx/mach-mx21ads.c"; then
        (cd "$SRC" && patch -p1 -N < "$HERE/0001-mx21ads-no-nand-platform-device.patch")
    fi
fi

# NixOS has no /usr/bin/awk; the kernel's scripts use it as a shebang.
if [ ! -e /usr/bin/awk ]; then
    for f in "$SRC/scripts/ld-version.sh" "$SRC/scripts/ver_linux"; do
        [ -f "$f" ] || continue
        sed -i '1s|^#!/usr/bin/awk -f|#!/usr/bin/env -S awk -f|' "$f"
    done
fi

# Darwin's <sys/types.h> already has uuid_t, and its clang has no elf.h.
HOSTCFLAGS=
if [ "$(uname -s)" = Darwin ]; then
    NEWLIB=$(nix eval --raw "$NIXPKGS#pkgsCross.arm-embedded.newlib.outPath")
    rm -f "$BUILD/host-include/elf.h"
    cp "$NEWLIB/arm-none-eabi/include/elf.h" "$BUILD/host-include/elf.h"
    chmod 644 "$BUILD/host-include/elf.h"
    if ! grep -q local_uuid_t "$SRC/scripts/mod/file2alias.c"; then
        (cd "$SRC" && patch -p1 -N < "$HERE/0002-macos-host-uuid.patch")
    fi
    HOSTCFLAGS="-I$BUILD/host-include"
fi

cd "$SRC"
kbuild() {
    if [ -n "${CROSS_COMPILE:-}" ] || command -v arm-none-eabi-gcc >/dev/null 2>&1; then
        make ARCH=arm CROSS_COMPILE="${CROSS_COMPILE:-arm-none-eabi-}" \
            HOSTCFLAGS="$HOSTCFLAGS" "$@"
    else
        nix shell "$NIXPKGS#gnumake" "$NIXPKGS#gcc14" "$NIXPKGS#bison" \
            "$NIXPKGS#flex" "$NIXPKGS#bc" "$NIXPKGS#xz" "$NIXPKGS#gawk" \
            "$NIXPKGS#pkgsCross.arm-embedded.stdenv.cc" -c \
            make ARCH=arm CROSS_COMPILE=arm-none-eabi- \
            HOSTCFLAGS="$HOSTCFLAGS" "$@"
    fi
}
kbuild imx_v4_v5_defconfig
cat "$HERE/$FRAGMENT" >> .config
kbuild olddefconfig
kbuild -j"$JOBS" zImage

mkdir -p "$(dirname "$OUT")"
cp arch/arm/boot/zImage "$OUT"
zsize=$(wc -c < "$OUT")
isize=$(wc -c < arch/arm/boot/Image)
echo "kernel: $OUT (module handoff; do not flash)"
echo "kernel: zImage $zsize bytes (limit 4194304), Image $isize bytes (limit 8388608)"
if [ "$zsize" -gt 4194304 ] || [ "$isize" -gt 8388608 ]; then
    echo "build: the kernel does not fit the module's reserved blocks" >&2
    exit 1
fi
if command -v sha256sum >/dev/null; then sha256sum "$OUT"; else shasum -a 256 "$OUT"; fi
