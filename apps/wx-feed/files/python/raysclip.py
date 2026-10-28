#!/usr/bin/env python3
"""raysclip: turn a green-screen light-rays clip into the aurora's light frames.

The aurora plays the real footage as additive light (wxshow `glowseq`). This one-off converter
does only the clip-wide steps, and writes the result as one greyscale PNG strip that the feeder
reads on every render (aurora.py applies the bottom fade and the black point, then packs the
frames into aurora_seq_NNN.l8):

  key       the light is how far a pixel lifts R and B above the green plate: min(R, B) minus
            that pixel's darkest min(R, B) over the whole clip (the plate is green, so its R and
            B sit near 0; white light lifts all three channels)
  crop      the right-hand 4:3 of the 16:9 frame (x 480..1920), scaled to 320x240, so the beams
            enter from the top right as on the Chumby
  trim      drop the opening white flash (the clip blooms to white for its first ~1 s)
  loop      crossfade the last LOOP_XFADE s into the first, so the end flows into the start

Source: any green-screen light-rays clip you have the right to use, 1920x1080 (the crop
assumes 16:9). The output is footage, so it is never committed: keep it in a directory named
by NEOCHUMBY_WX_ASSETS (see wxassets.py), where aurora.py finds it. Needs ffmpeg and numpy
(on the machine that converts the clip; the renderer itself needs only Pillow).

Usage: raysclip.py SOURCE_VIDEO [OUT_PNG]   (default OUT_PNG: ./raysclip.png)
"""
import os
import subprocess
import sys

import numpy as np
from PIL import Image, PngImagePlugin

W, H = 320, 240
FPS = 12                       # the engine rebuilds glows at 12.5 fps; 12 keeps FPS an integer
CROP = "crop=1440:1080:480:0"  # right-hand 4:3 of the 1920x1080 frame
START = 1.2                    # s: after the opening white flash has gone
LOOP_XFADE = 1.0               # s of tail crossfaded into the head


def frames(src):
    ff = os.environ.get("FFMPEG", "ffmpeg")
    raw = subprocess.run([ff, "-v", "error", "-i", src, "-an", "-vf",
                          f"fps={FPS},{CROP},scale={W}:{H}:flags=area",
                          "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
                         check=True, stdout=subprocess.PIPE).stdout
    return np.frombuffer(raw, np.uint8).reshape(-1, H, W, 3)


def main():
    src = sys.argv[1]
    out = sys.argv[2] if len(sys.argv) > 2 else "raysclip.png"
    rgb = frames(src)[int(round(START * FPS)):].astype(np.int16)
    minrb = np.minimum(rgb[..., 0], rgb[..., 2])
    plate = minrb.min(axis=0)                       # each pixel's darkest: the bare green plate
    light = np.clip(minrb - plate, 0, 255).astype(np.float32)
    x = int(round(LOOP_XFADE * FPS))
    n = len(light) - x                              # loop length
    seq = light[:n].copy()
    for i in range(x):                              # head fades in while the tail fades out
        a = (i + 0.5) / x
        seq[i] = light[i] * a + light[n + i] * (1 - a)
    strip = np.rint(seq).astype(np.uint8).reshape(n * H, W)
    meta = PngImagePlugin.PngInfo()
    meta.add_text("fps", str(FPS))
    meta.add_text("source", os.path.basename(src))
    Image.fromarray(strip, "L").save(out, pnginfo=meta, optimize=True)
    print(f"{out}: {n} frames at {FPS} fps ({n / FPS:.2f} s loop), peak {int(seq.max())}")


if __name__ == "__main__":
    main()
