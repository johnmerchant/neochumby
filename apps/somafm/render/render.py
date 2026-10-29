#!/usr/bin/env python3
"""render.py: draws neochumby-somafm's screen, at build time, into files/.

The screen is Matt's 2008 Pandora-on-Chumby Music screen from
neochumby-classic (screens.py, music()), as its own app: the station list on
the left, the now-playing card on the right, and play/pause, skip, the
volume slider and DONE along the bottom. Every picture is RGB565, the
screen's own format, so the app only copies rectangles (src/screen.rs). A
dark set beside it is shown at night.

    python3 -m venv /tmp/venv && /tmp/venv/bin/pip install Pillow==13.0.0
    /tmp/venv/bin/python apps/somafm/render/render.py [--png DIR]

writes:
    files/screen.txt         where each picture goes on the screen, and the
                             tap zones (src/screen.rs reads it)
    files/day/*.rgb565       the pictures; files/night/*.rgb565 the dark set
    render/rendered-from.txt a hash of each input (this script, the fonts,
                             stations.txt), so that a test fails if one
                             changes and files/ isn't made again
and, with --png, the whole screen as PNGs, for looking at.

Python and Pillow are needed only to run this, never to build or run the
app: files/ is committed. The tiles are our own drawings (a colour, a
glyph and the station's name), never SomaFM's artwork (docs/legal.md). The
fonts are the bundled open ones in fonts/, with their licences.
"""
import argparse
import math
import os
import sys

from PIL import Image, ImageChops, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
CRATE = os.path.dirname(HERE)
FILES = os.path.join(CRATE, "files")
FONTS = os.path.join(HERE, "fonts")

W, H = 320, 240
SS = 3                                   # drawn at 3x, then scaled down: smooth edges
ROUND = os.path.join(FONTS, "SNPro-Bold.otf")         # stands in for Arial Rounded Bold
VERD = os.path.join(FONTS, "DejaVuSans.ttf")          # stands in for Verdana
VERDB = os.path.join(FONTS, "DejaVuSans-Bold.ttf")
KEY = (255, 0, 255)                      # 0xF81F: transparent in the knob

# Where things go, in screen pixels: classic's music() layout.
ROWS = (7, 43, 168, 27)                  # x, y, w, h of the first station row
CARD = (183, 27, 130, 178)               # the now-playing card
PLAY = (8, 212, 26, 26)                  # play/pause
SKIP = (52, 225)                         # the skip button's centre (drawn in the background)
SLIDER = (70, 212, 186, 26, 10, 176)     # x, y, w, h, and the track's ends within it
KNOB = 24
HOTS = [("play", 2, 210, 34, 30), ("skip", 36, 210, 32, 30), ("done", 256, 210, 62, 30)]
ART = 92                                 # the tile, square, on the card

# The day colours are classic's Music screen; the night ones follow its Home
# screen's dark theme (screens.py NIGHT).
DAY = dict(
    bg=((236, 238, 242), (214, 218, 226)), bar=((250, 250, 252), (222, 226, 234)), bar_line=(170, 175, 190),
    soma=(30, 40, 70), fm=(200, 60, 40), tag=(120, 125, 140), panel=(255, 255, 255), card=(250, 251, 253),
    edge=(170, 175, 190), head=((232, 234, 240), (208, 212, 222)), head_text=(70, 75, 95),
    row_text=(40, 45, 65), row_line=(225, 228, 236), sel=((110, 150, 230), (60, 100, 200)), sel_text=(255, 255, 255),
    title=(30, 40, 70), desc=(80, 85, 105), frame=(255, 255, 255), frame_edge=(160, 165, 180),
    transport=((205, 210, 220), (175, 180, 194)), transport_line=(150, 155, 170),
    pill=(245, 246, 250), pill_edge=(120, 125, 145), pill_text=(50, 55, 75),
    ring=(255, 255, 255), rim=(60, 90, 170), face=((205, 222, 255), (150, 180, 240)), gloss=(232, 240, 255),
    ink=(35, 60, 130), track=(205, 212, 232), track_full=(70, 110, 200), track_edge=(110, 130, 180), tile_dim=1.0)
NIGHT = dict(
    bg=((14, 19, 44), (24, 32, 70)), bar=((30, 40, 84), (18, 24, 54)), bar_line=(48, 62, 115),
    soma=(170, 190, 245), fm=(230, 110, 90), tag=(110, 128, 190), panel=(20, 27, 60), card=(22, 30, 64),
    edge=(48, 62, 115), head=((40, 54, 108), (28, 38, 82)), head_text=(150, 172, 238),
    row_text=(170, 186, 235), row_line=(34, 44, 88), sel=((58, 84, 160), (36, 56, 124)), sel_text=(235, 240, 255),
    title=(190, 205, 250), desc=(130, 146, 200), frame=(40, 52, 100), frame_edge=(58, 78, 145),
    transport=((30, 40, 84), (16, 22, 52)), transport_line=(48, 62, 115),
    pill=(36, 48, 96), pill_edge=(80, 105, 185), pill_text=(190, 205, 250),
    ring=(18, 24, 54), rim=(80, 105, 185), face=((52, 72, 138), (26, 40, 92)), gloss=(78, 100, 165),
    ink=(205, 218, 255), track=(36, 46, 90), track_full=(80, 110, 200), track_edge=(70, 90, 160), tile_dim=0.7)


def f(path, size):
    return ImageFont.truetype(path, size * SS)


def big(img):
    return img.resize((img.size[0] * SS, img.size[1] * SS))


def small(img):
    return img.resize((img.size[0] // SS, img.size[1] // SS), Image.LANCZOS)


def grad(d, box, top, bot):
    """A vertical gradient filling box (x0, y0, x1, y1)."""
    x0, y0, x1, y1 = box
    n = max(1, y1 - y0)
    for i in range(n):
        t = i / max(1, n - 1)
        d.line([(x0, y0 + i), (x1, y0 + i)], fill=tuple(int(a + (b - a) * t) for a, b in zip(top, bot)))


def pack565(img):
    """RGB -> RGB565, little-endian: the Chumby's framebuffer format."""
    r, g, b = img.convert("RGB").split()
    hi = ImageChops.add(r.point(lambda v: v & 0xF8), g.point(lambda v: v >> 5))
    lo = ImageChops.add(g.point(lambda v: ((v >> 2) & 7) << 5), b.point(lambda v: v >> 3))
    return Image.merge("LA", (lo, hi)).tobytes()


def sheet(frames):
    """Frames of one size, stacked top to bottom."""
    w, h = frames[0].size
    out = Image.new("RGB", (w, h * len(frames)))
    for i, fr in enumerate(frames):
        out.paste(fr, (0, i * h))
    return out


def load_stations():
    out = []
    with open(os.path.join(CRATE, "stations.txt"), encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            sid, title, url, colour, glyph, desc = [p.strip() for p in line.split("|")]
            rgb = tuple(int(colour[i:i + 2], 16) for i in (0, 2, 4))
            out.append(dict(id=sid, title=title, url=url, colour=rgb, glyph=glyph, desc=desc))
    return out


# ---------------------------------------------------------------- 2008 widgets
def glossy_button(d, T, cx, cy, r):
    """The Chumby control panel's round button: a rim, a gradient face, a gloss on top."""
    d.ellipse([cx - r - SS * 2, cy - r - SS * 2, cx + r + SS * 2, cy + r + SS * 2], fill=T["ring"])
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=T["rim"])
    ir = r - SS * 2
    top, bot = T["face"]
    for i in range(int(ir * 2)):
        y = cy - ir + i
        t = i / (ir * 2)
        half = math.sqrt(max(0, ir * ir - (y - cy) ** 2))
        d.line([(cx - half, y), (cx + half, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(top, bot)))
    d.ellipse([cx - ir * .7, cy - ir * .92, cx + ir * .7, cy - ir * .05], fill=T["gloss"])


def icon(d, kind, cx, cy, s, col):
    w = max(SS, int(s * .1))
    if kind == "next":
        for dx in (-s * .2, s * .1):
            d.polygon([(cx + dx - s * .12, cy - s * .26), (cx + dx + s * .16, cy), (cx + dx - s * .12, cy + s * .26)], fill=col)
    elif kind == "play":
        d.polygon([(cx - s * .16, cy - s * .26), (cx + s * .26, cy), (cx - s * .16, cy + s * .26)], fill=col)
    elif kind == "pause":
        d.rectangle([cx - s * .22, cy - s * .24, cx - s * .06, cy + s * .24], fill=col)
        d.rectangle([cx + s * .06, cy - s * .24, cx + s * .22, cy + s * .24], fill=col)
    elif kind == "speaker":
        d.polygon([(cx - s * .38, cy - s * .14), (cx - s * .16, cy - s * .14), (cx + s * .08, cy - s * .36),
                   (cx + s * .08, cy + s * .36), (cx - s * .16, cy + s * .14), (cx - s * .38, cy + s * .14)], fill=col)
        for k in (1, 2):
            rr = s * (.16 + .13 * k)
            d.arc([cx + s * .02 - rr, cy - rr, cx + s * .02 + rr, cy + rr], -45, 45, fill=col, width=w)


def on_bg(crop, rgba):
    """A supersampled RGBA drawing over a crop of the background."""
    base = big(crop).convert("RGBA")
    base.alpha_composite(rgba)
    return small(base.convert("RGB"))


def keyed(im_ss):
    """Supersampled RGBA -> screen-size RGB, KEY where transparent. Scaled in
    premultiplied alpha, so no key colour bleeds into the edges."""
    sm = im_ss.convert("RGBa").resize((im_ss.size[0] // SS, im_ss.size[1] // SS), Image.LANCZOS).convert("RGBA")
    out = Image.new("RGB", sm.size, KEY)
    out.paste(sm.convert("RGB"), (0, 0), sm.getchannel("A").point(lambda a: 255 if a >= 110 else 0))
    return out


# ---------------------------------------------------------------- the tiles (our own drawings)
def glyph(im, kind, cx, cy, s, col, cut):
    """A simple mark for a station on im, centred at (cx, cy), about s across, in
    col; cut is the colour behind it, for holes."""
    d = ImageDraw.Draw(im)
    w = max(SS, int(s * .06))
    if kind == "leaf":                                     # two leaves on a stem
        for sx in (-1, 1):
            leaf = Image.new("RGBA", (int(s * .5), int(s * .26)), (0, 0, 0, 0))
            ImageDraw.Draw(leaf).ellipse([0, 0, leaf.size[0] - 1, leaf.size[1] - 1], fill=col)
            leaf = leaf.rotate(35 * sx, expand=True, resample=Image.BICUBIC)
            im.alpha_composite(leaf, (int(cx + sx * s * .2 - leaf.size[0] / 2), int(cy - s * .12 - leaf.size[1] / 2)))
        d.line([(cx, cy - s * .05), (cx, cy + s * .32)], fill=col, width=w)
    elif kind == "waves":                                  # three slow sine waves
        for k in (-1, 0, 1):
            pts = [(cx - s * .4 + i * s * .8 / 40, cy + k * s * .2 + math.sin(i / 40 * 2 * math.pi) * s * .06)
                   for i in range(41)]
            d.line(pts, fill=col, width=w, joint="curve")
    elif kind == "planet":                                 # a ringed planet, and stars
        r = s * .2
        d.arc([cx - s * .42, cy - s * .12, cx + s * .42, cy + s * .12], 180, 360, fill=col, width=w)
        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=col)
        d.arc([cx - s * .42, cy - s * .12, cx + s * .42, cy + s * .12], 0, 180, fill=col, width=w)
        for sx, sy in ((-.38, -.32), (.34, -.36), (.4, .3)):
            d.ellipse([cx + s * sx - w, cy + s * sy - w, cx + s * sx + w, cy + s * sy + w], fill=col)
    elif kind == "flower":                                 # five petals round a centre
        for k in range(5):
            a = k * 2 * math.pi / 5 - math.pi / 2
            px, py = cx + math.cos(a) * s * .2, cy + math.sin(a) * s * .2
            d.ellipse([px - s * .14, py - s * .14, px + s * .14, py + s * .14], fill=col)
        d.ellipse([cx - s * .1, cy - s * .1, cx + s * .1, cy + s * .1], fill=cut)
        d.ellipse([cx - s * .07, cy - s * .07, cx + s * .07, cy + s * .07], fill=col)
    elif kind == "keyhole":                                # a keyhole in a ring
        d.ellipse([cx - s * .38, cy - s * .38, cx + s * .38, cy + s * .38], outline=col, width=w)
        d.ellipse([cx - s * .1, cy - s * .2, cx + s * .1, cy], fill=col)
        d.polygon([(cx - s * .05, cy - s * .08), (cx + s * .05, cy - s * .08), (cx + s * .1, cy + s * .22),
                   (cx - s * .1, cy + s * .22)], fill=col)
    elif kind == "satellite":                              # a body with two panels
        d.rectangle([cx - s * .08, cy - s * .12, cx + s * .08, cy + s * .12], fill=col)
        for sx in (-1, 1):
            x0, x1 = sorted((cx + sx * s * .14, cx + sx * s * .42))
            d.rectangle([x0, cy - s * .1, x1, cy + s * .1], outline=col, width=w)
            d.line([(x0, cy), (x1, cy)], fill=col, width=max(1, w // 2))
            d.line([(cx + sx * s * .08, cy), (cx + sx * s * .14, cy)], fill=col, width=w)
        d.line([(cx, cy - s * .12), (cx, cy - s * .3)], fill=col, width=w)
        d.ellipse([cx - w * 1.5, cy - s * .3 - w * 1.5, cx + w * 1.5, cy - s * .3 + w * 1.5], fill=col)
    else:
        raise ValueError(f"no glyph {kind!r}")


def tile(st, T):
    """A station's tile, ART pixels square: its colour, its glyph and its name."""
    n = ART * SS
    im = Image.new("RGBA", (n, n))
    d = ImageDraw.Draw(im)
    dim = T["tile_dim"]
    base = tuple(int(c * dim) for c in st["colour"])
    top = tuple(min(255, int(c + (255 - c) * .25)) for c in base)
    bot = tuple(int(c * .7) for c in base)
    grad(d, (0, 0, n, n), top, bot)
    white = tuple(int(255 * (0.75 + 0.25 * dim)) for _ in range(3)) + (255,)
    mid = tuple((a + b) // 2 for a, b in zip(top, bot)) + (255,)
    glyph(im, st["glyph"], n / 2, n * .4, n * .62, white, mid)
    # The name, along the bottom, on as many lines as it needs.
    fnt = f(VERDB, 9)
    words, lines, cur = st["title"].upper().split(), [], ""
    for w in words:
        t = (cur + " " + w).strip()
        if fnt.getlength(t) > n - 10 * SS and cur:
            lines.append(cur)
            cur = w
        else:
            cur = t
    lines.append(cur)
    y = n - 6 * SS - len(lines) * 11 * SS
    for ln in lines:
        d.text((n / 2, y), ln, font=fnt, fill=white, anchor="ma")
        y += 11 * SS
    return im.convert("RGB")


# ---------------------------------------------------------------- the screen
def background(T):
    img = Image.new("RGB", (W * SS, H * SS))
    d = ImageDraw.Draw(img)
    grad(d, (0, 0, W * SS, H * SS), *T["bg"])
    grad(d, (0, 0, W * SS, 22 * SS), *T["bar"])                         # top bar
    d.line([(0, 22 * SS), (W * SS, 22 * SS)], fill=T["bar_line"], width=SS)
    d.text((10 * SS, 4 * SS), "SOMA", font=f(ROUND, 13), fill=T["soma"])
    d.text((52 * SS, 4 * SS), "FM", font=f(ROUND, 13), fill=T["fm"])
    d.text((84 * SS, 7 * SS), "listener-supported radio", font=f(VERD, 7), fill=T["tag"])
    # The station list's panel.
    d.rounded_rectangle([6 * SS, 26 * SS, 176 * SS, 206 * SS], radius=4 * SS, fill=T["panel"], outline=T["edge"], width=SS)
    grad(d, (7 * SS, 27 * SS, 175 * SS, 41 * SS), *T["head"])
    d.text((12 * SS, 29 * SS), "Your Stations", font=f(VERDB, 8), fill=T["head_text"])
    # The now-playing panel.
    d.rounded_rectangle([182 * SS, 26 * SS, 314 * SS, 206 * SS], radius=4 * SS, fill=T["card"], outline=T["edge"], width=SS)
    # The transport bar: skip, and DONE.
    grad(d, (0, 210 * SS, W * SS, H * SS), *T["transport"])
    d.line([(0, 210 * SS), (W * SS, 210 * SS)], fill=T["transport_line"], width=SS)
    d.rounded_rectangle([262 * SS, 215 * SS, 312 * SS, 235 * SS], radius=9 * SS, fill=T["pill"], outline=T["pill_edge"], width=SS)
    d.text((287 * SS, 225 * SS), "DONE", font=f(VERDB, 9), fill=T["pill_text"], anchor="mm")
    glossy_button(d, T, SKIP[0] * SS, SKIP[1] * SS, 11 * SS)
    icon(d, "next", SKIP[0] * SS, SKIP[1] * SS, 13 * SS, T["ink"])
    return small(img)


def rows(stations, T):
    """Each station's row, unselected then selected."""
    _, _, w, h = ROWS
    out = []
    for st in stations:
        for sel in (False, True):
            im = Image.new("RGB", (w * SS, h * SS), T["panel"])
            d = ImageDraw.Draw(im)
            if sel:
                grad(d, (0, 0, w * SS, h * SS - SS), *T["sel"])
            d.line([(0, h * SS - SS), (w * SS, h * SS - SS)], fill=T["row_line"], width=SS)
            d.text((8 * SS, h * SS / 2), st["title"], font=f(VERDB if sel else VERD, 10),
                   fill=T["sel_text"] if sel else T["row_text"], anchor="lm")
            out.append(small(im))
    return out


def cards(stations, T):
    """Each station's now-playing card: its name, its tile and a line about it."""
    _, _, cw, ch = CARD
    out = []
    for st in stations:
        im = Image.new("RGB", (cw * SS, ch * SS), T["card"])
        d = ImageDraw.Draw(im)
        d.text((cw * SS / 2, 6 * SS), st["title"], font=f(VERDB, 10), fill=T["title"], anchor="ma")
        ax = (cw - ART) // 2
        d.rectangle([(ax - 2) * SS, 22 * SS, (ax + ART + 1) * SS, (24 + ART + 1) * SS], fill=T["frame"],
                    outline=T["frame_edge"], width=SS)
        im.paste(tile(st, T), (ax * SS, 24 * SS))
        fnt = f(VERD, 7)
        line, y = "", 24 + ART + 6
        for w in st["desc"].split():
            t = (line + " " + w).strip()
            if fnt.getlength(t) > (cw - 10) * SS and line:
                d.text((cw * SS / 2, y * SS), line, font=fnt, fill=T["desc"], anchor="ma")
                line, y = w, y + 10
            else:
                line = t
        if y > ch - 12:
            sys.exit(f"{st['id']}: the description is too long for the card")
        d.text((cw * SS / 2, y * SS), line, font=fnt, fill=T["desc"], anchor="ma")
        out.append(small(im))
    return out


def play(bg, T):
    """Play (stopped) and pause (playing), over the background."""
    x, y, s, _ = PLAY
    crop = bg.crop((x, y, x + s, y + s))
    out = []
    for kind in ("play", "pause"):
        im = Image.new("RGBA", (s * SS, s * SS), (0, 0, 0, 0))
        d = ImageDraw.Draw(im)
        glossy_button(d, T, s * SS / 2, s * SS / 2, 11 * SS)
        icon(d, kind, s * SS / 2, s * SS / 2, 13 * SS, T["ink"])
        out.append(on_bg(crop, im))
    return out


def slider(bg, T):
    """The volume slider's empty and full tracks, over the background, and its knob."""
    x, y, w, h, t0, t1 = SLIDER
    crop = bg.crop((x, y, x + w, y + h))
    tracks = []
    for full in (False, True):
        im = Image.new("RGBA", (w * SS, h * SS), (0, 0, 0, 0))
        ImageDraw.Draw(im).rounded_rectangle([t0 * SS, 10 * SS, t1 * SS, 16 * SS], radius=3 * SS,
                                             fill=T["track_full"] if full else T["track"],
                                             outline=T["track_edge"], width=SS)
        tracks.append(on_bg(crop, im))
    knob = Image.new("RGBA", (KNOB * SS, KNOB * SS), (0, 0, 0, 0))
    kd = ImageDraw.Draw(knob)
    glossy_button(kd, T, KNOB * SS / 2, KNOB * SS / 2, KNOB * SS / 2 - SS * 3)
    icon(kd, "speaker", KNOB * SS / 2, KNOB * SS / 2, KNOB * SS * .62, T["ink"])
    return tracks, keyed(knob)


def screen_txt(n):
    x, y, w, h = ROWS
    lines = ["# Made by render/render.py: don't edit. Pixels on the 320x240 screen; the",
             "# pictures are in day/ and night/, RGB565, frames stacked top to bottom.",
             f"stations {n}",
             f"rows {x} {y} {w} {h}",
             "card {} {} {} {}".format(*CARD),
             "play {} {} {} {}".format(*PLAY),
             "slider {} {} {} {} {} {} {} {}".format(*SLIDER, KNOB, KNOB)]
    lines += [f"hot {name} {x} {y} {w} {h}" for name, x, y, w, h in HOTS]
    return "\n".join(lines) + "\n"


def fnv1a64(data):
    """FNV-1a, 64 bits: the same in src/screen.rs's test, without a crate."""
    h = 0xcbf29ce484222325
    for b in data:
        h = ((h ^ b) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h


def inputs():
    """What the pictures are made from, relative to the crate."""
    names = ["render/render.py", "stations.txt"]
    names += sorted("render/fonts/" + n for n in os.listdir(FONTS) if n.endswith((".ttf", ".otf")))
    return names


def rendered_from():
    out = ["# The inputs files/ was made from (FNV-1a 64): see render.py."]
    for n in inputs():
        with open(os.path.join(CRATE, n), "rb") as fh:
            out.append(f"{fnv1a64(fh.read()):016x} {n}")
    return "\n".join(out) + "\n"


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(data)


def preview(bg, row_frames, card_frames, play_frames, tracks, knob, sel, playing, vol):
    """The whole screen as the app composes it (src/screen.rs), for --png."""
    im = bg.copy()
    x, y, w, h = ROWS
    for i in range(len(row_frames) // 2):
        im.paste(row_frames[2 * i + (i == sel)], (x, y + i * h))
    im.paste(card_frames[sel], CARD[:2])
    im.paste(play_frames[int(playing)], PLAY[:2])
    sx, sy, sw, sh, t0, t1 = SLIDER
    strip = tracks[0].copy()
    kx = t0 + (t1 - t0) * vol // 100
    strip.paste(tracks[1].crop((t0, 0, kx, sh)), (t0, 0))
    # Opaque wherever the knob isn't KEY.
    alpha = ImageChops.difference(knob, Image.new("RGB", knob.size, KEY)).convert("L").point(lambda v: 255 if v else 0)
    strip.paste(knob, (kx - KNOB // 2, (sh - KNOB) // 2), alpha)
    im.paste(strip, (sx, sy))
    return im


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--png", help="also write the whole screen as PNGs here, for looking at")
    args = ap.parse_args()
    stations = load_stations()
    if len(stations) * ROWS[3] > 206 - ROWS[1]:
        sys.exit("too many stations for the list")
    for theme, T in (("day", DAY), ("night", NIGHT)):
        bg = background(T)
        rf, cf, pf = rows(stations, T), cards(stations, T), play(bg, T)
        tracks, knob = slider(bg, T)
        d = os.path.join(FILES, theme)
        write(os.path.join(d, "background.rgb565"), pack565(bg))
        write(os.path.join(d, "rows.rgb565"), pack565(sheet(rf)))
        write(os.path.join(d, "cards.rgb565"), pack565(sheet(cf)))
        write(os.path.join(d, "play.rgb565"), pack565(sheet(pf)))
        write(os.path.join(d, "track_empty.rgb565"), pack565(tracks[0]))
        write(os.path.join(d, "track_full.rgb565"), pack565(tracks[1]))
        write(os.path.join(d, "knob.rgb565"), pack565(knob))
        if args.png:
            os.makedirs(args.png, exist_ok=True)
            preview(bg, rf, cf, pf, tracks, knob, 0, True, 65).save(os.path.join(args.png, f"{theme}.png"))
    write(os.path.join(FILES, "screen.txt"), screen_txt(len(stations)).encode())
    write(os.path.join(HERE, "rendered-from.txt"), rendered_from().encode())


if __name__ == "__main__":
    main()
