#!/usr/bin/env python3
"""Build the QEMU -device loader container for the kernel sideloader.

The container is loaded at 0xc2000000.  Layout:

    0x00  "NEOCHUM1" + version/header size + offsets/sizes
    0x1000  Linux zImage
    ...     initramfs (gzipped cpio), unless --no-initramfs

The sideloader copies the zImage to 0xc2000000 and, when the container has
one, the initramfs to 0xc3000000.  That gives each a non-overlapping window
below ATAGS at 0xc3f00000: 16 MiB for the container/kernel and 15 MiB for
the initramfs.

With --no-initramfs the zImage uses the initramfs built into it; that is the
sideload recovery image.  No NAND is used or written.
"""
import struct
import sys

BASE = 0xC2000000
LIMIT = 0x01000000  # 16 MiB, before the initramfs window at 0xc3000000
INITRD_LIMIT = 0x00F00000  # 15 MiB, from 0xc3000000 to ATAGS at 0xc3f00000
MAGIC = b"NEOCHUM1"

def main():
    args = sys.argv[1:]
    no_initramfs = bool(args and args[0] == "--no-initramfs")
    if no_initramfs:
        args = args[1:]
        if len(args) != 2:
            sys.exit("usage: mkpayload.py [--no-initramfs] ZIMAGE [INITRAMFS] OUT")
        zimage, out = args
        initramfs = None
    else:
        if len(args) != 3:
            sys.exit("usage: mkpayload.py [--no-initramfs] ZIMAGE [INITRAMFS] OUT")
        zimage, initramfs, out = args
    k = open(zimage, "rb").read()
    r = open(initramfs, "rb").read() if initramfs else b""
    if k[0x24:0x28] != bytes.fromhex("18286f01"):
        sys.exit("not a zImage (magic at 0x24 is not 0x016f2818)")
    if len(r) > INITRD_LIMIT:
        sys.exit(f"initramfs {len(r)} bytes does not fit the 15 MiB window")
    koff = 0x1000
    roff = (koff + len(k) + 3) & ~3 if r else 0
    total = (roff + len(r)) if r else (koff + len(k))
    if total > LIMIT:
        sys.exit(f"payload {total} bytes does not fit below 0x{LIMIT:x}")
    hdr = struct.pack(
        "<8sIIIIIIII",
        MAGIC, 1, 40, koff, len(k), roff, len(r), 0, 0,
    )
    blob = bytearray(hdr)
    blob.extend(b"\0" * (koff - len(blob)))
    blob.extend(k)
    if r:
        blob.extend(b"\0" * (roff - len(blob)))
        blob.extend(r)
    blob.extend(b"\0" * ((4 - len(blob) % 4) % 4))
    with open(out, "wb") as f:
        f.write(blob)
    if r:
        print(f"{out}: {len(blob)} bytes, zImage {len(k)} at +0x{koff:x}, "
              f"initramfs {len(r)} at +0x{roff:x}; loads at 0x{BASE:x}, "
              f"ends 0x{BASE + len(blob):x}")
    else:
        print(f"{out}: {len(blob)} bytes, zImage {len(k)} at +0x{koff:x}, "
              f"built-in initramfs; loads at 0x{BASE:x}, "
              f"ends 0x{BASE + len(blob):x}")

if __name__ == "__main__":
    main()
