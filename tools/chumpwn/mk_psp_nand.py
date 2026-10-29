#!/usr/bin/env python3
"""Make a scratch NAND image with a new PSP partition.

The input image is the sanitized Chumby NAND dump (528-byte records).  Only
the PSP partition (offset 0x100000, 2 MiB) is replaced; every other byte,
including OOB, is copied unchanged.  The input is never opened for writing.
"""
import hashlib
import math
import sys

PSP_OFF = 0x100000
PAGE = 512
SPARE = 16
RECORD = PAGE + SPARE

def put_part(nand, off, data):
    start = off + (off // PAGE) * SPARE
    for i in range(math.ceil(len(data) / PAGE)):
        o = start + i * RECORD
        chunk = data[i * PAGE:(i + 1) * PAGE]
        nand[o:o + len(chunk)] = chunk

def main():
    if len(sys.argv) != 4:
        sys.exit("usage: mk_psp_nand.py NAND_IN PSP.jffs2 NAND_OUT")
    src, psp, out = sys.argv[1:]
    nand = bytearray(open(src, "rb").read())
    if len(nand) != 0x4200000:
        sys.exit(f"{src}: expected 0x4200000 bytes, got {len(nand):#x}")
    put_part(nand, PSP_OFF, open(psp, "rb").read())
    with open(out, "wb") as f:
        f.write(nand)
    print(f"{out}: {len(nand)} bytes, sha256 {hashlib.sha256(nand).hexdigest()}")

if __name__ == "__main__":
    main()
