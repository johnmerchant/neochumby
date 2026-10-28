"""Converts a Chumby Classic framebuffer dump (320x240 RGB565 LE) to PNG.

    python3 -I fb2png.py in.raw out.png
"""
import struct
import sys
import zlib

W, H = 320, 240


def main(src, dst):
    raw = open(src, "rb").read()[: W * H * 2]
    rows = []
    for y in range(H):
        row = bytearray([0])
        for x in range(W):
            (p,) = struct.unpack_from("<H", raw, (y * W + x) * 2)
            r, g, b = (p >> 11) & 0x1F, (p >> 5) & 0x3F, p & 0x1F
            row += bytes([r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2])
        rows.append(bytes(row))

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(b"".join(rows), 9))
    png += chunk(b"IEND", b"")
    open(dst, "wb").write(png)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
