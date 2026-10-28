"""qrscreen: Settings -> APPS on the Chumby. A big QR code for the feeder's config page
(configweb.py), the address under it, and BACK, in the Settings sub-screen style.

    lines = qrscreen.configure(out, rgb565, url)               # screen "configure"
    lines += qrscreen.configure(out, rgb565, url, night=True)  # "configure_night"

QR encoding uses segno (pip install segno; Debian: python3-segno) or, failing that,
qrcode (Debian: python3-qrcode). Without either the screen still renders, showing the
address only.
"""
import os

from PIL import Image, ImageDraw

import screens

SS = screens.SS
QR_BOX = (12, 30, 172, 190)          # where the code (with its quiet zone) sits, device px


def qr_matrix(text):
    """Rows of booleans (True = dark), no border, or None if no QR library is installed."""
    try:
        import segno
        q = segno.make(text, error="m", micro=False)
        return [[bool(v) for v in row] for row in q.matrix]
    except ImportError:
        pass
    try:
        import qrcode
        q = qrcode.QRCode(error_correction=qrcode.constants.ERROR_CORRECT_M, border=0)
        q.add_data(text)
        q.make(fit=True)
        return q.get_matrix()
    except ImportError:
        return None


def qr_image(text, size):
    """A crisp black-on-white code: whole pixels per module, centred in size x size,
    with at least a 2-module quiet zone (white, so it scans on the dark theme too)."""
    m = qr_matrix(text)
    img = Image.new("RGB", (size, size), (255, 255, 255))
    if not m:
        return img, False
    n = len(m)
    px = max(1, size // (n + 4))
    off = (size - n * px) // 2
    d = ImageDraw.Draw(img)
    for y, row in enumerate(m):
        for x, v in enumerate(row):
            if v:
                d.rectangle([off + x * px, off + y * px, off + x * px + px - 1, off + y * px + px - 1], fill=(0, 0, 0))
    return img, True


def qr_icon(d, cx, cy, s, col):
    """A tiny QR-code glyph (three finder squares and a few modules), for the Settings button
    and the header. screens.icon(kind="qr") calls this."""
    u = s / 9
    x0, y0 = cx - 4.5 * u, cy - 4.5 * u
    for fx, fy in ((0, 0), (5, 0), (0, 5)):
        d.rectangle([x0 + fx * u, y0 + fy * u, x0 + (fx + 4) * u - 1, y0 + (fy + 4) * u - 1], fill=col)
        d.rectangle([x0 + (fx + 0.8) * u, y0 + (fy + 0.8) * u, x0 + (fx + 3.2) * u - 1, y0 + (fy + 3.2) * u - 1],
                    fill=screens.T["cut"])
        d.rectangle([x0 + (fx + 1.5) * u, y0 + (fy + 1.5) * u, x0 + (fx + 2.5) * u - 1, y0 + (fy + 2.5) * u - 1], fill=col)
    for mx, my in ((5, 5), (7, 5), (6, 6), (5, 7), (8, 7), (7, 8), (5, 8.2)):
        d.rectangle([x0 + mx * u, y0 + my * u, x0 + (mx + 0.9) * u, y0 + (my + 0.9) * u], fill=col)


def configure(out, rgb565, url, night=False):
    saved = screens.T                                   # screens' theme is a module global: restore it
    screens.T = screens.NIGHT if night else screens.LIGHT
    try:
        return _configure(out, rgb565, url, "_night" if night else "")
    finally:
        screens.T = saved


def _configure(out, rgb565, url, sfx):
    T = screens.T
    f = screens.f
    scr = "configure" + sfx
    img, d = screens.panel_background("Configure Apps", "none")      # unknown kind: no header icon...
    qr_icon(d, 12 * SS, 12 * SS, 16 * SS, T["text"])                   # ...so draw ours
    x0, y0, x1, y1 = QR_BOX
    d.rounded_rectangle([(x0 - 4) * SS, (y0 - 4) * SS, (x1 + 4) * SS, (y1 + 4) * SS], radius=8 * SS,
                        fill=(255, 255, 255), outline=T["frame1"], width=SS)
    # right column: what to do
    tx = 186
    d.text((tx * SS, 36 * SS), "Choose your apps", font=f(screens.VERDB, 10), fill=T["text"])
    steps = ["Scan the code with", "your phone's camera.", "", "Add or remove apps,", "pick stocks, change", "the order.", "",
             "The Chumby updates", "at its next refresh."]
    for i, s in enumerate(steps):
        d.text((tx * SS, (56 + i * 13) * SS), s, font=f(screens.VERD, 8), fill=T["body"])
    shown = url[:-1] if url.endswith("/") and url.count("/") == 3 else url
    shown = shown.replace("http://", "")
    fnt = f(screens.VERDB, 9)
    while fnt.getlength(shown) > 228 * SS and fnt.size > 6 * SS:
        fnt = f(screens.VERDB, fnt.size / SS - 0.5)
    d.text((10 * SS, 212 * SS), "or visit", font=f(screens.VERD, 7), fill=T["hint"], anchor="ls")
    d.text((10 * SS, 226 * SS), shown, font=fnt, fill=T["text"], anchor="ls")
    screens.pill(d, screens.DONE_BOX, "BACK")
    im = screens.small(img)
    qr, ok = qr_image(url, x1 - x0)
    if not ok:
        ImageDraw.Draw(qr).text(((x1 - x0) // 2, (y1 - y0) // 2), "no QR library", fill=(0, 0, 0), anchor="mm")
    im.paste(qr, (x0, y0))                              # pasted after the downscale: no blurred modules
    import config
    if config.write_if_changed(rgb565, im, f"{out}/{scr}.raw") or not os.path.exists(f"{out}/{scr}.png"):
        im.save(f"{out}/{scr}.png")                     # static unless the URL changes: byte-identical renders
    b = screens.DONE_BOX
    return [f"screen {scr} {scr}.raw",
            f"hot {scr} {b[0] - 6} {b[1] - 6} {b[2] - b[0] + 12} 40 screen settings"]
