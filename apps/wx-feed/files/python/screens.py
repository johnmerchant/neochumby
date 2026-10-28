"""screens: the Home screen (2008 Chumby control panel look), Settings (the stock
Chumby settings screen's look) and the Music app (2008 Pandora-on-Chumby look),
pre-rendered for wxshow.

The device knows nothing about buttons or lists. Each screen is a background plus:
  pane   a picture chosen by a state variable (music, station, night, nightmode, pin, page, secs)
  hot    a tap zone with an action
  slider the volume slider
so new screens need no C changes. See wxshow.c for the actions.
"""
import io
import math
import re
import urllib.request

from PIL import Image, ImageDraw, ImageFont

import config
import wxassets

W, H = 320, 240
SS = 3                                           # supersampling for smooth 2008 gloss
FD = __file__.rsplit("/", 1)[0] + "/assets/fonts/"       # bundled open fonts, see assets/fonts/README.md
ROUND = FD + "SNPro-Bold.otf"                            # stands in for Arial Rounded Bold
VERD, VERDB = FD + "DejaVuSans.ttf", FD + "DejaVuSans-Bold.ttf"   # stand in for Verdana
NAVY = (40, 70, 150)
INK = (35, 60, 130)
UA = config.USER_AGENT


def paste_logo(img, cx, cy, height):
    """The octopus mark, centred at (cx, cy) on a supersampled image: octopus.png from the
    asset directories if there is one (see wxassets), else our own drawn octopus."""
    path = wxassets.find("octopus.png")
    if not path:
        icon(ImageDraw.Draw(img), "octo", cx, cy, height / 0.76, T["ink"])
        return
    lg = Image.open(path).convert("RGBA")
    w = int(lg.size[0] * height / lg.size[1])
    lg = lg.resize((w, int(height)), Image.LANCZOS)
    img.paste(lg, (int(cx - w / 2), int(cy - height / 2)), lg)


# Home's two themes: the 2008 pale-blue panel, and a dark navy one for night mode
LIGHT = dict(bg_top=(190, 205, 240), bg_bot=(225, 233, 252), swirl=(205, 216, 244), panel_top=(212, 222, 248),
             panel_bot=(236, 241, 254), sep=(170, 185, 225), text=NAVY, sub=(80, 100, 160), hint=(110, 130, 190),
             frame1=(150, 170, 220), frame2=(180, 195, 235), ring=(255, 255, 255), rim=(60, 90, 170), rim_off=(150, 160, 190),
             face=((205, 222, 255), (150, 180, 240)), face_off=((225, 228, 238), (200, 205, 220)), gloss=(232, 240, 255),
             gloss_off=(238, 240, 246), ink=INK, ink_off=(160, 168, 195), label=NAVY, label_off=(150, 160, 190),
             cut=(190, 210, 250), arrow=(255, 255, 255), thumb_edge=(120, 140, 200), dim=(20, 30, 70),
             box=(238, 242, 252), box_head=((200, 214, 248), (170, 190, 238)), body=(50, 60, 100))
NIGHT = dict(bg_top=(10, 14, 36), bg_bot=(24, 32, 70), swirl=(26, 36, 78), panel_top=(16, 22, 52),
             panel_bot=(28, 38, 82), sep=(48, 62, 115), text=(170, 190, 245), sub=(120, 140, 205), hint=(95, 115, 185),
             frame1=(58, 78, 145), frame2=(42, 56, 112), ring=(18, 24, 54), rim=(80, 105, 185), rim_off=(42, 52, 88),
             face=((52, 72, 138), (26, 40, 92)), face_off=((32, 38, 66), (22, 27, 52)), gloss=(78, 100, 165),
             gloss_off=(38, 46, 76), ink=(205, 218, 255), ink_off=(68, 78, 118), label=(150, 172, 238), label_off=(68, 78, 118),
             cut=(44, 62, 120), arrow=(40, 55, 110), thumb_edge=(70, 90, 160), dim=(0, 0, 10),
             box=(28, 36, 74), box_head=((50, 66, 128), (34, 46, 98)), body=(180, 195, 240))
T = LIGHT


STATIONS = ["groovesalad", "dronezone", "deepspaceone", "lush", "secretagent", "spacestation"]


def f(path, size):
    return ImageFont.truetype(path, size * SS)


def big(img):
    return img.resize((img.size[0] * SS, img.size[1] * SS))


def small(img):
    return img.resize((img.size[0] // SS, img.size[1] // SS), Image.LANCZOS)


def grad(d, box, top, bot, horizontal=False):
    x0, y0, x1, y1 = box
    n = (x1 - x0) if horizontal else (y1 - y0)
    for i in range(max(1, n)):
        t = i / max(1, n - 1)
        c = tuple(int(a + (b - a) * t) for a, b in zip(top, bot))
        if horizontal:
            d.line([(x0 + i, y0), (x0 + i, y1)], fill=c)
        else:
            d.line([(x0, y0 + i), (x1, y0 + i)], fill=c)


# ---------------------------------------------------------------- stations
def fetch(url):
    with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": UA}), timeout=30) as r:
        return r.read()


def get_stations():
    """Title, description, artwork and a plain-HTTP MP3 URL for each station."""
    import json
    chans = {c["id"]: c for c in json.loads(fetch("https://api.somafm.com/channels.json"))["channels"]}
    out = []
    for sid in STATIONS:
        c = chans.get(sid)
        if not c:
            continue
        url = f"http://ice6.somafm.com/{sid}-128-mp3"
        try:   # prefer what SomaFM's own MP3 playlist says, but only a plain-http server
            pls = fetch(f"https://api.somafm.com/{sid}.pls").decode("latin-1")
            m = re.search(r"File\d=(http://[^\s]+)", pls)
            if m:
                url = m.group(1).strip()
        except Exception:
            pass
        try:
            art = Image.open(io.BytesIO(fetch(c["image"]))).convert("RGB")
        except Exception:
            art = Image.new("RGB", (120, 120), (90, 110, 160))
        out.append({"id": sid, "title": c["title"], "desc": c["description"], "url": url, "art": art})
    return out


# ---------------------------------------------------------------- 2008 widgets
def glossy_button(d, cx, cy, r, enabled=True, pressed=False):
    """The Chumby control panel's round button: navy rim, pale gradient face, top gloss."""
    rim = T["rim"] if enabled else T["rim_off"]
    d.ellipse([cx - r - SS * 2, cy - r - SS * 2, cx + r + SS * 2, cy + r + SS * 2], fill=T["ring"])
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=rim)
    ir = r - SS * 2
    for i in range(int(ir * 2)):                                  # vertical gradient face
        y = cy - ir + i
        t = i / (ir * 2)
        top, bot = T["face"] if enabled else T["face_off"]
        if pressed:
            top, bot = (120, 200, 120), (60, 150, 70)
        c = tuple(int(a + (b - a) * t) for a, b in zip(top, bot))
        half = math.sqrt(max(0, ir * ir - (y - cy) ** 2))
        d.line([(cx - half, y), (cx + half, y)], fill=c)
    d.ellipse([cx - ir * .7, cy - ir * .92, cx + ir * .7, cy - ir * .05], fill=T["gloss"] if enabled else T["gloss_off"])


def arched_label(img, cx, cy, r, text, size=8, color=None):
    """Text curved over the top of a round button, like the Chumby's MUTE / CHANNEL labels."""
    color = color or T["label"]
    fnt = f(VERDB, size)
    widths = [fnt.getlength(ch) for ch in text]
    total = sum(widths) + SS * 0.6 * (len(text) - 1)
    rr = r + SS * 9
    ang = -math.pi / 2 - (total / rr) / 2                          # start angle (left), sweep right
    for ch, w in zip(text, widths):
        a = ang + (w / 2) / rr
        tile = Image.new("RGBA", (int(w) + SS * 6, size * SS * 2), (0, 0, 0, 0))
        ImageDraw.Draw(tile).text((tile.size[0] / 2, tile.size[1] / 2), ch, font=fnt, fill=color, anchor="mm")
        tile = tile.rotate(-math.degrees(a + math.pi / 2), resample=Image.BICUBIC, expand=True)
        x, y = cx + rr * math.cos(a), cy + rr * math.sin(a)
        img.paste(tile, (int(x - tile.size[0] / 2), int(y - tile.size[1] / 2)), tile)
        ang += (w + SS * 0.6) / rr


def icon(d, kind, cx, cy, s, col=None, muted=False):
    col = col or T["ink"]
    w = max(SS, int(s * .1))
    if kind == "speaker":
        d.polygon([(cx - s * .38, cy - s * .14), (cx - s * .16, cy - s * .14), (cx + s * .08, cy - s * .36),
                   (cx + s * .08, cy + s * .36), (cx - s * .16, cy + s * .14), (cx - s * .38, cy + s * .14)], fill=col)
        if muted:
            d.line([(cx + s * .18, cy - s * .16), (cx + s * .42, cy + s * .16)], fill=(200, 40, 40), width=w)
            d.line([(cx + s * .18, cy + s * .16), (cx + s * .42, cy - s * .16)], fill=(200, 40, 40), width=w)
        else:
            for k in (1, 2):
                rr = s * (.16 + .13 * k)
                d.arc([cx + s * .02 - rr, cy - rr, cx + s * .02 + rr, cy + rr], -45, 45, fill=col, width=w)
    elif kind == "clock":
        d.ellipse([cx - s * .38, cy - s * .38, cx + s * .38, cy + s * .38], outline=col, width=w)
        for k in range(12):
            a = k * math.pi / 6
            d.line([(cx + s * .3 * math.cos(a), cy + s * .3 * math.sin(a)), (cx + s * .36 * math.cos(a), cy + s * .36 * math.sin(a))], fill=col, width=SS)
        d.line([(cx, cy), (cx + s * .16, cy - s * .2)], fill=col, width=w)
        d.line([(cx, cy), (cx - s * .2, cy + s * .05)], fill=col, width=w)
    elif kind == "moon":
        d.ellipse([cx - s * .34, cy - s * .34, cx + s * .34, cy + s * .34], fill=col)
        d.ellipse([cx - s * .16, cy - s * .44, cx + s * .5, cy + s * .2], fill=T["cut"])
    elif kind == "note":
        for x in (cx - s * .24, cx + s * .22):
            d.ellipse([x - s * .15, cy + s * .1, x + s * .1, cy + s * .3], fill=col)
            d.rectangle([x + s * .06, cy - s * .3, x + s * .06 + w, cy + s * .22], fill=col)
        d.polygon([(cx - s * .18 + w, cy - s * .3), (cx + s * .28 + w, cy - s * .36), (cx + s * .28 + w, cy - s * .24), (cx - s * .18 + w, cy - s * .18)], fill=col)
    elif kind == "wrench":
        d.line([(cx - s * .26, cy + s * .26), (cx + s * .14, cy - s * .14)], fill=col, width=int(w * 1.8))
        d.ellipse([cx + s * .02, cy - s * .38, cx + s * .38, cy - s * .02], fill=col)
        d.ellipse([cx + s * .14, cy - s * .3, cx + s * .3, cy - s * .14], fill=T["cut"])
    elif kind == "bell":
        d.chord([cx - s * .3, cy - s * .36, cx + s * .3, cy + s * .3], 180, 360, fill=col)
        d.rectangle([cx - s * .3, cy - s * .04, cx + s * .3, cy + s * .2], fill=col)
        d.ellipse([cx - s * .08, cy + s * .2, cx + s * .08, cy + s * .34], fill=col)
    elif kind == "pin":
        d.ellipse([cx - s * .2, cy - s * .38, cx + s * .2, cy + s * .02], fill=col if col == T["ink_off"] else (210, 60, 50))
        d.line([(cx, cy), (cx - s * .2, cy + s * .38)], fill=(120, 120, 130), width=w)
    elif kind == "next":
        for dx in (-s * .2, s * .1):
            d.polygon([(cx + dx - s * .12, cy - s * .26), (cx + dx + s * .16, cy), (cx + dx - s * .12, cy + s * .26)], fill=col)
    elif kind == "octo":
        d.ellipse([cx - s * .3, cy - s * .38, cx + s * .3, cy + s * .14], fill=col)
        for k in range(5):
            x = cx - s * .28 + k * s * .14
            d.line([(x, cy + s * .05), (x - s * .04 + (k - 2) * s * .05, cy + s * .38)], fill=col, width=int(w * 1.2))
        for ex in (cx - s * .12, cx + s * .12):
            d.ellipse([ex - s * .07, cy - s * .2, ex + s * .07, cy - s * .06], fill=(255, 255, 255))
    elif kind in ("play", "pause"):
        if kind == "play":
            d.polygon([(cx - s * .16, cy - s * .26), (cx + s * .26, cy), (cx - s * .16, cy + s * .26)], fill=col)
        else:
            d.rectangle([cx - s * .22, cy - s * .24, cx - s * .06, cy + s * .24], fill=col)
            d.rectangle([cx + s * .06, cy - s * .24, cx + s * .22, cy + s * .24], fill=col)
    elif kind == "wifi":
        d.ellipse([cx - s * .07, cy + s * .2, cx + s * .07, cy + s * .34], fill=col)
        for k in (1, 2, 3):
            rr = s * .15 * k
            d.arc([cx - rr, cy + s * .27 - rr, cx + rr, cy + s * .27 + rr], 225, 315, fill=col, width=w)
    elif kind == "qr":
        import qrscreen                                            # lazy: qrscreen imports this module
        qrscreen.qr_icon(d, cx, cy, s * .62, col)
    elif kind == "news":                                           # folded newspaper
        d.rectangle([cx - s * .34, cy - s * .28, cx + s * .34, cy + s * .3], outline=col, width=w)
        d.rectangle([cx - s * .24, cy - s * .18, cx - s * .02, cy + s * .02], fill=col)
        for k in range(4):
            y = cy - s * .16 + k * s * .11
            d.line([(cx + s * .04, y), (cx + s * .24, y)], fill=col, width=w)
        d.line([(cx - s * .24, cy + s * .17), (cx + s * .24, cy + s * .17)], fill=col, width=w)
    elif kind in ("minus", "plus"):
        d.rectangle([cx - s * .26, cy - w * .7, cx + s * .26, cy + w * .7], fill=col)
        if kind == "plus":
            d.rectangle([cx - w * .7, cy - s * .26, cx + w * .7, cy + s * .26], fill=col)


def slider_assets(width, track_x, knob_kind="speaker"):
    """Empty and full slider strips plus a keyed knob, in the 2008 style."""
    h = 26
    imgs = []
    for full in (False, True):
        im = Image.new("RGBA", (width * SS, h * SS), (0, 0, 0, 0))
        d = ImageDraw.Draw(im)
        t0, t1 = track_x
        d.rounded_rectangle([t0 * SS, 10 * SS, t1 * SS, 16 * SS], radius=3 * SS,
                            fill=(70, 110, 200) if full else (205, 212, 232), outline=(110, 130, 180), width=SS)
        imgs.append(im)
    k = 24
    knob = Image.new("RGBA", (k * SS, k * SS), (0, 0, 0, 0))
    kd = ImageDraw.Draw(knob)
    glossy_button(kd, k * SS / 2, k * SS / 2, k * SS / 2 - SS * 3)
    icon(kd, knob_kind, k * SS / 2, k * SS / 2, k * SS * .62)
    return imgs, knob, k, h


def keyed(im_ss, key=(255, 0, 255)):
    """Supersampled RGBA -> device-size RGB with the colour key where transparent.
    Resized in premultiplied alpha so no key colour bleeds into the edges."""
    sm = im_ss.convert("RGBa").resize((im_ss.size[0] // SS, im_ss.size[1] // SS), Image.LANCZOS).convert("RGBA")
    out = Image.new("RGB", sm.size, key)
    out.paste(sm.convert("RGB"), (0, 0), sm.getchannel("A").point(lambda a: 255 if a >= 110 else 0))
    return out


def on_bg(bg_crop, rgba):
    """Composite a supersampled RGBA drawing over a crop of the real background (no keying needed)."""
    base = big(bg_crop).convert("RGBA")
    base.alpha_composite(rgba)
    return small(base.convert("RGB"))


# ---------------------------------------------------------------- Home
HOME_BTNS = [  # (label, icon, cx, cy, action)
    ("MUTE", "speaker", 228, 82, "music_toggle"),
    ("CHANNEL", "clock", 284, 82, "next_app"),
    ("NIGHT", "moon", 228, 140, "night"),
    ("MUSIC", "note", 284, 140, "screen music"),
    ("SETTINGS", "wrench", 228, 198, "screen settings"),
    ("ALARMS", "bell", 284, 198, "none"),                     # as on the real Chumby (not wired up)
]
HOME_BOTTOM = [("STAY", "pin", 32, 214, "pin"), ("NEXT", "next", 84, 214, "next_app"), ("CHUMBY", "octo", 148, 214, "screen confirm")]
BTN_R = 17
TOGGLES = {"night": "night", "pin": "pin"}   # action -> state variable; greyed when off, normal when on


def home_background():
    img = Image.new("RGB", (W * SS, H * SS))
    d = ImageDraw.Draw(img)
    grad(d, (0, 0, W * SS, H * SS), T["bg_top"], T["bg_bot"])
    for k in range(9):                                             # the faint swirl of the 2008 panel
        r = (60 + k * 26) * SS
        d.arc([(-40 * SS) - r, (300 * SS) - r, (-40 * SS) + r, (300 * SS) + r], 270, 360, fill=T["swirl"], width=SS * 3)
    grad(d, (196 * SS, 0, W * SS, H * SS), T["panel_top"], T["panel_bot"])   # right panel
    d.line([(196 * SS, 0), (196 * SS, H * SS)], fill=T["sep"], width=SS)
    d.text((24 * SS, 4 * SS), "Slideshow", font=f(VERDB, 10), fill=T["text"])
    paste_logo(img, 12 * SS, 10 * SS, 14 * SS)
    for i in range(4):                                             # signal bars, as on the real panel
        x = (296 + i * 4) * SS
        d.rectangle([x, (14 - i * 2) * SS, x + 2 * SS, 15 * SS], fill=(60, 160, 70))
    # left card frame (dashed, rounded) where the app thumbnail sits
    x0, y0, x1, y1 = 22 * SS, 52 * SS, 176 * SS, 168 * SS
    d.rounded_rectangle([x0, y0, x1, y1], radius=14 * SS, outline=T["frame1"], width=SS)
    d.rounded_rectangle([x0 + 5 * SS, y0 + 5 * SS, x1 - 5 * SS, y1 - 5 * SS], radius=10 * SS, outline=T["frame2"], width=SS)
    d.text(((x0 + x1) / 2, y1 - 9 * SS), "TAP TO RESUME", font=f(VERDB, 7), fill=T["hint"], anchor="mm")
    for label, kind, cx, cy, action in HOME_BTNS + HOME_BOTTOM:
        if action in TOGGLES:                                      # drawn entirely by their panes
            continue
        enabled = action != "none"
        glossy_button(d, cx * SS, cy * SS, BTN_R * SS, enabled)
        if kind == "octo":
            paste_logo(img, cx * SS, cy * SS + SS, BTN_R * SS * 1.05)
        elif kind != "speaker":                                    # the MUTE icon is a pane (changes with state)
            icon(d, kind, cx * SS, cy * SS, BTN_R * SS * 1.1, T["ink"] if enabled else T["ink_off"])
    out = small(img)
    big_out = big(out).convert("RGBA")
    for label, kind, cx, cy, action in HOME_BTNS + HOME_BOTTOM:
        if action not in TOGGLES:
            arched_label(big_out, cx * SS, cy * SS, BTN_R * SS, label, 7, T["label"] if action != "none" else T["label_off"])
    return small(big_out.convert("RGB"))


def home(out, rgb565, page_titles, page_thumbs, night=False):
    """Home screen files + manifest lines. page_titles/page_thumbs: one per app in the channel (its name
    and its first page as a 320x240 PIL image); the card follows wxshow's "app" (pageapp lines).
    night=True renders the dark theme as screen "home_night"; wxshow shows it while night mode is on."""
    global T
    T = NIGHT if night else LIGHT
    sfx = "_night" if night else ""
    scr = "home" + sfx
    bg = home_background()
    bg.save(f"{out}/{scr}.png")
    rgb565(bg, f"{out}/{scr}.raw")
    lines = [f"screen {scr} {scr}.raw"]
    # left card: app title, "n of N", arrows and a thumbnail, one frame per app
    cw, ch = 180, 150
    cx0, cy0 = 8, 22
    frames = []
    for i, (title, thumb) in enumerate(zip(page_titles, page_thumbs)):
        crop = bg.crop((cx0, cy0, cx0 + cw, cy0 + ch))
        im = Image.new("RGBA", (cw * SS, ch * SS), (0, 0, 0, 0))
        d = ImageDraw.Draw(im)
        for ax, pts in ((8, [(14, 8), (4, 16), (14, 24)]), (172, [(166, 8), (176, 16), (166, 24)])):
            d.polygon([(x * SS, y * SS) for x, y in pts], fill=T["arrow"], outline=T["text"])
        d.text((90 * SS, 8 * SS), title, font=f(VERDB, 11), fill=T["text"], anchor="ma")
        d.text((90 * SS, 22 * SS), f"{i + 1} of {len(page_titles)}", font=f(VERDB, 8), fill=T["sub"], anchor="ma")
        th = thumb.resize((112 * SS, 84 * SS), Image.LANCZOS).convert("RGBA")
        d.rectangle([(34 - 2) * SS, (40 - 2) * SS, (34 + 112 + 1) * SS, (40 + 84 + 1) * SS], fill=(255, 255, 255), outline=T["thumb_edge"], width=SS)
        im.alpha_composite(th, (34 * SS, 40 * SS))
        frames.append(on_bg(crop, im))
    sheet = Image.new("RGB", (cw, ch * len(frames)))
    for i, fr in enumerate(frames):
        sheet.paste(fr, (0, i * ch))
    rgb565(sheet, f"{out}/{scr}_card.raw")
    lines.append(f"pane {scr} {scr}_card.raw {cw} {ch} {cx0} {cy0} app {len(frames)}")
    # MUTE button face: 0 = music off (muted), 1 = playing
    mx, my = 228, 82
    s = BTN_R * 2 + 4
    crop = bg.crop((mx - s // 2, my - s // 2, mx + s // 2, my + s // 2))
    mframes = []
    for state in (0, 1):
        im = Image.new("RGBA", (s * SS, s * SS), (0, 0, 0, 0))
        d = ImageDraw.Draw(im)
        glossy_button(d, s * SS / 2, s * SS / 2, BTN_R * SS)
        icon(d, "speaker", s * SS / 2, s * SS / 2, BTN_R * SS * 1.1, muted=state == 0)
        mframes.append(on_bg(crop, im))
    msheet = Image.new("RGB", (s, s * 2))
    for i, fr in enumerate(mframes):
        msheet.paste(fr, (0, i * s))
    rgb565(msheet, f"{out}/{scr}_mute.raw")
    lines.append(f"pane {scr} {scr}_mute.raw {s} {s} {mx - s // 2} {my - s // 2} music 2")
    # toggles (NIGHT, STAY): drawn like a disabled button when off, label
    # included, and as a normal button when on. Pane covers the button and its arched label.
    for label, kind, cx, cy, action in HOME_BTNS + HOME_BOTTOM:
        if action not in TOGGLES:
            continue
        pw, ph, top = 56, 56, 34                                   # top: label height above the centre
        px0, py0 = cx - pw // 2, cy - top
        crop = bg.crop((px0, py0, px0 + pw, py0 + ph))
        frs = []
        for state in (0, 1):
            im = Image.new("RGBA", (pw * SS, ph * SS), (0, 0, 0, 0))
            d = ImageDraw.Draw(im)
            bx, by = pw * SS / 2, top * SS
            glossy_button(d, bx, by, BTN_R * SS, enabled=state == 1)
            icon(d, kind, bx, by, BTN_R * SS * 1.1, T["ink"] if state else T["ink_off"])
            arched_label(im, bx, by, BTN_R * SS, label, 7, T["label"] if state else T["label_off"])
            frs.append(on_bg(crop, im))
        sh = Image.new("RGB", (pw, ph * 2))
        for i, fr in enumerate(frs):
            sh.paste(fr, (0, i * ph))
        rgb565(sh, f"{out}/{scr}_btn_{action}.raw")                # not {scr}_night: that's the dark screen
        lines.append(f"pane {scr} {scr}_btn_{action}.raw {pw} {ph} {px0} {py0} {TOGGLES[action]} 2")
    # tap zones (generous: resistive panel, fingers)
    lines.append(f"hot {scr} 22 60 154 110 slideshow")
    lines.append(f"hot {scr} 4 22 40 32 prev_app")
    lines.append(f"hot {scr} 152 22 40 32 next_app")
    for label, kind, cx, cy, action in HOME_BTNS + HOME_BOTTOM:
        if action != "none":
            lines.append(f"hot {scr} {cx - 24} {cy - 24} 48 48 {action}")
    lines += confirm_screen(out, rgb565, bg, sfx)
    # volume slider across the top of the right panel
    sw, sx, sy = 120, 196, 22
    (empty, full), knob, k, sh_ = slider_assets(sw, (12, 106))
    for name, im in ((f"{scr}_vol_empty", empty), (f"{scr}_vol_full", full)):
        rgb565(on_bg(bg.crop((sx, sy, sx + sw, sy + sh_)), im), f"{out}/{name}.raw")
    rgb565(keyed(knob), f"{out}/{scr}_vol_knob.raw")
    lines.append(f"slider {scr} {scr}_vol_empty.raw {scr}_vol_full.raw {scr}_vol_knob.raw {sw} {sh_} {sx} {sy} 12 106 {k} {k}")
    return lines


def pill(d, box, label, primary=False):
    x0, y0, x1, y1 = [v * SS for v in box]
    top, bot = ((120, 160, 235), (60, 100, 200)) if primary else ((252, 252, 255), (218, 224, 238))
    d.rounded_rectangle([x0, y0, x1, y1], radius=(y1 - y0) // 2, fill=bot, outline=(90, 110, 170), width=SS)
    d.rounded_rectangle([x0 + SS * 2, y0 + SS * 2, x1 - SS * 2, (y0 + y1) // 2], radius=(y1 - y0) // 3, fill=top)
    d.text(((x0 + x1) / 2, (y0 + y1) / 2), label, font=f(VERDB, 10), fill=(255, 255, 255) if primary else NAVY, anchor="mm")


def confirm_screen(out, rgb565, home_bg, sfx=""):
    """'Leave for the classic Chumby?' over a dimmed Home, so CHUMBY can't be hit by accident."""
    dim = Image.blend(home_bg, Image.new("RGB", home_bg.size, T["dim"]), 0.55)
    img = big(dim)
    d = ImageDraw.Draw(img)
    x0, y0, x1, y1 = 40, 62, 280, 178
    d.rounded_rectangle([(x0 + 3) * SS, (y0 + 4) * SS, (x1 + 3) * SS, (y1 + 4) * SS], radius=14 * SS, fill=(10, 15, 40))
    d.rounded_rectangle([x0 * SS, y0 * SS, x1 * SS, y1 * SS], radius=14 * SS, fill=T["box"], outline=T["rim"], width=SS * 2)
    grad(d, (x0 * SS + SS * 2, y0 * SS + SS * 2, x1 * SS - SS * 2, (y0 + 26) * SS), *T["box_head"])
    paste_logo(img, (x0 + 20) * SS, (y0 + 13) * SS, 16 * SS)
    d.text(((x0 + 34) * SS, (y0 + 13) * SS), "Classic Chumby", font=f(VERDB, 11), fill=T["text"], anchor="lm")
    d.text((160 * SS, (y0 + 46) * SS), "Leave the slideshow and go back", font=f(VERD, 9), fill=T["body"], anchor="mm")
    d.text((160 * SS, (y0 + 60) * SS), "to the original Chumby screen?", font=f(VERD, 9), fill=T["body"], anchor="mm")
    pill(d, (x0 + 18, y1 - 38, 150, y1 - 14), "Cancel")
    pill(d, (170, y1 - 38, x1 - 18, y1 - 14), "Yes, leave", primary=True)
    im = small(img)
    c = "confirm" + sfx
    im.save(f"{out}/{c}.png")
    rgb565(im, f"{out}/{c}.raw")
    return [f"screen {c} {c}.raw",
            f"hot {c} {x0 + 12} {y1 - 44} {150 - x0 - 6} 36 screen home",
            f"hot {c} 164 {y1 - 44} {x1 - 164 - 6} 36 chumby",
            f"hot {c} 0 0 320 56 screen home",               # tapping outside the dialog cancels
            f"hot {c} 0 {y1 + 6} 320 {240 - y1 - 6} screen home"]


# ---------------------------------------------------------------- Music (Pandora-on-Chumby style)
def music_background():
    img = Image.new("RGB", (W * SS, H * SS))
    d = ImageDraw.Draw(img)
    grad(d, (0, 0, W * SS, H * SS), (236, 238, 242), (214, 218, 226))
    grad(d, (0, 0, W * SS, 22 * SS), (250, 250, 252), (222, 226, 234))         # top bar
    d.line([(0, 22 * SS), (W * SS, 22 * SS)], fill=(170, 175, 190), width=SS)
    d.text((10 * SS, 4 * SS), "SOMA", font=f(ROUND, 13), fill=(30, 40, 70))
    d.text((52 * SS, 4 * SS), "FM", font=f(ROUND, 13), fill=(200, 60, 40))
    d.text((84 * SS, 7 * SS), "listener-supported radio", font=f(VERD, 7), fill=(120, 125, 140))
    # station list panel
    d.rounded_rectangle([6 * SS, 26 * SS, 176 * SS, 206 * SS], radius=4 * SS, fill=(255, 255, 255), outline=(170, 175, 190), width=SS)
    grad(d, (7 * SS, 27 * SS, 175 * SS, 41 * SS), (232, 234, 240), (208, 212, 222))
    d.text((12 * SS, 29 * SS), "Your Stations", font=f(VERDB, 8), fill=(70, 75, 95))
    # now-playing panel
    d.rounded_rectangle([182 * SS, 26 * SS, 314 * SS, 206 * SS], radius=4 * SS, fill=(250, 251, 253), outline=(170, 175, 190), width=SS)
    # transport bar
    grad(d, (0, 210 * SS, W * SS, H * SS), (205, 210, 220), (175, 180, 194))
    d.line([(0, 210 * SS), (W * SS, 210 * SS)], fill=(150, 155, 170), width=SS)
    d.rounded_rectangle([262 * SS, 215 * SS, 312 * SS, 235 * SS], radius=9 * SS, fill=(245, 246, 250), outline=(120, 125, 145), width=SS)
    d.text((287 * SS, 225 * SS), "DONE", font=f(VERDB, 9), fill=(50, 55, 75), anchor="mm")
    for cx, kind in ((52, "next"),):                               # skip button (static)
        glossy_button(d, cx * SS, 225 * SS, 11 * SS)
        icon(d, kind, cx * SS, 225 * SS, 13 * SS)
    return small(img)


ROW_Y0, ROW_H = 43, 27


def music(out, rgb565, stations):
    bg = music_background()
    bg.save(f"{out}/music.png")
    rgb565(bg, f"{out}/music.raw")
    lines = ["screen music music.raw"]
    for i, s in enumerate(stations):
        lines.append(f"station {i} {s['id']} {s['url']}")
    # station list: one frame per selected station
    lw, lh = 168, ROW_H * len(stations)
    lx, ly = 7, ROW_Y0
    frames = []
    for sel in range(len(stations)):
        im = Image.new("RGB", (lw * SS, lh * SS), (255, 255, 255))
        d = ImageDraw.Draw(im)
        for i, s in enumerate(stations):
            y = i * ROW_H * SS
            if i == sel:
                grad(d, (0, y, lw * SS, y + ROW_H * SS - SS), (110, 150, 230), (60, 100, 200))
                col = (255, 255, 255)
            else:
                col = (40, 45, 65)
            d.line([(0, y + ROW_H * SS - SS), (lw * SS, y + ROW_H * SS - SS)], fill=(225, 228, 236), width=SS)
            d.text((8 * SS, y + ROW_H * SS / 2), s["title"], font=f(VERDB if i == sel else VERD, 10), fill=col, anchor="lm")
        frames.append(small(im))
    sheet = Image.new("RGB", (lw, lh * len(frames)))
    for i, fr in enumerate(frames):
        sheet.paste(fr, (0, i * lh))
    rgb565(sheet, f"{out}/music_list.raw")
    lines.append(f"pane music music_list.raw {lw} {lh} {lx} {ly} station {len(frames)}")
    for i in range(len(stations)):
        lines.append(f"hot music {lx} {ly + i * ROW_H} {lw} {ROW_H} station {i}")
    # now-playing card: art + title + description, one frame per station
    cw, chh, cx, cy = 130, 178, 183, 27
    cards = []
    for s in stations:
        im = Image.new("RGB", (cw * SS, chh * SS), (250, 251, 253))
        d = ImageDraw.Draw(im)
        d.text((cw * SS / 2, 6 * SS), s["title"], font=f(VERDB, 10), fill=(30, 40, 70), anchor="ma")
        art = s["art"].resize((92 * SS, 92 * SS), Image.LANCZOS)
        ax = (cw - 92) // 2
        d.rectangle([(ax - 2) * SS, 22 * SS, (ax + 93) * SS, 117 * SS], fill=(255, 255, 255), outline=(160, 165, 180), width=SS)
        im.paste(art, (ax * SS, 24 * SS))
        fnt = f(VERD, 7)
        words, line, y = s["desc"].split(), "", 122
        for w in words:
            t = (line + " " + w).strip()
            if fnt.getlength(t) > (cw - 10) * SS:
                d.text((cw * SS / 2, y * SS), line, font=fnt, fill=(80, 85, 105), anchor="ma")
                line, y = w, y + 10
                if y > chh - 12:
                    break
            else:
                line = t
        if y <= chh - 12:
            d.text((cw * SS / 2, y * SS), line, font=fnt, fill=(80, 85, 105), anchor="ma")
        cards.append(small(im))
    csheet = Image.new("RGB", (cw, chh * len(cards)))
    for i, fr in enumerate(cards):
        csheet.paste(fr, (0, i * chh))
    rgb565(csheet, f"{out}/music_card.raw")
    lines.append(f"pane music music_card.raw {cw} {chh} {cx} {cy} station {len(cards)}")
    # play/pause: 0 = stopped (show play), 1 = playing (show pause)
    ps, px, py = 26, 8, 212
    crop = bg.crop((px, py, px + ps, py + ps))
    frs = []
    for state in (0, 1):
        im = Image.new("RGBA", (ps * SS, ps * SS), (0, 0, 0, 0))
        d = ImageDraw.Draw(im)
        glossy_button(d, ps * SS / 2, ps * SS / 2, 11 * SS)
        icon(d, "pause" if state else "play", ps * SS / 2, ps * SS / 2, 13 * SS)
        frs.append(on_bg(crop, im))
    sh = Image.new("RGB", (ps, ps * 2))
    for i, fr in enumerate(frs):
        sh.paste(fr, (0, i * ps))
    rgb565(sh, f"{out}/music_play.raw")
    lines.append(f"pane music music_play.raw {ps} {ps} {px} {py} music 2")
    lines += ["hot music 2 210 34 30 music_toggle", "hot music 36 210 32 30 next_station",
              "hot music 256 210 62 30 screen home"]
    # volume slider in the transport bar
    sw, sx, sy = 186, 70, 212
    (empty, full), knob, k, sh_ = slider_assets(sw, (10, 176))
    for name, im in (("music_vol_empty", empty), ("music_vol_full", full)):
        rgb565(on_bg(bg.crop((sx, sy, sx + sw, sy + sh_)), im), f"{out}/{name}.raw")
    rgb565(keyed(knob), f"{out}/music_vol_knob.raw")
    lines.append(f"slider music music_vol_empty.raw music_vol_full.raw music_vol_knob.raw {sw} {sh_} {sx} {sy} 10 176 {k} {k}")
    return lines


# ---------------------------------------------------------------- Settings (the stock Chumby settings screen)
SECS_OPTS = [8, 12, 20, 30, 60]             # the timings SLIDESHOW steps through, after AUTO
SET_R = 22
SET_BTNS = [  # (label, icon, cx, cy, action)
    ("CHUMBY INFO", "octo", 64, 86, "screen about"),
    ("SLIDESHOW", "clock", 160, 86, "screen timing"),
    ("NETWORK", "wifi", 256, 86, "screen network"),
    ("STAY", "pin", 64, 168, "pin"),
    ("NIGHT", "moon", 160, 168, "nightmode"),             # cycles auto -> on -> off (state nightmode)
    ("APPS", "qr", 256, 168, "screen configure"),          # QR code for the config web page (qrscreen.py)
]
DONE_BOX = (248, 206, 308, 230)
# Settings' toggle-looking buttons: action -> the state variable that lights them. NIGHT here
# cycles the night mode (its label below says which); lit while night mode is actually on.
SET_TOGGLES = {"pin": "pin", "nightmode": "night"}
NIGHT_MODES = ["auto (sunset)", "always on", "always off"]   # nightmode 0, 1, 2


def panel_background(title, kind):
    """The pale-blue (or night navy) 2008 panel with a header bar: small icon + title, top left."""
    img = Image.new("RGB", (W * SS, H * SS))
    d = ImageDraw.Draw(img)
    grad(d, (0, 0, W * SS, H * SS), T["bg_top"], T["bg_bot"])
    for k in range(9):
        r = (60 + k * 26) * SS
        d.arc([(-40 * SS) - r, (300 * SS) - r, (-40 * SS) + r, (300 * SS) + r], 270, 360, fill=T["swirl"], width=SS * 3)
    grad(d, (0, 0, W * SS, 24 * SS), T["panel_top"], T["panel_bot"])
    d.line([(0, 24 * SS), (W * SS, 24 * SS)], fill=T["sep"], width=SS)
    if kind == "octo":
        paste_logo(img, 12 * SS, 12 * SS, 15 * SS)
    else:
        icon(d, kind, 12 * SS, 12 * SS, 16 * SS, T["text"])
    d.text((24 * SS, 5 * SS), title, font=f(VERDB, 10), fill=T["text"])
    return img, d


def screen_lines(out, rgb565, img_ss, name):
    im = small(img_ss)
    im.save(f"{out}/{name}.png")
    rgb565(im, f"{out}/{name}.raw")
    return im, [f"screen {name} {name}.raw"]


def done_pill(d, label="DONE"):
    pill(d, DONE_BOX, label)


def settings(out, rgb565, info, night=False):
    """Settings and its sub-screens (timing, network, about), as screens settings, timing,
    network, about (+ _night). info: dict of strings the feeder gathered from the Chumby.
    APPS opens screen "configure" (the QR code), which wxfeed adds from qrscreen.py."""
    global T
    T = NIGHT if night else LIGHT
    sfx = "_night" if night else ""
    scr = "settings" + sfx
    img, d = panel_background("Settings", "wrench")
    for label, kind, cx, cy, action in SET_BTNS:
        if action in SET_TOGGLES:                                      # drawn by their panes
            continue
        glossy_button(d, cx * SS, cy * SS, SET_R * SS)
        if kind == "octo":
            paste_logo(img, cx * SS, cy * SS + SS, SET_R * SS * 1.05)
        else:
            icon(d, kind, cx * SS, cy * SS, SET_R * SS * 1.1)
    done_pill(d)
    big_out = big(small(img)).convert("RGBA")
    for label, kind, cx, cy, action in SET_BTNS:
        if action not in SET_TOGGLES:
            arched_label(big_out, cx * SS, cy * SS, SET_R * SS, label, 8)
    bg, lines = screen_lines(out, rgb565, big_out.convert("RGB"), scr)
    # STAY / NIGHT: the same toggles as on Home (state pin / night), drawn greyed when off
    for label, kind, cx, cy, action in SET_BTNS:
        if action not in SET_TOGGLES:
            continue
        pw, ph, top = 72, 70, 42
        px0, py0 = cx - pw // 2, cy - top
        crop = bg.crop((px0, py0, px0 + pw, py0 + ph))
        frs = []
        for state in (0, 1):
            im = Image.new("RGBA", (pw * SS, ph * SS), (0, 0, 0, 0))
            dd = ImageDraw.Draw(im)
            bx, by = pw * SS / 2, top * SS
            glossy_button(dd, bx, by, SET_R * SS, enabled=state == 1)
            icon(dd, kind, bx, by, SET_R * SS * 1.1, T["ink"] if state else T["ink_off"])
            arched_label(im, bx, by, SET_R * SS, label, 8, T["label"] if state else T["label_off"])
            frs.append(on_bg(crop, im))
        lines += sheet_pane(out, rgb565, frs, f"{scr}_btn_{action}", scr, px0, py0, SET_TOGGLES[action])
    # the current timing, under SLIDESHOW
    vw, vh, vx, vy = 80, 14, 120, 112
    crop = bg.crop((vx, vy, vx + vw, vy + vh))
    frs = []
    for k in range(len(SECS_OPTS) + 1):
        im = Image.new("RGBA", (vw * SS, vh * SS), (0, 0, 0, 0))
        ImageDraw.Draw(im).text((vw * SS / 2, vh * SS / 2), "auto" if k == 0 else f"{SECS_OPTS[k - 1]} s per page",
                                font=f(VERD, 7), fill=T["sub"], anchor="mm")
        frs.append(on_bg(crop, im))
    lines += sheet_pane(out, rgb565, frs, f"{scr}_secs", scr, vx, vy, "secs")
    # the night mode, under NIGHT (the same place and look as the timing under SLIDESHOW)
    vx = 160 - vw // 2
    vy = 168 + (vy - 86)
    crop = bg.crop((vx, vy, vx + vw, vy + vh))
    frs = []
    for label in NIGHT_MODES:
        im = Image.new("RGBA", (vw * SS, vh * SS), (0, 0, 0, 0))
        ImageDraw.Draw(im).text((vw * SS / 2, vh * SS / 2), label, font=f(VERD, 7), fill=T["sub"], anchor="mm")
        frs.append(on_bg(crop, im))
    lines += sheet_pane(out, rgb565, frs, f"{scr}_nightmode", scr, vx, vy, "nightmode")
    for label, kind, cx, cy, action in SET_BTNS:
        lines.append(f"hot {scr} {cx - 30} {cy - 34} 60 64 {action}")
    lines.append(f"hot {scr} {DONE_BOX[0] - 6} {DONE_BOX[1] - 6} {DONE_BOX[2] - DONE_BOX[0] + 12} 40 screen home")
    if not night:
        lines.append("secsopts " + " ".join(map(str, SECS_OPTS)))
    lines += timing_screen(out, rgb565, sfx)
    lines += info_screen(out, rgb565, "network" + sfx, "Network", "wifi", network_rows(info), info)
    lines += info_screen(out, rgb565, "about" + sfx, "Chumby Info", "octo", about_rows(info), info)
    return lines


def sheet_pane(out, rgb565, frames, name, scr, x, y, var):
    w, h = frames[0].size
    sheet = Image.new("RGB", (w, h * len(frames)))
    for i, fr in enumerate(frames):
        sheet.paste(fr, (0, i * h))
    rgb565(sheet, f"{out}/{name}.raw")
    return [f"pane {scr} {name}.raw {w} {h} {x} {y} {var} {len(frames)}"]


def timing_screen(out, rgb565, sfx):
    """SLIDESHOW: how long each slideshow page shows. - / + step through AUTO and SECS_OPTS."""
    scr = "timing" + sfx
    img, d = panel_background("Slideshow", "clock")
    d.text((160 * SS, 44 * SS), "Show each page for", font=f(VERDB, 10), fill=T["text"], anchor="mm")
    bx0, by0, bx1, by1 = 92, 66, 228, 158
    d.rounded_rectangle([bx0 * SS, by0 * SS, bx1 * SS, by1 * SS], radius=12 * SS, fill=T["box"], outline=T["frame1"], width=SS)
    for kind, cx in (("minus", 46), ("plus", 274)):
        glossy_button(d, cx * SS, 112 * SS, SET_R * SS)
        icon(d, kind, cx * SS, 112 * SS, SET_R * SS * 1.1)
    d.text((160 * SS, 180 * SS), "Tap STAY in Settings to stop on one page", font=f(VERD, 7), fill=T["hint"], anchor="mm")
    done_pill(d)
    big_out = big(small(img)).convert("RGBA")
    for label, cx in (("SHORTER", 46), ("LONGER", 274)):
        arched_label(big_out, cx * SS, 112 * SS, SET_R * SS, label, 8)
    bg, lines = screen_lines(out, rgb565, big_out.convert("RGB"), scr)
    vx, vy, vw, vh = bx0 + 4, by0 + 4, bx1 - bx0 - 8, by1 - by0 - 8
    crop = bg.crop((vx, vy, vx + vw, vy + vh))
    frs = []
    for k in range(len(SECS_OPTS) + 1):
        im = Image.new("RGBA", (vw * SS, vh * SS), (0, 0, 0, 0))
        dd = ImageDraw.Draw(im)
        if k == 0:
            dd.text((vw * SS / 2, 34 * SS), "AUTO", font=f(ROUND, 26), fill=T["text"], anchor="mm")
            dd.text((vw * SS / 2, 64 * SS), "each page's own time", font=f(VERD, 7), fill=T["sub"], anchor="mm")
        else:
            dd.text((vw * SS / 2, 34 * SS), str(SECS_OPTS[k - 1]), font=f(ROUND, 38), fill=T["text"], anchor="mm")
            dd.text((vw * SS / 2, 66 * SS), "seconds", font=f(VERDB, 9), fill=T["sub"], anchor="mm")
        for i in range(len(SECS_OPTS) + 1):                         # position dots
            x = (vw / 2 + (i - len(SECS_OPTS) / 2) * 9) * SS
            dd.ellipse([x - 2 * SS, 78 * SS, x + 2 * SS, 82 * SS], fill=T["text"] if i == k else T["frame2"])
        frs.append(on_bg(crop, im))
    lines += sheet_pane(out, rgb565, frs, f"{scr}_value", scr, vx, vy, "secs")
    lines += [f"hot {scr} 14 74 66 72 secs_down", f"hot {scr} 240 74 66 72 secs_up",
              f"hot {scr} {DONE_BOX[0] - 6} {DONE_BOX[1] - 6} {DONE_BOX[2] - DONE_BOX[0] + 12} 40 screen settings"]
    return lines


def signal_bars(d, x, y, level, col, off):
    """Four bars, bottom-aligned at y, level 0-4."""
    for i in range(4):
        h = 3 + i * 2
        d.rectangle([(x + i * 4) * SS, (y - h) * SS, (x + i * 4 + 2) * SS, y * SS], fill=col if i < level else off)


def network_rows(info):
    q = info.get("quality", "")
    sig = info.get("signal", "")
    sig_txt = ", ".join(v for v in (f"{sig} dBm" if sig else "", f"{q}%" if q else "") if v) or "?"
    return [("Network", info.get("ssid", "?")), ("Signal", sig_txt), ("Access point", info.get("ap", "?")),
            ("IP address", info.get("ip", "?")), ("Gateway", info.get("gateway", "?")),
            ("DNS", info.get("dns", "?")), ("MAC address", info.get("mac", "?")), ("Link rate", info.get("rate", "?"))]


def about_rows(info):
    return [("Name", info.get("hostname", "?")), ("Firmware", info.get("firmware", "?")),
            ("Kernel", info.get("kernel", "?")), ("Uptime", info.get("uptime", "?")),
            ("Memory", info.get("memory", "?")), ("USB stick", info.get("usb", "?")),
            ("wxshow", info.get("wxshow", "?")), ("Feed", info.get("feed", "?"))]


def info_screen(out, rgb565, scr, title, kind, rows, info):
    """A read-only page of label / value rows, rendered on the server (the Chumby can't draw text)."""
    img, d = panel_background(title, kind)
    x0, y0, x1, y1 = 10, 32, 310, 198
    d.rounded_rectangle([x0 * SS, y0 * SS, x1 * SS, y1 * SS], radius=10 * SS, fill=T["box"], outline=T["frame1"], width=SS)
    rh = (y1 - y0 - 8) / len(rows)
    lf, vf = f(VERD, 8), f(VERDB, 9)
    for i, (label, value) in enumerate(rows):
        y = (y0 + 4 + rh * i + rh / 2) * SS
        if i:
            d.line([((x0 + 8) * SS, (y0 + 4 + rh * i) * SS), ((x1 - 8) * SS, (y0 + 4 + rh * i) * SS)], fill=T["frame2"], width=SS)
        d.text(((x0 + 10) * SS, y), label, font=lf, fill=T["sub"], anchor="lm")
        vx = x0 + 100
        if label == "Signal" and info.get("quality", "").isdigit():
            signal_bars(d, vx, y / SS + 5, min(4, (int(info["quality"]) + 12) // 25), (60, 160, 70), T["frame2"])
            vx += 22
        while vf.getlength(value) > (x1 - 10 - vx) * SS and len(value) > 4:
            value = value[:-2] + "…"
        d.text((vx * SS, y), value, font=vf, fill=T["text"], anchor="lm")
    d.text((14 * SS, 218 * SS), info.get("asof", "Not read from the Chumby yet"), font=f(VERD, 7), fill=T["hint"], anchor="lm")
    done_pill(d)
    _, lines = screen_lines(out, rgb565, img, scr)
    lines.append(f"hot {scr} {DONE_BOX[0] - 6} {DONE_BOX[1] - 6} {DONE_BOX[2] - DONE_BOX[0] + 12} 40 screen settings")
    return lines
