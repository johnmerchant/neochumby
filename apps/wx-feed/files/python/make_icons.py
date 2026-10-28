#!/usr/bin/env python3
"""make_icons: draw the 80x80 app icons in assets/icons/ that the config page shows on
each app card (chumby.com's "my channels" style: a small square picture per app).

Usage: make_icons.py        (re-run after changing a drawing; the PNGs are committed)
"""
import math
import os

from PIL import Image, ImageDraw, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = HERE + "/assets/icons"
S = 80
K = 4                       # supersampling
Z = S * K


def vgrad(size, top, bot):
    im = Image.new("RGB", size)
    d = ImageDraw.Draw(im)
    for y in range(size[1]):
        t = y / (size[1] - 1)
        d.line([(0, y), (size[0], y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(top, bot)))
    return im


def finish(im, name):
    im = im.resize((S, S), Image.LANCZOS)
    os.makedirs(OUT, exist_ok=True)
    im.save(f"{OUT}/{name}.png", optimize=True)
    return im


def gloss(im, alpha=60):
    """The soft top highlight every 2008 icon had."""
    ov = Image.new("RGBA", im.size, (0, 0, 0, 0))
    ImageDraw.Draw(ov).ellipse([-Z * .3, -Z * .75, Z * 1.3, Z * .42], fill=(255, 255, 255, alpha))
    out = im.convert("RGBA")
    out.alpha_composite(ov)
    return out.convert("RGB")


def cloud(d, cx, cy, s, fill, edge):
    parts = ((-0.34, 0.06, 0.27), (0.0, -0.12, 0.35), (0.34, 0.06, 0.25), (0.0, 0.12, 0.29))
    for col, grow in ((edge, K * 1.5), (fill, 0)):
        for dx, dy, r in parts:
            x, y, rr = cx + dx * s, cy + dy * s, r * s + grow
            d.ellipse([x - rr, y - rr, x + rr, y + rr], fill=col)


def weather():
    im = vgrad((Z, Z), (60, 130, 220), (150, 205, 250))
    d = ImageDraw.Draw(im)
    cx, cy, r = Z * .4, Z * .38, Z * .17
    for i in range(12):
        a = i * math.pi / 6
        r0, r1 = r * 1.3, r * (1.85 if i % 2 else 1.6)
        d.line([(cx + r0 * math.cos(a), cy + r0 * math.sin(a)), (cx + r1 * math.cos(a), cy + r1 * math.sin(a))],
               fill=(255, 200, 40), width=int(K * 2.6))
    d.ellipse([cx - r - K * 1.5, cy - r - K * 1.5, cx + r + K * 1.5, cy + r + K * 1.5], fill=(235, 140, 20))
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(255, 220, 60))
    cloud(d, Z * .58, Z * .64, Z * .58, (250, 250, 255), (150, 160, 190))
    return finish(gloss(im, 45), "weather")


def stocks():
    im = vgrad((Z, Z), (40, 44, 52), (10, 12, 16))
    d = ImageDraw.Draw(im)
    for i in range(1, 5):                                   # grid
        v = i * Z / 5
        d.line([(v, 0), (v, Z)], fill=(55, 60, 70), width=K)
        d.line([(0, v), (Z, v)], fill=(55, 60, 70), width=K)
    pts = [(.1, .78), (.3, .58), (.45, .66), (.66, .38), (.78, .44)]
    pts = [(x * Z, y * Z) for x, y in pts]
    glow = Image.new("RGBA", im.size, (0, 0, 0, 0))
    ImageDraw.Draw(glow).line(pts, fill=(60, 230, 110, 150), width=K * 9, joint="curve")
    glow = glow.filter(ImageFilter.GaussianBlur(K * 3))
    im = im.convert("RGBA")
    im.alpha_composite(glow)
    d = ImageDraw.Draw(im)
    d.line(pts, fill=(70, 220, 100), width=K * 6, joint="curve")
    # arrow head along the last segment
    (ax, ay), (bx, by) = pts[-2], pts[-1]
    a = math.atan2(by - ay, bx - ax) - 0.55                 # point it up and right, past the last vertex
    tip = (bx + math.cos(a) * Z * .14, by + math.sin(a) * Z * .14)
    a2 = math.atan2(tip[1] - by, tip[0] - bx)
    L, Wd = Z * .17, Z * .1
    base = (tip[0] - math.cos(a2) * L, tip[1] - math.sin(a2) * L)
    nx, ny = -math.sin(a2), math.cos(a2)
    d.line([pts[-1], base], fill=(70, 220, 100), width=K * 6)
    d.polygon([tip, (base[0] + nx * Wd, base[1] + ny * Wd), (base[0] - nx * Wd, base[1] - ny * Wd)], fill=(70, 220, 100))
    return finish(gloss(im.convert("RGB"), 35), "stocks")


def aurora():
    im = vgrad((Z, Z), (6, 62, 78), (40, 140, 105))
    lay = Image.new("RGBA", im.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(lay)
    for base, amp, thick, col in ((.62, .1, .2, (90, 220, 235, 200)), (.74, .08, .12, (150, 240, 170, 180)),
                                  (.52, .06, .05, (255, 255, 255, 230))):
        top, bot = [], []
        for x in range(0, Z + 1, K):
            u = x / Z * 2 * math.pi
            y = (base + amp * math.sin(u * 1.2 + base * 9)) * Z
            t = thick * Z * (0.6 + 0.4 * math.sin(2 * u))
            top.append((x, y - t / 2))
            bot.append((x, y + t / 2))
        d.polygon(top + bot[::-1], fill=col)
    lay = lay.filter(ImageFilter.GaussianBlur(K * 3))
    im = im.convert("RGBA")
    im.alpha_composite(lay)
    h = int(Z * .42)
    lg = Image.new("RGBA", (h * 2, h * 2), (0, 0, 0, 0))            # our own drawn octopus (screens.icon)
    import screens
    screens.icon(ImageDraw.Draw(lg), "octo", h, h * 1.1, h / 0.76, (40, 90, 200))
    lg = lg.crop(lg.getbbox())
    halo = Image.new("RGBA", im.size, (0, 0, 0, 0))
    ImageDraw.Draw(halo).ellipse([Z * .5 - h * .62, Z * .3 - h * .55, Z * .5 + h * .62, Z * .3 + h * .62], fill=(255, 255, 255, 170))
    im.alpha_composite(halo.filter(ImageFilter.GaussianBlur(K * 4)))
    im.alpha_composite(lg, (int(Z * .5 - lg.width / 2), int(Z * .3 - h / 2)))
    return finish(gloss(im.convert("RGB"), 30), "aurora")


def news():
    """A folded newspaper (no publisher's logo)."""
    im = vgrad((Z, Z), (250, 250, 250), (220, 222, 228))
    d = ImageDraw.Draw(im)
    d.rectangle([Z * .14, Z * .2, Z * .86, Z * .82], fill=(255, 255, 255), outline=(120, 125, 140), width=K * 2)
    d.rectangle([Z * .2, Z * .27, Z * .8, Z * .4], fill=(200, 40, 40))
    for i in range(4):
        y = Z * (.48 + i * .08)
        d.line([(Z * .2, y), (Z * (.8 if i < 3 else .6), y)], fill=(130, 135, 150), width=K * 2)
    return finish(im, "news")


def default():
    """The web page's own icon, and any app's without one: the drawn octopus on a pale blue tile."""
    im = vgrad((Z, Z), (225, 235, 252), (170, 195, 240))
    lay = Image.new("RGBA", (Z, Z), (0, 0, 0, 0))
    import screens
    screens.icon(ImageDraw.Draw(lay), "octo", Z / 2, Z * .52, Z * .9, (35, 60, 130))
    im = im.convert("RGBA")
    im.alpha_composite(lay)
    return finish(gloss(im.convert("RGB"), 40), "default")


def sheet():
    icons = [weather(), stocks(), aurora(), news(), default()]
    return icons


if __name__ == "__main__":
    sheet()
    print("wrote", ", ".join(sorted(os.listdir(OUT))), "to", OUT)
