#!/usr/bin/env python3
"""wxfeed: render a Chumby's 90s-weather-channel pages and screens for its wx engine.

The Chumby (ARM926, no FPU) only blits pixels, so everything that is hard
(fetching HTTPS, parsing, fonts, anti-aliasing, layout) happens here, on the server.
Output, all 16-bit RGB565 little-endian, into OUTDIR:
  weather_*.raw   320x240 page backgrounds (clock area left blank), and their _night copies
  *_icon<N>.raw   animated icon sprite sheets (frames stacked vertically, 0xF81F = transparent)
  clock.raw       glyph cells for "0123456789: APM", drawn on the header background
  ticker.raw      a long 24px-high strip that the device scrolls
  hashes.txt      md5sum-format hashes of every file the manifest names
  manifest.txt    what to draw where (one directive per line), written last

Usage: wxfeed.py OUTDIR [--config FILE] [--info FILE] [--no-saver]   (needs Pillow)

Nothing here talks to a Chumby: neochumbyd delivers OUTDIR to it (over NFS, or by
sync), and the app's companion (neochumby-wx-feed) runs this every few minutes.
Every file is written only if its bytes changed, and atomically, so a reader never sees
half a file; a render that changes nothing writes nothing, manifest included.
--info names a key=value file of facts about the Chumby (ssid, ip, firmware, ...) for
Settings -> Network and Chumby Info; without it they show "?".

What plays, in what order, comes from the config file (config.py; the web page in
configweb.py writes it): apps x their enabled sub-slides, cut to the engine's page limit.
"""
import datetime as dt
import hashlib
import math
import os
import struct
import sys
import urllib.request
import xml.etree.ElementTree as ET
from zoneinfo import ZoneInfo

from PIL import Image, ImageDraw, ImageFont

import aurora
import config
import news as abcnews                                  # the ABC News app (main() has its own `news`)
import photos
import qrscreen
import screens
import stocks
import wxassets

W, H = 320, 240
TZ = ZoneInfo(config.DEFAULT_TZ)                        # the general "timezone" setting (set_timezone)
UA = config.USER_AGENT
# The weather app's location: its options in the config (config.WEATHER_SCHEMA), applied by
# set_location() before anything is drawn. BOM's website blocks scrapers and points to its
# anonymous FTP, so observations and forecasts come from there, by product id.
BOM_FTP = "ftp://ftp.bom.gov.au/anon/gen/fwo/{}.xml"
LOC = {}
# The ticker's headlines (examples: ABC Adelaide and ABC Just In)
NEWS_URLS = ["https://www.abc.net.au/news/feed/8057540/rss.xml",
             "https://www.abc.net.au/news/feed/51120/rss.xml"]
KEY = (255, 0, 255)                                                     # sprite colour key (0xF81F)

# Bundled open fonts, metric-compatible with the Arial the pages were designed in, so every
# server renders the same pixels (see assets/fonts/README.md)
FONT_DIR = __file__.rsplit("/", 1)[0] + "/assets/fonts/"
BOLD = FONT_DIR + "LiberationSans-Bold.ttf"              # Arial Bold
NARROW = FONT_DIR + "LiberationSansNarrow-Bold.ttf"      # Arial Narrow Bold

# Palette: WeatherStar-4000-inspired, not copied. Two themes: the day one, and a darker
# navy one with dimmer accents for night mode (the pages' pagenight files, the clock and
# ticker's clocknight / tickernight), which keeps the 90s look without glaring in the dark.
THEMES = {
    "day": dict(SKY_TOP=(28, 10, 87), SKY_BOT=(64, 44, 152),      # page background gradient
                HDR_TOP=(16, 18, 64), HDR_BOT=(40, 58, 140),      # header band
                PANEL=(34, 52, 150), ORANGE=(230, 120, 20), YELLOW=(255, 230, 60), WHITE=(240, 240, 240),
                TICK_TOP=(10, 30, 90), TICK_BOT=(20, 50, 130),    # ticker band
                FAINT=(170, 180, 230), RAIN=(140, 200, 255), LOGO_DIM=1.0),
    "night": dict(SKY_TOP=(5, 4, 24), SKY_BOT=(12, 12, 48),
                  HDR_TOP=(3, 4, 18), HDR_BOT=(10, 16, 50),
                  PANEL=(12, 20, 64), ORANGE=(140, 72, 14), YELLOW=(196, 170, 64), WHITE=(178, 180, 194),
                  TICK_TOP=(3, 8, 30), TICK_BOT=(7, 16, 48),
                  FAINT=(100, 108, 150), RAIN=(84, 128, 176), LOGO_DIM=0.7),
}
SHADOW = (0, 0, 0)
THEME = "day"


def set_theme(name):
    """Rebind the palette globals the drawing code reads (day or night)."""
    global THEME
    THEME = name
    globals().update(THEMES[name])


# the palette names set_theme() binds, declared so the code reading them is checkable
SKY_TOP = SKY_BOT = HDR_TOP = HDR_BOT = PANEL = ORANGE = YELLOW = WHITE = TICK_TOP = TICK_BOT = None
FAINT = RAIN = LOGO_DIM = None
set_theme("day")

HEADER_H = 40
TICKER_Y, TICKER_H = 216, 24
_logo = {}


def set_location(opts):
    """The weather app's options (config.WEATHER_SCHEMA) -> LOC, what the pages read."""
    o = dict(config.defaults(config.WEATHER_SCHEMA), **(opts or {}))
    LOC.update(name=o["location"], obs_url=BOM_FTP.format(o["obs_product"]), wmo=o["obs_station"],
               fcst_url=BOM_FTP.format(o["forecast_product"]), aac=o["forecast_area"],
               lat=float(o["latitude"]), lon=float(o["longitude"]))


set_location(None)


def set_timezone(name):
    """The general timezone setting, for every module that prints a time."""
    global TZ
    try:
        tz = ZoneInfo(name)
    except Exception:
        print(f"wxfeed: unknown timezone {name!r}, using {config.DEFAULT_TZ}", file=sys.stderr)
        tz = ZoneInfo(config.DEFAULT_TZ)
    TZ = abcnews.TZ = stocks.HOME_TZ = tz


def badge():
    """The weather pages' badge when no weather-badge.png is supplied: a sun behind a
    cloud on a navy tile with an orange rim, drawn at 4x and shrunk."""
    k, w, h = 4, 120, 96
    im = Image.new("RGBA", (w * k, h * k), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    d.rounded_rectangle([2 * k, 2 * k, (w - 2) * k, (h - 2) * k], radius=16 * k, fill=(30, 40, 120), outline=(230, 120, 20),
                        width=5 * k)
    draw_sun(d, 48 * k, 40 * k, 52 * k, 0.3)
    draw_cloud(d, 70 * k, 58 * k, 56 * k)
    return im.resize((w, h), Image.LANCZOS)


def header_logo(h=34):
    """The badge, dimmed in the night theme (its white and yellow would glare)."""
    if THEME not in _logo:
        path = wxassets.find("weather-badge.png")
        src = Image.open(path).convert("RGBA") if path else badge()
        im = src.resize((round(src.width * h / src.height), h), Image.LANCZOS)
        if LOGO_DIM != 1.0:
            r, g, b, a = im.split()
            r, g, b = (c.point(lambda v: int(v * LOGO_DIM)) for c in (r, g, b))
            im = Image.merge("RGBA", (r, g, b, a))
        _logo[THEME] = im
    return _logo[THEME]
CLOCK_R, CLOCK_Y = W - 6, 22   # the device right-aligns the time to CLOCK_R


def font(size, path=BOLD):
    return ImageFont.truetype(path, size)


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read()


# ---------------------------------------------------------------- data
def get_obs():
    """The location's latest observation, mapped to the keys the pages use."""
    root = ET.fromstring(fetch(LOC["obs_url"]))
    st = next(s for s in root.iter("station") if s.get("wmo-id") == LOC["wmo"])
    per = next(st.iter("period"))
    e = {x.get("type"): x.text for x in per.iter("element")}
    num = lambda k: float(e[k]) if e.get(k) not in (None, "") else None
    return {"air_temp": num("air_temperature"), "apparent_t": e.get("apparent_temp", "--"),
            "rel_hum": e.get("rel-humidity", "--"), "wind_dir": e.get("wind_dir", ""),
            "wind_spd_kmh": e.get("wind_spd_kmh", "--"), "gust_kmh": e.get("gust_kmh", "--"),
            "press": e.get("msl_pres", "--"), "rain_trace": e.get("rainfall", "--"),
            "weather": e.get("weather"), "cloud": e.get("cloud"),
            "max": e.get("maximum_air_temperature"), "min": e.get("minimum_air_temperature"),
            "time": per.get("time-local", "")}


def get_forecast():
    root = ET.fromstring(fetch(LOC["fcst_url"]))
    for a in root.iter("area"):
        if a.get("aac") == LOC["aac"]:
            days = []
            for p in a.findall("forecast-period"):
                v = {e.get("type"): (e.text or "") for e in p}
                v["date"] = dt.date.fromisoformat(p.get("start-time-local")[:10])
                days.append(v)
            return days
    return []


def get_news(limit=8):
    seen, out = set(), []
    for url in NEWS_URLS:
        try:
            ch = ET.fromstring(fetch(url)).find("channel")
        except Exception as e:  # one dead feed must not kill the page
            print("news feed failed:", url, e, file=sys.stderr)
            continue
        for it in ch.findall("item"):
            t = (it.findtext("title") or "").strip()
            if t and t not in seen:
                seen.add(t)
                out.append(t)
    return out[:limit]


# ---------------------------------------------------------------- sunrise / sunset
def sun_events(day, lat=None, lon=None):
    """(sunrise, sunset) as UTC datetimes for a local date at the location (or lat, lon):
    NOAA's solar equations, zenith 90.833 deg (refraction + the sun's radius). Good to a
    minute or so."""
    lat = LOC["lat"] if lat is None else lat
    lon = LOC["lon"] if lon is None else lon
    n = day.timetuple().tm_yday
    g = 2 * math.pi / 365 * (n - 1)
    eqt = 229.18 * (0.000075 + 0.001868 * math.cos(g) - 0.032077 * math.sin(g)
                    - 0.014615 * math.cos(2 * g) - 0.040849 * math.sin(2 * g))
    decl = (0.006918 - 0.399912 * math.cos(g) + 0.070257 * math.sin(g) - 0.006758 * math.cos(2 * g)
            + 0.000907 * math.sin(2 * g) - 0.002697 * math.cos(3 * g) + 0.00148 * math.sin(3 * g))
    la = math.radians(lat)
    ha = math.degrees(math.acos(math.cos(math.radians(90.833)) / (math.cos(la) * math.cos(decl))
                                - math.tan(la) * math.tan(decl)))
    midnight = dt.datetime(day.year, day.month, day.day, tzinfo=dt.timezone.utc)
    rise = midnight + dt.timedelta(minutes=720 - 4 * (lon + ha) - eqt)
    sets = midnight + dt.timedelta(minutes=720 - 4 * (lon - ha) - eqt)
    return rise, sets


def sun_lines(now, days=8):
    """'sun RISE SET' (epoch seconds) for the next week, so night mode keeps working
    on an old copy even if the renderer stops."""
    out = []
    for i in range(days):
        rise, sets = sun_events(now.date() + dt.timedelta(days=i))
        out.append(f"sun {int(rise.timestamp())} {int(sets.timestamp())}")
    return out


# ---------------------------------------------------------------- drawing helpers
def vgrad(img, box, top, bot):
    x0, y0, x1, y1 = box
    d = ImageDraw.Draw(img)
    for y in range(y0, y1):
        t = (y - y0) / max(1, y1 - y0 - 1)
        d.line([(x0, y), (x1 - 1, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(top, bot)))


def stext(d, xy, text, f, fill=None, anchor="la", shadow=2):
    """Text with the hard black drop shadow that gives the 90s look."""
    fill = fill or WHITE                                 # the current theme's, not the day one's
    x, y = xy
    d.text((x + shadow, y + shadow), text, font=f, fill=SHADOW, anchor=anchor)
    d.text((x, y), text, font=f, fill=fill, anchor=anchor)


def wrap(text, f, width):
    words, lines, cur = text.split(), [], ""
    for w in words:
        t = (cur + " " + w).strip()
        if f.getlength(t) <= width:
            cur = t
        else:
            lines.append(cur)
            cur = w
    if cur:
        lines.append(cur)
    return lines


def base_page(title1, title2, now):
    img = Image.new("RGB", (W, H))
    vgrad(img, (0, 0, W, H), SKY_TOP, SKY_BOT)
    vgrad(img, (0, 0, W, HEADER_H), HDR_TOP, HDR_BOT)
    d = ImageDraw.Draw(img)
    d.line([(0, HEADER_H), (W, HEADER_H)], fill=ORANGE, width=2)
    # logo box (top-left): tapping it returns to the Chumby UI
    logo = header_logo()
    img.paste(logo, (4, 3), logo)
    tx = 4 + logo.width + 8
    stext(d, (tx, 5), title1, font(15), YELLOW)
    stext(d, (tx, 21), title2, font(15), YELLOW)
    stext(d, (W - 6, 4), now.strftime("%a %b %-d").upper(), font(12), WHITE, anchor="ra", shadow=1)
    # ticker band
    d.rectangle([0, TICKER_Y - 2, W, TICKER_Y - 1], fill=ORANGE)
    vgrad(img, (0, TICKER_Y, W, H), TICK_TOP, TICK_BOT)
    return img


def panel(img, box):
    d = ImageDraw.Draw(img)
    x0, y0, x1, y1 = box
    d.rectangle(box, fill=PANEL)
    d.rectangle([x0, y0, x1, y1], outline=ORANGE, width=2)


# ---------------------------------------------------------------- animated icons
def icon_kind(code):
    code = int(code or 3)
    return {1: "sun", 2: "moon", 3: "partly", 4: "cloud", 6: "fog", 8: "rain", 9: "wind",
            10: "fog", 11: "rain", 12: "rain", 13: "fog", 16: "storm", 17: "rain", 18: "rain"}.get(code, "partly")


def draw_cloud(d, cx, cy, s, fill=(235, 235, 245), edge=(150, 150, 175)):
    for dx, dy, r in ((-0.35, 0.05, 0.28), (0.0, -0.12, 0.36), (0.35, 0.05, 0.26), (0.0, 0.12, 0.3)):
        x, y, rr = cx + dx * s, cy + dy * s, r * s
        d.ellipse([x - rr - 1, y - rr - 1, x + rr + 1, y + rr + 1], fill=edge)
    for dx, dy, r in ((-0.35, 0.05, 0.28), (0.0, -0.12, 0.36), (0.35, 0.05, 0.26), (0.0, 0.12, 0.3)):
        x, y, rr = cx + dx * s, cy + dy * s, r * s
        d.ellipse([x - rr, y - rr, x + rr, y + rr], fill=fill)


def draw_sun(d, cx, cy, s, phase):
    for i in range(12):
        a = phase + i * math.pi / 6
        r0, r1 = s * 0.42, s * (0.62 if i % 2 else 0.54)
        d.line([(cx + r0 * math.cos(a), cy + r0 * math.sin(a)),
                (cx + r1 * math.cos(a), cy + r1 * math.sin(a))], fill=(255, 190, 30), width=max(2, int(s / 16)))
    r = s * 0.34
    d.ellipse([cx - r - 1, cy - r - 1, cx + r + 1, cy + r + 1], fill=(230, 120, 10))
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(255, 215, 40))


def icon_frames(kind, size, n=8):
    """n frames, each size x size, keyed background. Supersampled 3x for smooth edges."""
    ss = 3
    frames = []
    for f in range(n):
        t = f / n
        im = Image.new("RGBA", (size * ss, size * ss), (0, 0, 0, 0))
        d = ImageDraw.Draw(im)
        S, c = size * ss, size * ss / 2
        if kind == "sun":
            draw_sun(d, c, c, S * 0.76, t * math.pi / 6 * 2)       # long rays reach 0.62 x size: keep inside the sprite
        elif kind == "moon":
            d.ellipse([c - S * .3, c - S * .3, c + S * .3, c + S * .3], fill=(240, 240, 200))
            d.ellipse([c - S * .15, c - S * .38, c + S * .38, c + S * .2], fill=(0, 0, 0, 0))
        elif kind == "partly":
            draw_sun(d, c - S * .1, c - S * .1, S * .6, t * math.pi / 3)
            draw_cloud(d, c + S * .1 + math.sin(t * 2 * math.pi) * S * .03, c + S * .16, S * .62)
        elif kind == "cloud":
            draw_cloud(d, c - S * .12 + math.sin(t * 2 * math.pi) * S * .04, c - S * .08, S * .55, (200, 200, 215))
            draw_cloud(d, c + S * .1 - math.sin(t * 2 * math.pi) * S * .04, c + S * .12, S * .65)
        elif kind in ("rain", "storm"):
            draw_cloud(d, c, c - S * .14, S * .75, (205, 205, 220))
            for i in range(5):
                x = c - S * .3 + i * S * .15
                y = c + S * .1 + ((t + i * .37) % 1) * S * .32
                d.line([(x, y), (x - S * .04, y + S * .1)], fill=(110, 190, 255), width=ss * 2)
            if kind == "storm" and f % 4 < 2:
                d.polygon([(c, c), (c - S * .1, c + S * .25), (c, c + S * .22), (c - S * .06, c + S * .45),
                           (c + S * .14, c + S * .14), (c + S * .03, c + S * .17)], fill=(255, 240, 60))
        elif kind == "fog":
            for i in range(4):
                y = c - S * .25 + i * S * .17
                off = math.sin(t * 2 * math.pi + i) * S * .06
                d.rounded_rectangle([c - S * .38 + off, y, c + S * .38 + off, y + S * .08], radius=S * .04, fill=(210, 210, 225))
        elif kind == "wind":
            for i in range(3):
                y = c - S * .2 + i * S * .2
                off = ((t + i * .3) % 1) * S * .2
                d.line([(c - S * .4 + off, y), (c + S * .25 + off, y)], fill=(230, 230, 240), width=ss * 3)
        # shrink in premultiplied alpha so no background colour bleeds into the edges,
        # then threshold to the device's colour key (it can't blend)
        sm = im.convert("RGBa").resize((size, size), Image.LANCZOS).convert("RGBA")
        im = Image.new("RGB", (size, size), KEY)
        im.paste(sm.convert("RGB"), (0, 0), sm.getchannel("A").point(lambda a: 255 if a >= 110 else 0))
        frames.append(im)
    sheet = Image.new("RGB", (size, size * n))
    for i, fr in enumerate(frames):
        sheet.paste(fr, (0, i * size))
    return sheet


# ---------------------------------------------------------------- pages
def page_current(obs, fc, now):
    img = base_page("Current", "Conditions", now)
    panel(img, (8, 48, W - 9, 208))
    d = ImageDraw.Draw(img)
    temp = obs.get("air_temp")
    stext(d, (72, 58), LOC["name"], font(18), YELLOW, anchor="ma")
    stext(d, (72, 78), f"{temp:.0f}°" if temp is not None else "--", font(44), WHITE, anchor="ma")
    # BOM often reports "-" for both; fall back to today's forecast wording
    cond = next((c for c in (obs.get("weather"), obs.get("cloud")) if c and c != "-"), "")
    if not cond and fc:
        cond = fc[0].get("precis", "").rstrip(".")
    stext(d, (72, 176), cond[:14], font(14), WHITE, anchor="ma")
    rows = [("Feels like", f"{obs.get('apparent_t', '--')}°"),
            ("Humidity", f"{obs.get('rel_hum', '--')}%"),
            ("Wind", f"{obs.get('wind_dir', '')} {obs.get('wind_spd_kmh', '--')} km/h"),
            ("Gusts", f"{obs.get('gust_kmh', '--')} km/h"),
            ("Pressure", f"{obs.get('press', '--')} hPa"),
            ("Rain 9am", f"{obs.get('rain_trace', '--')} mm")]
    for i, (k, v) in enumerate(rows):
        y = 60 + i * 22
        stext(d, (150, y), k, font(13), YELLOW, shadow=1)
        stext(d, (W - 16, y), v, font(13), WHITE, anchor="ra", shadow=1)
    t = obs.get("time", "")
    when = t[11:16] if len(t) >= 16 else ""
    hilo = f"  hi {obs['max']}° lo {obs['min']}°" if obs.get("max") else ""
    d.text((72, 196), f"obs {when}{hilo}", font=font(9), fill=FAINT, anchor="ma")
    code = fc[0].get("forecast_icon_code") if fc else (1 if cond.lower() in ("clear", "sunny") else 3)
    return img, [(icon_kind(code), 44, 50, 128)]


def page_local(fc, now):
    img = base_page("Local", "Forecast", now)
    d = ImageDraw.Draw(img)
    icons = []
    for i, day in enumerate(fc[:3]):
        x0 = 8 + i * 102
        panel(img, (x0, 48, x0 + 98, 208))
        name = "TODAY" if day["date"] == now.date() else day["date"].strftime("%A").upper()
        stext(d, (x0 + 49, 53), name[:9], font(13), YELLOW, anchor="ma", shadow=1)
        icons.append((icon_kind(day.get("forecast_icon_code")), 48, x0 + 25, 70))
        hi, lo = day.get("air_temperature_maximum", "--"), day.get("air_temperature_minimum", "")
        stext(d, (x0 + 49, 122), f"{hi}°" + (f" / {lo}°" if lo else ""), font(16), WHITE, anchor="ma")
        for j, line in enumerate(wrap(day.get("precis", ""), font(11), 88)[:3]):
            stext(d, (x0 + 49, 146 + j * 14), line, font(11), WHITE, anchor="ma", shadow=1)
        pop = day.get("probability_of_precipitation", "")
        if pop:
            d.text((x0 + 49, 196), f"rain {pop}", font=font(10), fill=RAIN, anchor="ma")
    return img, icons


def page_extended(fc, now):
    img = base_page("Extended", "Forecast", now)
    panel(img, (8, 48, W - 9, 208))
    d = ImageDraw.Draw(img)
    for i, day in enumerate(fc[1:7]):
        y = 54 + i * 25
        stext(d, (18, y + 3), day["date"].strftime("%a").upper(), font(14), YELLOW, shadow=1)
        small = icon_frames(icon_kind(day.get("forecast_icon_code")), 22, 1)
        img.paste(small, (58, y), _keymask(small))
        stext(d, (88, y + 4), day.get("precis", "")[:24], font(12, NARROW), WHITE, shadow=1)
        hi, lo = day.get("air_temperature_maximum", "--"), day.get("air_temperature_minimum", "--")
        stext(d, (W - 18, y + 3), f"{lo}° / {hi}°", font(14), WHITE, anchor="ra", shadow=1)
    return img, []


def _keymask(im):
    m = Image.new("L", im.size, 255)
    pm, pi = m.load(), im.load()
    for y in range(im.size[1]):
        for x in range(im.size[0]):
            if pi[x, y] == KEY:
                pm[x, y] = 0
    return m


# ---------------------------------------------------------------- clock + ticker
def clock_cells(now):
    """Glyphs drawn on the exact header rows they will sit on, so no keying is needed.
    Each glyph is left-aligned in its cell; widths go in the manifest so the
    device can lay the time out proportionally and right-align it."""
    glyphs = "0123456789: APM"
    f = font(14)
    widths = [int(round(f.getlength(g))) + 1 for g in glyphs]
    cw, ch = max(widths) + 1, 16
    base = base_page("", "", now).crop((W - cw, CLOCK_Y, W, CLOCK_Y + ch))   # header rows are uniform across x
    sheet = Image.new("RGB", (cw, ch * len(glyphs)))
    for i, g in enumerate(glyphs):
        cell = base.copy()
        stext(ImageDraw.Draw(cell), (0, 0), g, f, WHITE, shadow=1)
        sheet.paste(cell, (0, i * ch))
    return sheet, cw, ch, widths


def ticker_strip(obs, fc, news):
    f = font(14)
    parts = [f"{LOC['name'].upper()} NOW {obs.get('air_temp', '--')}°C  {obs.get('wind_dir', '')} {obs.get('wind_spd_kmh', '--')} km/h"]
    if fc:
        parts.append(f"TODAY: {fc[0].get('precis', '')} Max {fc[0].get('air_temperature_maximum', '--')}°")
    parts += [h.upper() for h in news]
    text = "     •     ".join(parts) + "     •     "
    wpx = int(f.getlength(text)) + W
    img = Image.new("RGB", (wpx, TICKER_H))
    vgrad(img, (0, 0, wpx, TICKER_H), TICK_TOP, TICK_BOT)
    stext(ImageDraw.Draw(img), (W, 4), text, f, WHITE, shadow=1)
    return img


# ---------------------------------------------------------------- output
def rgb565(img, path):
    data = img.convert("RGB").tobytes()
    out = bytearray(len(data) // 3 * 2)
    j = 0
    for i in range(0, len(data), 3):
        r, g, b = data[i], data[i + 1], data[i + 2]
        struct.pack_into("<H", out, j, ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3))
        j += 2
    write_if_changed(path, out)


def write_if_changed(path, data):
    """Leave an identical file alone (mtime and all), so delivery skips it. Otherwise write
    a temp file beside it and rename it into place: a reader (the engine over NFS, or sync)
    never sees half a file."""
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


# the Weather app's sub-slides (config.BUILTIN "weather"): id -> Home title, renderer
WEATHER_SLIDES = {
    "current": ("Current Conditions", lambda obs, fc, now: page_current(obs, fc, now)),
    "local": ("Local Forecast", lambda obs, fc, now: page_local(fc, now)),
    "extended": ("Extended Forecast", lambda obs, fc, now: page_extended(fc, now)),
}


def arg(name):
    """The value after --name on the command line, or None."""
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv[:-1] else None


def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    cfg = config.load(arg("--config"))
    set_timezone(cfg["general"]["timezone"])
    set_location(config.app_options(cfg, "weather"))
    now = dt.datetime.now(TZ)
    slides, dropped = config.plan(cfg)                     # apps x enabled sub-slides, cut to MAXP pages
    if "--no-saver" in sys.argv:                           # for wxshow builds without the demo effects
        slides = [s for s in slides if s["app"] != "aurora"] or slides
    for s in dropped:
        print(f"over wxshow's {config.MAXP} pages: dropped {s['app']} {s['subslide'] or ''} ({s['pages']} page(s))")
    obs, fc, news = get_obs(), get_forecast(), get_news()   # the ticker needs them whatever is enabled

    man = sun_lines(now)
    titles, thumbs, index = [], [], {}                     # Home's card: one title + thumbnail per page
    icon_n = i = 0
    page_app = []                                          # the app each slideshow page belongs to
    while i < len(slides):
        s, p = slides[i], len(titles)
        if s["app"] == "weather":
            title, draw = WEATHER_SLIDES[s["subslide"]]
            img, icons = draw(obs, fc, now)
            name = f"weather_{s['subslide']}"              # stable per sub-slide, whatever the play order
            rgb565(img, f"{out}/{name}.raw")
            img.save(f"{out}/{name}.png")
            man.append(f"page {p} {name}.raw {s['seconds']}")
            set_theme("night")                             # the dark copy wxshow draws in night mode
            nimg, _ = draw(obs, fc, now)
            set_theme("day")
            rgb565(nimg, f"{out}/{name}_night.raw")
            nimg.save(f"{out}/{name}_night.png")
            man.append(f"pagenight {p} {name}_night.raw")
            for k, (kind, size, x, y) in enumerate(icons):
                frames = 8
                rgb565(icon_frames(kind, size, frames), f"{out}/{name}_icon{k}.raw")
                man.append(f"sprite {p} {name}_icon{k}.raw {size} {size} {frames} {x} {y} 8")
                icon_n += 1
            titles.append(title)
            thumbs.append(img)
            index.setdefault("weather", []).append(f"{name}.raw")
            i += 1
        elif s["app"] in ("stocks", "photos", "news"):            # module apps: one render call for all their slides
            group = [s]
            while i + len(group) < len(slides) and slides[i + len(group)]["app"] == s["app"]:
                group.append(slides[i + len(group)])
            subs = [(g["subslide"], g["pages"]) for g in group]
            if s["app"] == "stocks":
                r = stocks.render(out, p, rgb565, s["options"], subs, s["seconds"], now=now, night_files=True)
            elif s["app"] == "news":                       # one slide per source; tapping opens its reader
                r = abcnews.render_slides(out, p, rgb565, s["options"], subs, s["seconds"], now=now,
                                          night_files=True)
            else:
                r = photos.render(out, p, rgb565, s["options"], subs, s["seconds"], now=now,
                                  config_url=cfg["general"]["config_url"])
            man += r.lines
            titles += r.titles
            thumbs += r.thumbs
            index[s["app"]] = [ln.split()[2] for ln in r.lines if ln.startswith("page ")]
            i += len(group)
        elif s["app"] == "aurora":
            man += aurora.render(out, p, rgb565, s["seconds"], s.get("options"))
            titles.append("Aurora Screensaver")
            thumbs.append(aurora.thumbnail())
            index["aurora"] = ["aurora_bg.raw"]
            i += 1
        else:                                              # an app this feeder can't draw yet
            i += 1
        page_app += [s["app"]] * (len(titles) - p)
    # Home browses apps, not pages: number the apps in play order and tell wxshow each page's app
    apps = []                                              # (name, first page)
    for pg, a in enumerate(page_app):
        if not apps or page_app[pg - 1] != a:
            name = f"{LOC['name']} Weather" if a == "weather" else (config.app_info(a) or {}).get("name", titles[pg])
            apps.append((name, pg))
        man.append(f"pageapp {pg} {len(apps) - 1}")
    if "stocks" not in index and any(e["app"] == "stocks" and e["enabled"] for e in config.channel(cfg)["apps"]):
        # no stock page in the slideshow (sub-slides off, or cut at MAXP): still make its screens
        man += stocks.render(out, len(titles), rgb565, config.app_options(cfg, "stocks"), [], now=now).lines
    config.write_render_index(out, index)
    # Home (top button), Settings and the Music app
    app_titles, app_thumbs = [n for n, _ in apps], [thumbs[pg] for _, pg in apps]
    man += screens.home(out, rgb565, app_titles, app_thumbs)
    man += screens.home(out, rgb565, app_titles, app_thumbs, night=True)    # dark theme, shown in night mode
    info = chumby_info(arg("--info"), obs)
    man += screens.settings(out, rgb565, info)
    man += screens.settings(out, rgb565, info, night=True)
    url = cfg["general"]["config_url"]                     # Settings -> APPS: the QR code for configweb.py
    man += qrscreen.configure(out, rgb565, url)
    man += qrscreen.configure(out, rgb565, url, night=True)
    screens.T = screens.LIGHT
    man += screens.music(out, rgb565, screens.get_stations())
    clk, cw, ch, widths = clock_cells(now)
    rgb565(clk, f"{out}/clock.raw")
    man.append(f"clock clock.raw {cw} {ch} {CLOCK_R} {CLOCK_Y} " + " ".join(map(str, widths)))
    tk = ticker_strip(obs, fc, news)
    rgb565(tk, f"{out}/ticker.raw")
    man.append(f"ticker ticker.raw {tk.size[0]} {TICKER_H} {TICKER_Y} 2")
    # the same glyphs and strip on the night header and ticker band (same sizes: the engine
    # checks), used on the pages drawn from their pagenight
    set_theme("night")
    nclk, ncw, nch, nwidths = clock_cells(now)
    ntk = ticker_strip(obs, fc, news)
    set_theme("day")
    if (ncw, nch, nwidths) == (cw, ch, widths) and ntk.size == tk.size:
        rgb565(nclk, f"{out}/clock_night.raw")
        rgb565(ntk, f"{out}/ticker_night.raw")
        man += ["clocknight clock_night.raw", "tickernight ticker_night.raw"]
    changed = finish(out, man, now)
    print(f"rendered {len(titles)} pages, {icon_n} icons, ticker {tk.size[0]}px into {out}"
          + ("" if changed else " (nothing changed)"))


def chumby_info(path, obs):
    """Facts about the Chumby for Settings -> Network and Chumby Info, from a key=value file
    (ssid, ap, rate, signal, quality, ip, mac, gateway, dns, hostname, firmware, kernel,
    uptime_s, memfree "AVAIL TOTAL" in kB, usb "USED TOTAL" in kB, asof). Nothing writes it yet:
    the Chumby could report these over its session in future. Without it they show "?".
    The Feed row is the weather's observation time, not the render's: a time that changes
    on every render would make the whole screen new every time (the rendering contract)."""
    info = {}
    try:
        info["feed"] = "weather of " + dt.datetime.fromisoformat(obs["time"]).strftime("%-d %b %Y, %-I:%M %p")
    except (KeyError, TypeError, ValueError):
        pass
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            raw = dict(ln.rstrip("\n").split("=", 1) for ln in fh if "=" in ln)
    except (TypeError, OSError) as e:
        if path:
            print(f"chumby info: {e}", file=sys.stderr)
        return info
    raw = {k.strip(): "".join(ch for ch in v if ch.isprintable()).strip()[:80] for k, v in raw.items()}
    info.update({k: v for k, v in raw.items() if v})
    try:
        up = int(raw["uptime_s"])
        d, h, m = up // 86400, up % 86400 // 3600, up % 3600 // 60
        info["uptime"] = (f"{d} d " if d else "") + f"{h} h {m} min"
    except (KeyError, ValueError):
        pass
    try:
        free, total = map(int, raw["memfree"].split())
        info["memory"] = f"{free / 1024:.1f} MB available of {total / 1024:.0f} MB"
    except (KeyError, ValueError):
        pass
    try:
        used, total = map(int, raw["usb"].split())
        info["usb"] = f"{used / 1024:.1f} MB used of {total / 1024:.0f} MB"
    except (KeyError, ValueError):
        pass
    if "kernel" in info:
        info["kernel"] = f"Linux {info['kernel']}"
    if "asof" in raw:
        info["asof"] = f"As the Chumby reported at {raw['asof']}"
    return info


# ---------------------------------------------------------------- the manifest
FILE_OK = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.-")


def referenced_files(out, lines):
    """The files the manifest names (glowseq frames by prefix). Stale renders left in OUT
    are not named, so they are never delivered."""
    names = set()
    for line in lines:
        toks = line.split()
        if len(toks) >= 4 and toks[0] == "glowseq" and toks[3].isdigit():
            toks = toks + [f"{toks[2]}_{k:03d}.l8" for k in range(int(toks[3]))]
        for tok in toks:
            if tok.endswith((".raw", ".path", ".l8")) and os.path.isfile(os.path.join(out, tok)):
                if not set(tok) <= FILE_OK:
                    raise ValueError(f"unsafe file name in manifest: {tok!r}")
                names.add(tok)
    return sorted(names)


def md5_file(path):
    h = hashlib.md5()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def finish(out, lines, now):
    """Write hashes.txt, then manifest.txt, unless nothing changed since the last render.
    The manifest's first lines are a digest of everything it names and a version (the render
    time), so a render that changes nothing leaves the manifest, its version included,
    alone: the engine reloads and delivery sends something only when a picture changed."""
    files = referenced_files(out, lines)
    hashes = "".join(f"{md5_file(os.path.join(out, f))}  {f}\n" for f in files)
    body = "\n".join(lines) + "\n"
    meaning = "\n".join(ln for ln in lines if not ln.startswith("#"))      # comments (fetch counts) don't count
    digest = hashlib.sha256((hashes + meaning).encode()).hexdigest()[:16]
    try:
        with open(os.path.join(out, "manifest.txt")) as fh:
            if fh.readline().strip() == f"# wxfeed {digest}":
                return False
    except OSError:
        pass
    write_if_changed(os.path.join(out, "hashes.txt"), hashes.encode())
    write_if_changed(os.path.join(out, "manifest.txt"),
                     f"# wxfeed {digest}\nversion {int(now.timestamp())}\n{body}".encode())
    return True


if __name__ == "__main__":
    main()
