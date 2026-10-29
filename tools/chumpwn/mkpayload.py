#!/usr/bin/env python3
"""Build the QEMU -device loader container for chumpwn.

The container is loaded at 0xc3800000, above the stock kernel's mem=56M
limit, so the stock 2.6.16 kernel never allocates it.  Layout:

    0x00  "CHUMPWN1" + version/header size + offsets/sizes
    0x1000  Linux 5.9.16 zImage
    ...     initramfs (gzipped cpio)

No NAND is used or written.
"""
import struct
import sys

BASE = 0xC3800000
LIMIT = 0x00700000  # keep clear of ATAGS at 0xc3f00000
MAGIC = b"CHUMPWN1"

def main():
    if len(sys.argv) != 4:
        sys.exit("usage: mkpayload.py ZIMAGE INITRAMFS OUT")
    zimage, initramfs, out = sys.argv[1:]
    k = open(zimage, "rb").read()
    r = open(initramfs, "rb").read()
    if k[0x24:0x28] != bytes.fromhex("18286f01"):
        sys.exit("not a zImage (magic at 0x24 is not 0x016f2818)")
    koff = 0x1000
    roff = (koff + len(k) + 3) & ~3
    total = roff + len(r)
    if total > LIMIT:
        sys.exit(f"payload {total} bytes does not fit below 0x{LIMIT:x}")
    hdr = struct.pack(
        "<8sIIIIIIII",
        MAGIC, 1, 40, koff, len(k), roff, len(r), 0, 0,
    )
    blob = bytearray(hdr)
    blob.extend(b"\0" * (koff - len(blob)))
    blob.extend(k)
    blob.extend(b"\0" * (roff - len(blob)))
    blob.extend(r)
    blob.extend(b"\0" * ((4 - len(blob) % 4) % 4))
    with open(out, "wb") as f:
        f.write(blob)
    print(f"{out}: {len(blob)} bytes, zImage {len(k)} at +0x{koff:x}, "
          f"initramfs {len(r)} at +0x{roff:x}; loads at 0x{BASE:x}, "
          f"ends 0x{BASE + len(blob):x}")

if __name__ == "__main__":
    main()
