#!/bin/sh
# Build a gzipped cpio from userspace/ and a BusyBox binary.
#
#   BUSYBOX=/path/to/arm-busybox ./build-initramfs.sh [OUT]
#
# The userspace/ directory is the placeholder tree; replacing it (or the
# BusyBox binary) is the whole userspace swap.  No binary is committed.
set -eu
cd "$(dirname "$0")"
OUT=${1:-build/initramfs.cpio.gz}
BUSYBOX=${BUSYBOX:-build/busybox}
if [ ! -x "$BUSYBOX" ]; then
    echo "build-initramfs: no BusyBox at $BUSYBOX (set BUSYBOX=...)" >&2
    exit 1
fi
rm -rf build/initramfs-root
mkdir -p build/initramfs-root/bin
cp -a userspace/. build/initramfs-root/
cp "$BUSYBOX" build/initramfs-root/bin/busybox
chmod 755 build/initramfs-root/bin/busybox
# The kernel executes /init directly, so its #!/bin/sh must already exist.
ln -sf busybox build/initramfs-root/bin/sh
mkdir -p "$(dirname "$OUT")"
python3 mkinitramfs.py build/initramfs-root "$OUT"
