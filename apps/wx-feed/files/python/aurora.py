"""aurora: a Windows-Vista-style aurora screensaver (2008 Flash era) for wxshow.

  background  a seamless 960x240 panorama of glossy, glowing ribbons on a luminous
              sky-blue gradient; wxshow pans it sideways (bgscroll), ~38 s per loop
  logo        logo.png from the asset directories (see wxassets), else a "neochumby"
              wordmark of our own; wxshow's logofx builds it with a random demoscene intro
              each time the page comes on (fly, print, wobble, zoom, drop, dissolve, raster,
              decrunch, spin, octopus) and sweeps a shine across it; then (the Aurora app's
              options, written into the logofx line) holds it for N s, plays the intro backwards
              out, and starts another intro, for as long as the page shows; or stays still

Kept light where the logo sits, like the 2008 Chumby panel, so the blue logo reads
without an outline.
"""
import math
import os
import re
import struct

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

import wxassets

W, H = 320, 240
PANO = 960                                    # panorama width; every curve repeats every PANO px
ASSETS = __file__.rsplit("/", 1)[0] + "/assets"
KEY = (255, 0, 255)
LOGO_Y = 88                                   # logo is 64 px tall: (240 - 64) / 2, centred on screen

# (base y, [(amplitude, cycles per panorama, phase)], thickness, glow colour, core colour, blur)
RIBBONS = [
    (150, [(26, 1, 0.0), (10, 3, 1.3)], 34, (90, 200, 235), (220, 250, 255), 9),
    (172, [(20, 2, 2.1), (8, 5, 0.4)], 22, (140, 230, 160), (235, 255, 225), 7),
    (126, [(18, 1, 3.6), (7, 4, 2.2)], 16, (120, 170, 245), (240, 248, 255), 6),
    (196, [(16, 2, 0.9), (6, 3, 4.0)], 28, (70, 160, 225), (200, 235, 255), 10),
    (110, [(12, 3, 5.1), (5, 2, 0.7)], 7, (255, 255, 255), (255, 255, 255), 3),     # thin white streak
]


def _sky(width):
    img = Image.new("RGB", (width, H))
    d = ImageDraw.Draw(img)
    for y in range(H):
        t = y / (H - 1)
        top, mid, bot = (6, 62, 78), (16, 112, 118), (40, 140, 105)     # Vista Aurora teal
        a, b, u = (top, mid, t / 0.55) if t < 0.55 else (mid, bot, (t - 0.55) / 0.45)
        d.line([(0, y), (width, y)], fill=tuple(int(p + (q - p) * u) for p, q in zip(a, b)))
    return img


def _ribbon_mask(width, base, waves, thick, x0):
    """Soft band whose centre and thickness follow periodic curves (period PANO)."""
    m = Image.new("L", (width, H), 0)
    d = ImageDraw.Draw(m)
    top, bot = [], []
    for x in range(0, width + 1, 2):
        u = (x + x0) / PANO * 2 * math.pi
        y = base + sum(a * math.sin(k * u + ph) for a, k, ph in waves)
        t = thick * (0.55 + 0.45 * math.sin(2 * u + base))      # breathes along its length
        top.append((x, y - t / 2))
        bot.append((x, y + t / 2))
    d.polygon(top + bot[::-1], fill=255)
    return m


def panorama():
    """Rendered three panoramas wide and cropped to the middle one, so the blur wraps seamlessly."""
    wide = PANO * 3
    img = _sky(wide)
    for base, waves, thick, glow, core, blur in RIBBONS:
        mask = _ribbon_mask(wide, base, waves, thick, -PANO)
        halo = mask.filter(ImageFilter.GaussianBlur(blur * 2)).point(lambda v: int(v * 0.55))
        body = mask.filter(ImageFilter.GaussianBlur(blur))
        crest = _ribbon_mask(wide, base - thick * 0.18, waves, max(2, thick * 0.22), -PANO)
        crest = crest.filter(ImageFilter.GaussianBlur(max(1, blur // 3)))
        for m, col, k in ((halo, glow, 0.8), (body, glow, 1.0), (crest, core, 1.0)):
            layer = Image.new("RGB", (wide, H), col)
            img = ImageChops.screen(img, Image.composite(layer, Image.new("RGB", (wide, H)), m.point(lambda v: int(v * k))))
    # soft luminous patch where the logo sits (static in the pano, so it drifts gently too)
    glow = Image.new("L", (wide, H), 0)
    gd = ImageDraw.Draw(glow)
    gd.ellipse([0, LOGO_Y - 30, wide, LOGO_Y + 95], fill=70)       # band behind the centred logo
    glow = glow.filter(ImageFilter.GaussianBlur(25))
    img = ImageChops.screen(img, Image.composite(Image.new("RGB", (wide, H), (255, 255, 255)), Image.new("RGB", (wide, H)), glow))
    return img.crop((PANO, 0, PANO * 2, H))


def wordmark():
    """Our own logo when no logo.png is supplied: "neochumby" in the bundled rounded face,
    blue, at most 64 px tall like the logo it stands in for."""
    fnt = ImageFont.truetype(f"{ASSETS}/fonts/SNPro-Bold.otf", 44)
    x0, y0, x1, y1 = fnt.getbbox("neochumby")
    im = Image.new("RGBA", (x1 - x0 + 4, min(64, y1 - y0 + 4)), (0, 0, 0, 0))
    ImageDraw.Draw(im).text((2 - x0, 2 - y0), "neochumby", font=fnt, fill=(40, 90, 200, 255))
    return im


def logo_white():
    """The logo in white (blue fought the teal Vista palette); anything white in the
    original (a logo's eyes) becomes deep teal so it still reads."""
    path = wxassets.find("logo.png")
    lg = Image.open(path).convert("RGBA") if path else wordmark()
    px = lg.load()
    for y in range(lg.size[1]):
        for x in range(lg.size[0]):
            r, g, b, a = px[x, y]
            if a:
                px[x, y] = (10, 70, 82, a) if min(r, g, b) > 200 else (248, 252, 252, a)
    return lg


def logo_keyed():
    lg = logo_white()
    out = Image.new("RGB", lg.size, KEY)
    out.paste(lg.convert("RGB"), (0, 0), lg.getchannel("A").point(lambda a: 255 if a >= 128 else 0))
    return out

# parallax light layers: (name, width = loop period, speed in 1/4 px per frame (negative =
# the other way), ribbons [(base y within the layer, waves, thickness, glow, core, blur)])
LAYER_Y, LAYER_H = 104, 136                   # ribbons sweep below the logo (which ends at y 152)
FADE_TOP, FADE_BOTTOM = 40, 6                 # rows over which each layer fades in/out, so no hard edge
LAYERS = [   # saturated (the LCD washes colours out) but no white-hot crests near the logo
    ("far", 1280, 2, [(80, [(18, 1, 0.3), (8, 3, 2.0)], 40, (8, 60, 50), (25, 95, 70), 14)]),
    ("main", 960, 4, [(84, [(18, 1, 0.0), (7, 3, 1.3)], 26, (35, 95, 10), (110, 175, 40), 8),
                      (110, [(12, 2, 0.9), (5, 3, 4.0)], 20, (18, 80, 28), (70, 150, 55), 9)]),
    ("streak", 800, -3, [(96, [(10, 1, 5.1), (4, 2, 0.7)], 5, (45, 55, 45), (120, 130, 115), 3)]),
    ("lime", 1120, 6, [(104, [(14, 2, 2.1), (6, 5, 0.4)], 18, (75, 85, 0), (150, 165, 50), 7)]),
]

# Underwater light shafts, optional: a real clip played as additive light. raysclip.py keys
# the green screen out of a light-rays video you have the rights to (light = how far a pixel
# lifts R and B above the green plate), crops the right-hand 4:3 so the beams enter from the
# top right, trims the opening and crossfades the tail into the head, into raysclip.png: one
# 320x240 greyscale frame after another. It isn't bundled (footage isn't ours to publish), so
# it is read from the asset directories (wxassets), and without it the page has no rays.
# Here, on each render, the frames get only the bottom fade (gone ~15% above the bottom
# edge) and a black point, and are packed for wxshow's glowseq: per
# frame a u32 span count, then per span a u32 (start | len << 17, start = y*320 + x, len <= 320
# within a row) and len intensity bytes 1..255. Only lit pixels are stored: those are the cost.
# The footage's own beams appear, hold and fade, with gaps where none shows; nothing of ours
# is drawn in.
RAY_COLOUR = (150, 235, 220)                  # pale aqua on the teal sky (the glowseq tint)
SURFACE_Y = 18                                # the soft bright surface band across the top (in sky())
SEQ_NAME = "aurora_seq"
SEQ_BRIGHT = 170                              # tint scale 0..255 at full intensity
SEQ_BLACK = 24                                # black point: keyed light below this (of ~200 peak) is
                                              # dropped and the rest rescaled, so beams reach black
                                              # smoothly and their faint wide tails cost nothing
SEQ_GONE_FROM, SEQ_GONE = 165, 205            # bottom fade: smoothstep to nothing by y=205


def seq_frames():
    """The clip's light frames, ready to pack: list of 320x240 'L' images, and the clip's fps."""
    strip = Image.open(wxassets.find("raysclip.png"))
    fps = int(strip.text.get("fps", "12"))
    strip = strip.convert("L")
    n = strip.size[1] // H
    fade = Image.new("L", (1, H))
    fade.putdata([round(255 * (1 - u * u * (3 - 2 * u)))
                  for u in (min(1.0, max(0.0, (y - SEQ_GONE_FROM) / (SEQ_GONE - SEQ_GONE_FROM))) for y in range(H))])
    fade = fade.resize((W, H))
    lut = [0 if v <= SEQ_BLACK else min(255, round((v - SEQ_BLACK) * 255 / (255 - SEQ_BLACK))) for v in range(256)]
    return [ImageChops.multiply(strip.crop((0, k * H, W, (k + 1) * H)), fade).point(lut) for k in range(n)], fps


def pack_spans(frame):
    """One light frame in glowseq's lit-span format."""
    px = frame.tobytes()
    spans = []
    for y in range(H):
        row = px[y * W:(y + 1) * W]
        for m in re.finditer(rb"[^\x00]+", row):
            spans.append(struct.pack("<I", (y * W + m.start()) | (m.end() - m.start()) << 17) + m.group())
    return struct.pack("<I", len(spans)) + b"".join(spans)


def write_if_changed(path, data):
    """Leave an identical file alone (mtime and all), so delivery skips it; else write it
    beside and rename it into place, so no reader sees half a file."""
    try:
        if os.path.getsize(path) == len(data):
            with open(path, "rb") as fh:
                if fh.read() == data:
                    return False
    except OSError:
        pass
    tmp = os.path.join(os.path.dirname(path), f".{os.path.basename(path)}.{os.getpid()}.tmp")   # dot: sync skips it
    with open(tmp, "wb") as fh:
        fh.write(data)
    os.replace(tmp, path)
    return True


def light_layer(width, ribbons):
    """One seamless strip of additive light (black = nothing). Rendered 3 periods wide and
    cropped to the middle so the blur wraps."""
    wide = width * 3
    img = Image.new("RGB", (wide, LAYER_H))
    black = Image.new("RGB", (wide, LAYER_H))
    for base, waves, thick, glow, core, blur in ribbons:
        def band(by, th):
            m = Image.new("L", (wide, LAYER_H), 0)
            d = ImageDraw.Draw(m)
            top, bot = [], []
            for x in range(0, wide + 1, 2):
                u = (x - width) / width * 2 * math.pi
                y = by + sum(a * math.sin(k * u + ph) for a, k, ph in waves)
                t = th * (0.55 + 0.45 * math.sin(2 * u + by))
                top.append((x, y - t / 2)); bot.append((x, y + t / 2))
            d.polygon(top + bot[::-1], fill=255)
            return m
        mask = band(base, thick)
        for m, col in ((mask.filter(ImageFilter.GaussianBlur(blur * 2)), glow),
                       (mask.filter(ImageFilter.GaussianBlur(blur)), glow),
                       (band(base - thick * 0.18, max(2, thick * 0.22)).filter(ImageFilter.GaussianBlur(max(1, blur // 3))), core)):
            img = ImageChops.add(img, Image.composite(Image.new("RGB", (wide, LAYER_H), col), black, m))
    img = img.crop((width, 0, width * 2, LAYER_H))
    ramp = Image.new("L", (1, LAYER_H))
    ramp.putdata([int(255 * min(1, y / FADE_TOP, (LAYER_H - 1 - y) / FADE_BOTTOM)) for y in range(LAYER_H)])
    img = Image.composite(img, Image.new("RGB", img.size), ramp.resize(img.size))
    # trim the faint tails to pure black: they add almost no light, and wxshow skips black pixels
    dark = img.convert("L").point(lambda v: 255 if v >= 9 else 0)
    return Image.composite(img, Image.new("RGB", img.size), dark)


def sky():
    """The still background: the sky gradient with a soft bright band behind the centred logo."""
    img = _sky(W)
    glow = Image.new("L", (W, H), 0)
    ImageDraw.Draw(glow).ellipse([-60, LOGO_Y - 30, W + 60, LOGO_Y + 95], fill=0)   # none: the white logo needs no backing
    glow = glow.filter(ImageFilter.GaussianBlur(25))
    img = ImageChops.screen(img, Image.composite(Image.new("RGB", (W, H), (255, 255, 255)), Image.new("RGB", (W, H)), glow))
    # the bright, rippled surface the light shafts fall from: a wide soft band across the top
    surf = Image.new("L", (W, H), 0)
    ImageDraw.Draw(surf).rectangle([-40, SURFACE_Y - 40, W + 40, SURFACE_Y + 2], fill=70)   # a rectangle: the sky tiles into a panning panorama
    surf = surf.filter(ImageFilter.GaussianBlur(14))
    return ImageChops.screen(img, Image.composite(Image.new("RGB", (W, H), (120, 230, 225)), Image.new("RGB", (W, H)), surf))


def composite(t_frames=0):
    """What the device shows at a given frame (for thumbnails and previews)."""
    img = sky()
    for name, width, speed, ribbons in LAYERS:
        layer = light_layer(width, ribbons)
        off = (t_frames * speed // 4) % width
        strip = Image.new("RGB", (W, LAYER_H))
        for x0 in range(-off, W, width):
            strip.paste(layer, (x0, 0))
        band = img.crop((0, LAYER_Y, W, LAYER_Y + LAYER_H))
        img.paste(ImageChops.add(band, strip), (0, LAYER_Y))
    return img


def base_panorama():
    """The sky with the far (slowest, haziest) layer mixed in, as a seamless panorama: wxshow
    pans it with a plain copy, so only the nearer layers cost a per-pixel blend."""
    name, width, speed, ribbons = LAYERS[0]
    layer = light_layer(width, ribbons)
    pano = Image.new("RGB", (width, H))
    s = sky()
    for x0 in range(0, width, W):
        pano.paste(s, (x0, 0))
    band = pano.crop((0, LAYER_Y, width, LAYER_Y + LAYER_H))
    pano.paste(ImageChops.add(band, layer), (0, LAYER_Y))
    return pano, width, speed


def render(out, page, rgb565, secs=24, opts=None):
    """opts: the Aurora app's options (config.py BUILTIN schema): logo_fx, logo_mode, logo_hold."""
    opts = opts or {}
    fx = str(opts.get("logo_fx") or "random")
    mode = "once" if opts.get("logo_mode") == "once" else "loop"
    try:
        hold = max(1, min(60, int(opts.get("logo_hold", 4))))
    except (TypeError, ValueError):
        hold = 4
    if not re.fullmatch(r"[a-z]{1,16}", fx):
        fx = "random"
    bg = sky()
    rgb565(bg, f"{out}/aurora_bg.raw")
    lines = [f"page {page} aurora_bg.raw {secs} noclock noticker fullframe"]
    # the light rays (the clip, if there is one) first, ribbons over
    if wxassets.find("raysclip.png"):
        frames, fps = seq_frames()
        for k, fr in enumerate(frames):
            write_if_changed(f"{out}/{SEQ_NAME}_{k:03d}.l8", pack_spans(fr))
        r, g, b = RAY_COLOUR
        lines.append(f"glowseq {page} {SEQ_NAME} {len(frames)} {fps} {r} {g} {b} {SEQ_BRIGHT}")
    pano, pw, pspeed = base_panorama()
    rgb565(pano, f"{out}/aurora_base.raw")
    lines.append(f"bgscroll {page} aurora_base.raw {pw} {pspeed}")
    for name, width, speed, ribbons in LAYERS[1:]:             # back to front; far is in the base
        rgb565(light_layer(width, ribbons), f"{out}/aurora_{name}.raw")
        lines.append(f"glow {page} aurora_{name}.raw {width} {LAYER_H} {LAYER_Y} {speed}")
    lg = logo_keyed()
    rgb565(lg, f"{out}/aurora_logo.raw")
    lines.append(f"logofx {page} aurora_logo.raw {lg.size[0]} {lg.size[1]} {(W - lg.size[0]) // 2} {(H - lg.size[1]) // 2}"
                 f" {fx} {mode} {hold}")
    return lines


def thumbnail():
    img = composite(0)
    lg = logo_white()
    img.paste(lg, ((W - lg.size[0]) // 2, (H - lg.size[1]) // 2), lg)
    return img
