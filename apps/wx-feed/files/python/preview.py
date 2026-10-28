#!/usr/bin/env python3
"""preview: composite a wxfeed screen the way wxshow will, to check a design on the Mac.

Usage: preview.py OUTDIR SCREEN OUT.png [var=value ...] [vol=65]
  e.g. preview.py /tmp/wxout home home.png music=1 page=0 night=0 pin=0 vol=65
"""
import struct
import sys

from PIL import Image

KEY = 0xF81F


def load(path, w, h):
    raw = open(path, "rb").read()[: w * h * 2]
    px = struct.unpack(f"<{w * h}H", raw)
    im = Image.new("RGB", (w, h))
    im.putdata([(((v >> 11) & 31) << 3, ((v >> 5) & 63) << 2, (v & 31) << 3) for v in px])
    return im, px


def main():
    out, scr, dst = sys.argv[1:4]
    state = {"music": 0, "station": 0, "night": 0, "pin": 0, "page": 0, "vol": 65}
    for kv in sys.argv[4:]:
        k, v = kv.split("=")
        state[k] = int(v)
    lines = open(f"{out}/manifest.txt").read().splitlines()
    img = None
    for ln in lines:
        p = ln.split()
        if p[:1] == ["screen"] and p[1] == scr:
            img, _ = load(f"{out}/{p[2]}", 320, 240)
    for ln in lines:
        p = ln.split()
        if not p or len(p) < 2 or p[1] != scr:
            continue
        if p[0] == "pane":
            f, w, h, x, y, var, frames = p[2], *map(int, p[3:7]), p[7], int(p[8])
            fr = min(state.get(var, 0), frames - 1)
            sheet, _ = load(f"{out}/{f}", w, h * frames)
            img.paste(sheet.crop((0, fr * h, w, fr * h + h)), (x, y))
        elif p[0] == "slider":
            e, fu, kn = p[2:5]
            w, h, x, y, t0, t1, kw, kh = map(int, p[5:13])
            empty, _ = load(f"{out}/{e}", w, h)
            full, _ = load(f"{out}/{fu}", w, h)
            knob, kpx = load(f"{out}/{kn}", kw, kh)
            kx = t0 + (t1 - t0) * state["vol"] // 100
            empty.paste(full.crop((t0, 0, kx, h)), (t0, 0))
            mask = Image.new("L", (kw, kh))
            mask.putdata([0 if v == KEY else 255 for v in kpx])
            empty.paste(knob, (kx - kw // 2, (h - kh) // 2), mask)
            img.paste(empty, (x, y))
    img.save(dst)


if __name__ == "__main__":
    main()
