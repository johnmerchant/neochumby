#!/bin/sh
# Build Linux 5.9.16 for the Chumby Classic, sideload-only
# (no MTD/NAND at all; not the flashable recovery image).
#
#   ./kernel/build.sh
#
# Downloads linux-5.9.16.tar.xz (or uses LINUX_TARBALL), applies the
# sideload config fragment and the small MX21ADS patch, and builds
# build/linux-5.9.16-mx21-sideload-only-zImage.
#
# The cross compiler is arm-none-eabi-gcc 13.3 from nixpkgs 24.11.  If
# arm-none-eabi-gcc is already on PATH, set CROSS_COMPILE=arm-none-eabi-
# and the script uses it directly.  On macOS the script also supplies the
# host ELF header and a uuid_t workaround for scripts/mod/file2alias.c.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
BUILD=${BUILD:-$ROOT/build}
SRC=${LINUX_SRC:-$BUILD/linux-5.9.16}
TARBALL=${LINUX_TARBALL:-$BUILD/linux-5.9.16.tar.xz}
OUT=${OUT:-$BUILD/linux-5.9.16-mx21-sideload-only-zImage}
URL=https://cdn.kernel.org/pub/linux/kernel/v5.x/linux-5.9.16.tar.xz
SHA=b0d7abae88e5f91893627c645e680a95c818defd1b4fcaf3e2afb4b2b6b4ab86
NIXPKGS=${NIXPKGS:-github:NixOS/nixpkgs/nixos-24.11}
JOBS=${JOBS:-$( (command -v nproc >/dev/null && nproc) || (sysctl -n hw.ncpu) || echo 4 )}

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

# No NAND platform device on MX21ADS when the NAND driver is absent.
if ! grep -q "CONFIG_MTD_NAND_MXC" "$SRC/arch/arm/mach-imx/mach-mx21ads.c"; then
    (cd "$SRC" && patch -p1 < "$HERE/0001-mx21ads-no-nand-platform-device.patch")
fi

# Darwin's <sys/types.h> already has uuid_t, and its clang has no elf.h.
HOSTCFLAGS=
if [ "$(uname -s)" = Darwin ]; then
    NEWLIB=$(nix eval --raw "$NIXPKGS#pkgsCross.arm-embedded.newlib.outPath")
    rm -f "$BUILD/host-include/elf.h"
    cp "$NEWLIB/arm-none-eabi/include/elf.h" "$BUILD/host-include/elf.h"
    chmod 644 "$BUILD/host-include/elf.h"
    if ! grep -q local_uuid_t "$SRC/scripts/mod/file2alias.c"; then
        (cd "$SRC" && patch -p1 < "$HERE/0002-macos-host-uuid.patch")
    fi
    HOSTCFLAGS="-I$BUILD/host-include"
fi

cd "$SRC"
kbuild() {
    if [ -n "${CROSS_COMPILE:-}" ] || command -v arm-none-eabi-gcc >/dev/null 2>&1; then
        make ARCH=arm CROSS_COMPILE="${CROSS_COMPILE:-arm-none-eabi-}" \
            HOSTCFLAGS="$HOSTCFLAGS" "$@"
    else
        nix shell "$NIXPKGS#pkgsCross.arm-embedded.stdenv.cc" -c sh -c \
            "make ARCH=arm CROSS_COMPILE=arm-none-eabi- HOSTCFLAGS='$HOSTCFLAGS' $*"
    fi
}
kbuild imx_v4_v5_defconfig
cat "$HERE/sideload-only.config" >> .config
kbuild olddefconfig
kbuild -j"$JOBS" zImage

cp arch/arm/boot/zImage "$OUT"
echo "sideload: $OUT (sideload-only; do not flash)"
ls -l "$OUT"
if command -v sha256sum >/dev/null; then sha256sum "$OUT"; else shasum -a 256 "$OUT"; fi
