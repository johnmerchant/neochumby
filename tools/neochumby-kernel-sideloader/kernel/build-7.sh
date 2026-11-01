#!/bin/sh
# Build the Linux 7.3.1 target kernel for the 2.6.16 module handoff.
#
#   ./kernel/build-7.sh
#   ROOTFS=/path/to/sideload-rootfs.cpio.gz ./kernel/build-7.sh
#
# The kernel keeps the core, panel console and watchdog built in; USB,
# WiFi, sound, touch, NFS and VFAT are modules for the initramfs.  It
# downloads linux-7.3.1.tar.xz (or uses LINUX_TARBALL), applies the stable
# track's i.MX21/Chumby patches, adds the stable options and
# chumby-classic-7-full.config, builds the zImage, appends the Chumby DTB,
# builds the modules, and, with ROOTFS, packs them into the dist initramfs.
# It checks the handoff's limits: zImage < 4 MiB, Image < 8 MiB, initramfs
# < 4 MiB.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
REPO=$(cd "$HERE/../../.." && pwd)
BUILD=${BUILD:-$ROOT/build}
SRC=${LINUX_SRC:-$BUILD/linux-7.3.1-chumby}
TARBALL=${LINUX_TARBALL:-$BUILD/linux-7.3.1.tar.xz}
OUT=${OUT:-$BUILD/linux-7.3.1-chumby-zImage}
INITRAMFS_OUT=${INITRAMFS_OUT:-$BUILD/linux-7.3.1-chumby-initramfs.cpio.gz}
URL=https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.3.1.tar.xz
SHA=fa595aad93734f829c963c2a90ef7c3673224fd7fdae7b49406e397cde2de231
NIXPKGS=${NIXPKGS:-github:NixOS/nixpkgs/nixos-24.11}
JOBS=${JOBS:-$( (command -v nproc >/dev/null && nproc) || (sysctl -n hw.ncpu) || echo 4 )}
PATCHES=$REPO/dist/kernel/stable/linux
STABLE=$REPO/dist/kernel/stable/linux.fragment
TRIM=$HERE/chumby-classic-7-full.config
MODSTAGE=$BUILD/modules-7.3.1

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

if [ ! -f "$SRC/arch/arm/boot/dts/nxp/imx/imx21-chumby-classic.dts" ]; then
    for p in "$PATCHES"/*.patch; do
        echo "applying $(basename "$p")"
        (cd "$SRC" && patch -p1 -N < "$p") || exit 1
    done
fi

# NixOS has no /usr/bin/awk; the kernel's scripts use it as a shebang.
if [ ! -e /usr/bin/awk ]; then
    for f in "$SRC/scripts/ld-version.sh" "$SRC/scripts/ver_linux"; do
        [ -f "$f" ] || continue
        sed -i '1s|^#!/usr/bin/awk -f|#!/usr/bin/env -S awk -f|' "$f"
    done
fi

# certs/extract-cert is a host tool linked against OpenSSL.  nix shell puts
# the binary on PATH but not the headers or the library path.
HOSTCFLAGS=
if command -v nix >/dev/null 2>&1; then
    OPENSSL_DEV=$(nix eval --raw "$NIXPKGS#openssl.dev")
    OPENSSL_OUT=$(nix eval --raw "$NIXPKGS#openssl.out")
    HOSTCFLAGS="-I$OPENSSL_DEV/include"
    export HOSTLDFLAGS="-L$OPENSSL_OUT/lib"
fi

# Darwin's <sys/types.h> already has uuid_t, and its clang has no elf.h.
if [ "$(uname -s)" = Darwin ]; then
    NEWLIB=$(nix eval --raw "$NIXPKGS#pkgsCross.arm-embedded.newlib.outPath")
    rm -f "$BUILD/host-include/elf.h"
    cp "$NEWLIB/arm-none-eabi/include/elf.h" "$BUILD/host-include/elf.h"
    chmod 644 "$BUILD/host-include/elf.h"
    HOSTCFLAGS="$HOSTCFLAGS -I$BUILD/host-include"
fi

cd "$SRC"
kbuild() {
    if [ -n "${CROSS_COMPILE:-}" ] || command -v arm-none-eabi-gcc >/dev/null 2>&1; then
        make ARCH=arm CROSS_COMPILE="${CROSS_COMPILE:-arm-none-eabi-}" \
            HOSTCFLAGS="$HOSTCFLAGS" "$@"
    else
        nix shell "$NIXPKGS#gnumake" "$NIXPKGS#gcc14" "$NIXPKGS#bison" \
            "$NIXPKGS#flex" "$NIXPKGS#bc" "$NIXPKGS#xz" "$NIXPKGS#gawk" \
            "$NIXPKGS#openssl" "$NIXPKGS#openssl.dev" "$NIXPKGS#kmod" "$NIXPKGS#cpio" "$NIXPKGS#gzip" \
            "$NIXPKGS#pkgsCross.arm-embedded.stdenv.cc" -c \
            make ARCH=arm CROSS_COMPILE=arm-none-eabi- \
            HOSTCFLAGS="$HOSTCFLAGS" "$@"
    fi
}
kbuild imx_v4_v5_defconfig
cat "$STABLE" >> .config
cat "$TRIM" >> .config
kbuild olddefconfig
kbuild -j"$JOBS" zImage
kbuild -j"$JOBS" modules
kbuild -j"$JOBS" dtbs

rm -rf "$MODSTAGE"
kbuild modules_install INSTALL_MOD_PATH="$MODSTAGE" INSTALL_MOD_STRIP=1

DTB=arch/arm/boot/dts/nxp/imx/imx21-chumby-classic.dtb
[ -f "$DTB" ] || { echo "build: no $DTB" >&2; exit 1; }
mkdir -p "$(dirname "$OUT")"
cat arch/arm/boot/zImage "$DTB" > "$OUT"
zsize=$(wc -c < "$OUT")
isize=$(wc -c < arch/arm/boot/Image)
echo "kernel: $OUT (7.3.1 module handoff; do not flash)"
echo "kernel: zImage+DTB $zsize bytes (limit 4194304), Image $isize bytes (limit 12000000)"
if [ "$zsize" -gt 4194304 ] || [ "$isize" -gt 12000000 ]; then
    echo "build: the kernel does not fit the module's reserved blocks" >&2
    exit 1
fi
if command -v sha256sum >/dev/null; then sha256sum "$OUT"; else shasum -a 256 "$OUT"; fi

if [ -n "${ROOTFS:-}" ]; then
    [ -f "$ROOTFS" ] || { echo "build: no $ROOTFS" >&2; exit 1; }
    INITRD=$BUILD/initramfs-7.3.1
    rm -rf "$INITRD"
    mkdir -p "$INITRD"
    case "$ROOTFS" in
    *.xz) xzcat "$ROOTFS" ;;
    *) zcat "$ROOTFS" ;;
    esac | (cd "$INITRD" && cpio -idm --quiet)
    mkdir -p "$INITRD/lib/modules"
    cp -a "$MODSTAGE/lib/modules/." "$INITRD/lib/modules/"
    depmod -b "$INITRD" 7.3.1-mx21-sideload
    (cd "$INITRD" && find . | cpio -o -H newc --quiet | gzip -9 > "$INITRAMFS_OUT")
    rsize=$(wc -c < "$INITRAMFS_OUT")
    echo "initramfs: $INITRAMFS_OUT $rsize bytes (limit 4194304)"
    if [ "$rsize" -gt 4194304 ]; then
        echo "build: the initramfs does not fit the module's initrd block" >&2
        exit 1
    fi
    if command -v sha256sum >/dev/null; then sha256sum "$INITRAMFS_OUT"; else shasum -a 256 "$INITRAMFS_OUT"; fi
fi
