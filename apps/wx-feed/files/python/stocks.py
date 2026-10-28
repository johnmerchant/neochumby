"""stocks: the Stock Quotes app for wxshow, laid out to read at arm's length on the 3.5" LCD
(nothing under ~12 px, dark navy on white, bold).

Two ways to see it:
  slideshow pages   sub-slides "summary" (every stock in rows) and "details" (a page per
                    stock), cycling passively; each emits 'pagetap PAGE stocks_list', so
                    tapping one opens the screens (on an engine with pagetap)
  screens           ENTRY = "stocks_list" (+ stocks_list1.. for more than LIST_ROWS stocks,
                    paged with up/down), tap a row -> stocks_d<N>: PREV / LIST / NEXT, a
                    1D / 1M chart toggle (stocks_d<N>_m, only up to MONTH_MAX stocks, to
                    keep the screen count down) and DONE (back to the slideshow).
                    Every screen has a _night copy; with night_files, so do the
                    slideshow pages ('pagenight PAGE FILE').
Stable slot names; files are rewritten only when their pixels change. The only time on
a page is the quote's own "as of", so a closed market renders byte-identical pages.

Data: Yahoo Finance's chart endpoint (query1.finance.yahoo.com/v8/finance/chart), the
one free, keyless source that does both US and ASX (BHP.AX) with intraday data and the
exchange's own timezone and trading hours. It is unofficial and undocumented: fine for
one household display, not for redistribution. Quotes are as delayed as Yahoo's free
data (ASX ~20 min, US real-time-ish for Nasdaq/NYSE last sale).

Company logos come from financialmodelingprep.com/image-stock/<SYM>.png (no key; ASX
symbols as-is), falling back to assets.parqet.com, cached as re-encoded PNGs for 30 days
(and "no logo" for 30 days), in stock-logos/ beside the quote cache. Unofficial sources
too: a missing logo just means no tile.

Every fetch runs against a deadline in a thread pool, and the last good quote of each
stock is cached (stocks-cache.json next to the config file), so a dead network shows
the old numbers marked stale, never a failed render.

    lines, titles, thumbs = stocks.render(out, first_page, rgb565, opts, slides, seconds)
    (slides=[] renders just the screens, for when no stock page is in the slideshow)
"""
import concurrent.futures as cf
import datetime as dt
import json
import math
import os
import re
import sys
import time
import urllib.parse
import urllib.request
from collections import namedtuple
from zoneinfo import ZoneInfo

from PIL import Image, ImageDraw, ImageFont

import config
import screens

W, H = 320, 240
SS = 3
FD = os.path.dirname(os.path.abspath(__file__)) + "/assets/fonts/"
VERD, VERDB, ROUND = FD + "DejaVuSans.ttf", FD + "DejaVuSans-Bold.ttf", FD + "SNPro-Bold.otf"
UA = config.USER_AGENT
HOME_TZ = ZoneInfo(config.DEFAULT_TZ)               # wxfeed.set_timezone sets the configured one
TICKER_RE = r"\^?[A-Z0-9][A-Z0-9.\-=]{0,15}"    # TSLA, BHP.AX, BRK-B, ^AXJO, AUDUSD=X
FETCH_DEADLINE = 40                              # seconds for all stocks together

APP = {"id": "stocks", "name": "Stock Quotes", "icon": "stocks.png", "seconds": 12,
       "description": "Price, change and a chart for each stock you pick, US or ASX (Yahoo Finance).",
       "subslides": [{"id": "summary", "label": "Summary of all stocks", "default": True},
                     {"id": "details", "label": "A page per stock", "default": True}]}
CONFIG_SCHEMA = [
    {"key": "tickers", "type": "list", "label": "Stocks", "default": ["BHP.AX", "TSLA", "AAPL", "CBA.AX"],
     "pattern": TICKER_RE, "upper": True, "max_items": 12,
     "help": "Yahoo Finance symbols. ASX stocks end in .AX (BHP.AX); US ones are plain (TSLA)."},
    {"key": "range", "type": "choice", "label": "Chart", "default": "1d",
     "choices": [["1d", "Today"], ["5d", "5 days"], ["1mo", "1 month"], ["6mo", "6 months"], ["1y", "1 year"]]},
    {"key": "max_details", "type": "number", "label": "Most stock pages", "default": 4, "min": 1, "max": 8,
     "integer": True, "help": "The Chumby plays at most 12 pages in all."},
]


def page_count(subslide, opts):
    """Pages one sub-slide expands to (config.plan uses this to fit wxshow's 12)."""
    n = len(opts.get("tickers") or [])
    if subslide == "summary":
        return 1 if n else 0
    return min(n, int(opts.get("max_details", 4)))


INTERVAL = {"1d": "5m", "5d": "30m", "1mo": "1d", "6mo": "1d", "1y": "1d"}
Rendered = namedtuple("Rendered", "lines titles thumbs")

# themes: the Settings panel palette, plus market colours
# High-contrast themes for the real 3.5" LCD, which is washed out with a blue cast: dark navy
# on white / very light panels, bold weights, no light-grey text (2026-10-28 feedback).
LIGHT = dict(bg_top=(250, 251, 255), bg_bot=(234, 240, 252), panel_top=(214, 226, 251), panel_bot=(194, 211, 246),
             sep=(120, 145, 210), text=(12, 28, 92), sub=(30, 52, 128), frame1=(120, 145, 205), frame2=(196, 208, 236),
             card=(255, 255, 255), grid=(208, 218, 240), axis=(30, 52, 128), up=(0, 122, 44), down=(196, 18, 18),
             flat=(70, 82, 125), amber=(180, 92, 0), pill_text=(255, 255, 255), cut=(255, 255, 255))
NIGHT = dict(bg_top=(8, 12, 34), bg_bot=(18, 26, 62), panel_top=(30, 42, 94), panel_bot=(22, 32, 74),
             sep=(70, 92, 165), text=(232, 238, 255), sub=(178, 194, 242), frame1=(80, 102, 175), frame2=(48, 62, 115),
             card=(14, 20, 50), grid=(46, 60, 108), axis=(178, 194, 242), up=(100, 226, 132), down=(255, 112, 106),
             flat=(150, 162, 205), amber=(255, 182, 72), pill_text=(8, 12, 34), cut=(14, 20, 50))


def font(path, size):
    return ImageFont.truetype(path, int(size * SS))


# ---------------------------------------------------------------- data
def _get(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=12) as r:
        return json.loads(r.read(2_000_000))


def _chart(sym, rng):
    q = urllib.parse.urlencode({"range": rng, "interval": INTERVAL[rng], "includePrePost": "false"})
    last = None
    for host in ("query1", "query2"):
        try:
            d = _get(f"https://{host}.finance.yahoo.com/v8/finance/chart/{urllib.parse.quote(sym)}?{q}")
        except urllib.error.HTTPError as e:
            if e.code == 404:                       # Yahoo's answer for an unknown symbol
                raise LookupError("unknown symbol")
            last = e
            continue
        except Exception as e:
            last = e
            continue
        res = (d.get("chart") or {}).get("result")
        if not res:
            err = (d.get("chart") or {}).get("error") or {}
            raise LookupError(str(err.get("description") or "no data"))
        return res[0]
    raise last


def _num(v):
    return float(v) if isinstance(v, (int, float)) and math.isfinite(v) else None


def _series(res):
    ts = res.get("timestamp") or []
    q = ((res.get("indicators") or {}).get("quote") or [{}])[0]
    closes = q.get("close") or []
    return [[int(t), float(c)] for t, c in zip(ts, closes) if isinstance(c, (int, float)) and math.isfinite(c)], q


def fetch_quote(sym, rng="1d", extra=()):
    """One stock -> a plain dict (cacheable). Raises on failure."""
    res = _chart(sym, "1d")
    m = res.get("meta") or {}
    series, q = _series(res)
    opens = [o for o in (q.get("open") or []) if isinstance(o, (int, float))]
    tp = m.get("currentTradingPeriod") or {}
    day = ((m.get("tradingPeriods") or [[None]])[0] or [None])[0] or tp.get("regular") or {}
    out = {
        "symbol": m.get("symbol", sym), "name": m.get("longName") or m.get("shortName") or sym,
        "exchange": m.get("fullExchangeName") or m.get("exchangeName") or "", "currency": m.get("currency") or "",
        "tz": m.get("exchangeTimezoneName") or "UTC", "decimals": int(m.get("priceHint") or 2),
        "price": _num(m.get("regularMarketPrice")),
        "prev_close": _num(m.get("previousClose")) or _num(m.get("chartPreviousClose")),
        "open": float(opens[0]) if opens else None,
        "high": _num(m.get("regularMarketDayHigh")), "low": _num(m.get("regularMarketDayLow")),
        "volume": m.get("regularMarketVolume"), "wk52": [_num(m.get("fiftyTwoWeekLow")), _num(m.get("fiftyTwoWeekHigh"))],
        "market_cap": _num(m.get("marketCap")),     # the chart endpoint doesn't give this today; shown if it ever does
        "time": m.get("regularMarketTime"),
        "periods": {k: [v.get("start"), v.get("end")] for k, v in tp.items() if isinstance(v, dict)},
        "session": [day.get("start"), day.get("end")],
        "range": rng, "series": series, "fetched": int(time.time()),
    }
    if out["price"] is None:
        raise LookupError("no price")
    if rng != "1d":
        out["series"], _ = _series(_chart(sym, rng))
    out["alt"] = {}
    for r in extra:                                 # e.g. the 1M chart of the detail screens' toggle
        try:
            out["alt"][r] = out["series"] if r == rng else _series(_chart(sym, r))[0]
        except Exception as e:
            print(f"stocks: {sym} {r}: {e}", file=sys.stderr)
    return out


def load_cache(path):
    try:
        with open(path) as fh:
            d = json.load(fh)
        return d if isinstance(d, dict) else {}
    except (OSError, ValueError):
        return {}


def save_cache(path, cache):
    try:
        tmp = path + ".tmp"
        with open(tmp, "w") as fh:
            json.dump(cache, fh)
        os.replace(tmp, path)
    except OSError as e:
        print(f"stocks: can't write cache {path}: {e}", file=sys.stderr)


def get_quotes(tickers, rng, cache_path=None, fetch=fetch_quote, deadline=FETCH_DEADLINE, extra=()):
    """{sym: quote} for every ticker: fresh, else the cached one marked stale, else
    {"symbol", "error"}. Never raises."""
    cache = load_cache(cache_path) if cache_path else {}
    out = {}
    pool = cf.ThreadPoolExecutor(max_workers=6)
    futs = {pool.submit(fetch, s, rng, extra): s for s in tickers}
    try:
        for fu in cf.as_completed(futs, timeout=deadline):
            s = futs[fu]
            try:
                out[s] = fu.result()
                cache[s] = out[s]
            except Exception as e:
                print(f"stocks: {s}: {e}", file=sys.stderr)
                out[s] = {"symbol": s, "error": "Unknown symbol" if isinstance(e, LookupError) else "Quote unavailable"}
    except cf.TimeoutError:
        print("stocks: fetch deadline passed", file=sys.stderr)
    pool.shutdown(wait=False, cancel_futures=True)
    result = {}
    for s in tickers:
        q = out.get(s) or {"symbol": s, "error": "Quote unavailable"}
        if "error" in q and s in cache and q["error"] != "Unknown symbol":
            q = dict(cache[s], stale=True)
        result[s] = q
    if cache_path:
        save_cache(cache_path, {s: cache[s] for s in tickers if s in cache})
    return result


# ---------------------------------------------------------------- company logos
LOGO_URLS = ["https://financialmodelingprep.com/image-stock/{sym}.png",        # transparent or on white
             "https://assets.parqet.com/logos/symbol/{sym}?format=png"]       # square tiles (fallback)
LOGO_TTL = 30 * 86400                    # a logo (or "there is none") is fetched at most once a month
LOGO_MAX_BYTES = 1_000_000
LOGO_MAX_PIXELS = 1024 * 1024
LOGO_FORMATS = {"PNG", "JPEG", "WEBP", "GIF"}


def _logo_file(cache_dir, sym):
    return os.path.join(cache_dir, re.sub(r"[^A-Z0-9.=-]", "_", sym))


def _decode_logo(data):
    """Untrusted bytes -> a small RGBA image, or None. Pillow must really decode it."""
    import io
    import warnings
    if len(data) > LOGO_MAX_BYTES:
        return None
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("error", Image.DecompressionBombWarning)
            im = Image.open(io.BytesIO(data))
            if im.format not in LOGO_FORMATS or im.width * im.height > LOGO_MAX_PIXELS or min(im.size) < 16:
                return None
            im.load()
        im = im.convert("RGBA")
        im.thumbnail((128, 128), Image.LANCZOS)
        return im
    except Exception:
        return None


def _fetch_logo(sym):
    """('ok', image) | ('none', None) when every source says 404 | ('error', None) to retry later."""
    missing = 0
    for url in LOGO_URLS:
        try:
            req = urllib.request.Request(url.format(sym=urllib.parse.quote(sym)), headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=10) as r:
                im = _decode_logo(r.read(LOGO_MAX_BYTES + 1))
            if im is not None:
                return "ok", im
            missing += 1                                  # served, but not a usable picture
        except urllib.error.HTTPError as e:
            if e.code in (403, 404, 410):
                missing += 1
        except Exception:
            pass
    return ("none", None) if missing == len(LOGO_URLS) else ("error", None)


def get_logos(tickers, cache_dir, deadline=15):
    """{sym: RGBA image or None}. Logos are cached as re-encoded PNGs (<sym>.png) and misses as
    <sym>.none, both for LOGO_TTL, so each symbol hits the network at most monthly. Never raises."""
    out, todo = {}, []
    try:
        os.makedirs(cache_dir, exist_ok=True)
    except OSError:
        cache_dir = None
    now = time.time()
    for s in tickers:
        base = _logo_file(cache_dir, s) if cache_dir else None
        fresh = False
        for ext in (".png", ".none"):
            try:
                if base and now - os.path.getmtime(base + ext) < LOGO_TTL:
                    fresh = True
            except OSError:
                pass
        out[s] = _load_cached_logo(base)
        if not fresh:
            todo.append(s)
    if todo:
        pool = cf.ThreadPoolExecutor(max_workers=4)
        futs = {pool.submit(_fetch_logo, s): s for s in todo}
        try:
            for fu in cf.as_completed(futs, timeout=deadline):
                s, (status, im) = futs[fu], fu.result()
                base = _logo_file(cache_dir, s) if cache_dir else None
                if status == "ok":
                    out[s] = im
                    if base:
                        im.save(base + ".png.tmp", "PNG")   # our own re-encode: nothing from the source survives
                        os.replace(base + ".png.tmp", base + ".png")
                        _unlink(base + ".none")
                elif status == "none" and base:
                    open(base + ".none", "w").close()
                    _unlink(base + ".png")
                    out[s] = None
        except cf.TimeoutError:
            print("stocks: logo fetch deadline passed", file=sys.stderr)
        except OSError as e:
            print(f"stocks: logo cache: {e}", file=sys.stderr)
        pool.shutdown(wait=False, cancel_futures=True)
    return out


def _load_cached_logo(base):
    if not base or not os.path.exists(base + ".png"):
        return None
    try:
        with open(base + ".png", "rb") as fh:
            return _decode_logo(fh.read())
    except OSError:
        return None


def _unlink(p):
    try:
        os.unlink(p)
    except OSError:
        pass


def logo_tile(d, T, x, y, size):
    """The rounded white tile a logo sits on (drawn supersampled, before the downscale)."""
    d.rounded_rectangle([x * SS, y * SS, (x + size) * SS, (y + size) * SS], radius=4 * SS,
                        fill=(255, 255, 255), outline=T["frame1"], width=SS)


def paste_logo(img, logo, x, y, size, pad=2):
    """Logo fitted into the tile's inside, LANCZOS-downscaled straight to device pixels and
    pasted after the page's downscale, so it stays crisp. Composited onto white first."""
    inner = size - 2 * pad
    lg = logo.copy()
    lg.thumbnail((inner, inner), Image.LANCZOS)
    flat = Image.new("RGBA", lg.size, (255, 255, 255, 255))
    flat.alpha_composite(lg)
    img.paste(flat.convert("RGB"), (x + pad + (inner - lg.width) // 2, y + pad + (inner - lg.height) // 2))


def monogram(d, T, x, y, size, sym):
    """For rows without a logo when other rows have one: the ticker's first letter, so the
    column still lines up and nothing looks missing."""
    d.rounded_rectangle([x * SS, y * SS, (x + size) * SS, (y + size) * SS], radius=5 * SS, fill=T["sub"])
    letter = sym.lstrip("^")[:1]
    d.text(((x + size / 2) * SS, (y + size / 2 + 0.5) * SS), letter, font=font(VERDB, size * 0.55),
           fill=T["card"], anchor="mm")


# ---------------------------------------------------------------- formatting
CUR = {"USD": "$", "AUD": "A$", "EUR": "€", "GBP": "£", "JPY": "¥", "NZD": "NZ$", "CAD": "C$", "HKD": "HK$"}


def fmt_price(v, q, sym=True):
    if v is None:
        return "–"
    dec = q.get("decimals", 2) if abs(v) >= 1 else max(3, q.get("decimals", 2))
    s = f"{v:,.{dec}f}"
    if not sym:
        return s
    c = q.get("currency", "")
    if c == "GBp":
        return s + "p"
    return CUR.get(c, "") + s


def fmt_big(v):
    if v is None:
        return "–"
    for div, suf in ((1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")):
        if abs(v) >= div:
            return f"{v / div:.2f}{suf}" if abs(v) < 10 * div else f"{v / div:.1f}{suf}"
    return f"{v:,.0f}"


def change(q):
    p, pc = q.get("price"), q.get("prev_close")
    if p is None or not pc:
        return None, None
    return p - pc, (p - pc) / pc * 100


def market_state(q, now):
    """OPEN / PRE-MARKET / AFTER HOURS / CLOSED from the trading periods Yahoo sent."""
    t = now.timestamp()
    per = q.get("periods") or {}
    for key, label in (("regular", "OPEN"), ("pre", "PRE-MARKET"), ("post", "AFTER HOURS")):
        s, e = (per.get(key) or [None, None])
        if s and e and s <= t < e:
            return label
    return "CLOSED"


def market_time(q, ts=None):
    """'4:00 PM EDT' in the exchange's own timezone."""
    ts = ts or q.get("time") or q.get("fetched")
    if not ts:
        return "?"
    try:
        z = ZoneInfo(q.get("tz") or "UTC")
    except Exception:
        z = dt.timezone.utc
    d = dt.datetime.fromtimestamp(ts, z)
    return d.strftime("%-I:%M %p ") + d.strftime("%Z")


# ---------------------------------------------------------------- drawing
def _rgba(T, key):
    return T[key]


def tint(c, bg, a):
    return tuple(int(bg[i] + (c[i] - bg[i]) * a) for i in range(3))


def text_fit(d, xy, s, path, size, fill, maxw, anchor="la", min_size=6):
    while size > min_size and font(path, size).getlength(s) > maxw * SS:
        size -= 0.5
    fnt = font(path, size)
    while fnt.getlength(s) > maxw * SS and len(s) > 3:
        s = s[:-2] + "…"
    d.text((xy[0] * SS, xy[1] * SS), s, font=fnt, fill=fill, anchor=anchor)


def state_pill(d, T, x1, cy, label, h=24):
    """The market state, big enough to read across the room."""
    col = {"OPEN": T["up"], "CLOSED": T["flat"]}.get(label, T["amber"])
    fnt = font(VERDB, 12)
    w = fnt.getlength(label) / SS + 26
    x0 = x1 - w
    d.rounded_rectangle([x0 * SS, (cy - h / 2) * SS, x1 * SS, (cy + h / 2) * SS], radius=h / 2 * SS, fill=col)
    d.ellipse([(x0 + 8) * SS, (cy - 3.5) * SS, (x0 + 15) * SS, (cy + 3.5) * SS], fill=T["pill_text"])
    d.text(((x0 + 19) * SS, cy * SS), label, font=fnt, fill=T["pill_text"], anchor="lm")
    return x0


def chart(img, T, box, q, colour, labels=True, sparkline=False):
    """Line chart of q['series'] in box (device px). Intraday spans the whole session, so a
    morning shows a partly drawn line like every stock app; longer ranges plot by index.
    labels: the high and low of the line, in a legible size, in a column right of the box."""
    d = ImageDraw.Draw(img)
    x0, y0, x1, y1 = box
    ser = q.get("series") or []
    intraday = q.get("range", "1d") == "1d"
    pc = q.get("prev_close") if intraday else None
    if len(ser) < 2:
        if labels:
            d.text(((x0 + x1) / 2 * SS, (y0 + y1) / 2 * SS), "No chart yet", font=font(VERDB, 13), fill=T["sub"], anchor="mm")
        return
    vals = [v for _, v in ser]
    lo, hi = min(vals + ([pc] if pc else [])), max(vals + ([pc] if pc else []))
    if hi - lo < 1e-9:
        lo, hi = lo * 0.99, hi * 1.01 + 1e-6
    pad = (hi - lo) * 0.06
    lo_p, hi_p = lo - pad, hi + pad
    s0, s1 = (q.get("session") or [None, None])
    if intraday and s0 and s1 and s1 > s0:
        t0, t1 = min(s0, ser[0][0]), max(s1, ser[-1][0])
        xs = [x0 + (t - t0) / (t1 - t0) * (x1 - x0) for t, _ in ser]
    else:
        xs = [x0 + i / (len(ser) - 1) * (x1 - x0) for i in range(len(ser))]
    Y = lambda v: y1 - (v - lo_p) / (hi_p - lo_p) * (y1 - y0)
    if labels:                                     # two reference lines: the line's own high and low
        fnt = font(VERDB, 12)
        vh, vl = max(vals), min(vals)
        for v in (vh, vl):
            y = Y(v)
            d.line([(x0 * SS, y * SS), (x1 * SS, y * SS)], fill=T["grid"], width=SS)
        yh, yl = Y(vh), Y(vl)
        if yl - yh < 14:                           # flat line: keep the labels apart
            yh, yl = (yh + yl) / 2 - 7, (yh + yl) / 2 + 7
        d.text(((x1 + 8) * SS, yh * SS), fmt_price(vh, q, False), font=fnt, fill=T["axis"], anchor="lm")
        d.text(((x1 + 8) * SS, yl * SS), fmt_price(vl, q, False), font=fnt, fill=T["axis"], anchor="lm")
    pts = [(x * SS, Y(v) * SS) for x, (_, v) in zip(xs, ser)]
    mask = Image.new("L", img.size, 0)
    ImageDraw.Draw(mask).polygon(pts + [(pts[-1][0], y1 * SS), (pts[0][0], y1 * SS)], fill=255)
    fill = Image.new("RGB", img.size)
    screens.grad(ImageDraw.Draw(fill), (int(x0 * SS), int(y0 * SS), int(x1 * SS), int(y1 * SS)),
                 tint(colour, T["card"], 0.30), tint(colour, T["card"], 0.04))
    img.paste(fill, (0, 0), mask)
    if pc:
        y, x = Y(pc) * SS, x0 * SS
        while x < x1 * SS:
            d.line([(x, y), (min(x + 5 * SS, x1 * SS), y)], fill=T["axis"], width=SS)
            x += 9 * SS
    d.line(pts, fill=colour, width=int(SS * (2 if sparkline else 2.6)), joint="curve")
    if not sparkline:
        ex, ey = pts[-1]
        d.ellipse([ex - 3.5 * SS, ey - 3.5 * SS, ex + 3.5 * SS, ey + 3.5 * SS], fill=colour)


def asof_text(q):
    """'As of 4:00 PM EDT Tue' in the exchange's timezone; for a stale quote, when we last had one."""
    if q.get("stale"):
        f = dt.datetime.fromtimestamp(q.get("fetched", 0), HOME_TZ)
        return f"Old quote: {f.strftime('%-I:%M %p %a')}"
    try:
        z = ZoneInfo(q.get("tz") or "UTC")
    except Exception:
        z = dt.timezone.utc
    t = dt.datetime.fromtimestamp(q.get("time") or q.get("fetched") or 0, z)
    return f"As of {t.strftime('%-I:%M %p')} {t.strftime('%Z')} {t.strftime('%a')}"


RANGE_NAME = {"1d": "", "5d": "5 DAYS", "1mo": "1 MONTH", "6mo": "6 MONTHS", "1y": "1 YEAR"}
LOGO_HDR = (6, 5, 26)                   # header logo tile: x, y, size
NAV_Y = 204                             # screens: the button bar


def header(d, T, sym, name, label, logo):
    """Ticker (big), logo tile, the company name only if it fits legibly, market pill."""
    screens.grad(d, (0, 0, W * SS, 36 * SS), T["panel_top"], T["panel_bot"])
    d.line([(0, 36 * SS), (W * SS, 36 * SS)], fill=T["sep"], width=SS)
    sx = 8
    if logo is not None:
        logo_tile(d, T, *LOGO_HDR)
        sx = LOGO_HDR[0] + LOGO_HDR[2] + 6
    tf = font(VERDB, 19)
    while tf.getlength(sym) > 150 * SS:
        tf = font(VERDB, tf.size / SS - 1)
    d.text((sx * SS, 18 * SS), sym, font=tf, fill=T["text"], anchor="lm")
    px = state_pill(d, T, W - 6, 18, label) if label else W - 6
    nx = sx + tf.getlength(sym) / SS + 8
    nf = font(VERDB, 12)
    if name and nf.getlength(name) <= (px - nx - 6) * SS:   # legible or not at all: never shrunk or cut
        d.text((nx * SS, 18 * SS), name, font=nf, fill=T["sub"], anchor="lm")
    return sx != 8


def detail_page(q, now, T=LIGHT, idx=0, n=1, logo=None, nav=None):
    """One stock, laid out to read at arm's length on the 3.5" LCD. nav=None: a slideshow
    page; nav=dict(month=bool, on_month=bool): an interactive screen with a button bar."""
    sym = q["symbol"]
    img = Image.new("RGB", (W * SS, H * SS))
    d = ImageDraw.Draw(img)
    screens.grad(d, (0, 0, W * SS, H * SS), T["bg_top"], T["bg_bot"])
    err = "error" in q
    has_logo = header(d, T, sym, None if err else q.get("name", ""), None if err else market_state(q, now),
                      None if err else logo)
    bottom = NAV_Y - 4 if nav else H - 4
    if err:
        d.rounded_rectangle([14 * SS, 52 * SS, 306 * SS, (bottom - 8) * SS], radius=12 * SS, fill=T["card"],
                            outline=T["frame1"], width=SS * 2)
        cy = (52 + bottom - 8) / 2
        d.text((160 * SS, (cy - 18) * SS), q["error"], font=font(ROUND, 26), fill=T["text"], anchor="mm")
        hint = (["Check the symbol on the", "config page (ASX ends in .AX)"] if q["error"] == "Unknown symbol"
                else ["Yahoo Finance didn't answer.", "Trying again soon."])
        for k, line in enumerate(hint):
            d.text((160 * SS, (cy + 12 + k * 18) * SS), line, font=font(VERDB, 13), fill=T["sub"], anchor="mm")
    else:
        ch, pct = change(q)
        col = T["flat"] if ch is None or abs(ch) < 1e-9 else T["up"] if ch > 0 else T["down"]
        # price, and the change stacked on the right
        ptxt = fmt_price(q["price"], q)
        pf = font(ROUND, 46)
        while pf.getlength(ptxt) > 196 * SS:
            pf = font(ROUND, pf.size / SS - 2)
        d.text((8 * SS, 64 * SS), ptxt, font=pf, fill=T["text"], anchor="lm")
        if ch is not None:
            cf_ = font(VERDB, 17)
            a = f"{abs(ch):,.{q.get('decimals', 2)}f}"
            b = f"{pct:+.2f}%"
            aw = cf_.getlength(a) / SS
            tx = W - 8 - aw - 16
            tri = [(tx, 55), (tx + 12, 55), (tx + 6, 44)] if ch >= 0 else [(tx, 44), (tx + 12, 44), (tx + 6, 55)]
            d.polygon([(x * SS, y * SS) for x, y in tri], fill=col)
            d.text(((W - 8) * SS, 50 * SS), a, font=cf_, fill=col, anchor="rm")
            d.text(((W - 8) * SS, 74 * SS), b, font=cf_, fill=col, anchor="rm")
        # chart card: as-of (or range) in its top-left corner, high/low on the right
        stats_y = bottom - 34
        cy0, cy1 = 90, stats_y - 4
        d.rounded_rectangle([5 * SS, cy0 * SS, 315 * SS, cy1 * SS], radius=8 * SS, fill=T["card"], outline=T["frame1"], width=SS)
        rq = dict(q, series=q["alt"]["1mo"], range="1mo") if nav and nav.get("on_month") and (q.get("alt") or {}).get("1mo") else q
        tag = asof_text(q)
        if RANGE_NAME.get(rq.get("range"), ""):
            tag = RANGE_NAME[rq["range"]] + " · " + tag
        d.text((12 * SS, (cy0 + 11) * SS), tag, font=font(VERDB, 12), fill=T["amber"] if q.get("stale") else T["sub"], anchor="lm")
        chart(img, T, (12, cy0 + 24, 250, cy1 - 8), rq, col)
        # four stats across: label over value
        stats = [("OPEN", fmt_price(q.get("open"), q, False)), ("HIGH", fmt_price(q.get("high"), q, False)),
                 ("LOW", fmt_price(q.get("low"), q, False)), ("VOL", fmt_big(q.get("volume")))]
        for i, (k, v) in enumerate(stats):
            x = 8 + i * 78
            d.text((x * SS, stats_y * SS), k, font=font(VERDB, 12), fill=T["sub"])
            text_fit(d, (x, stats_y + 14), v, VERDB, 17, T["text"], 74, min_size=13)
    if nav:
        nav_bar(d, T, nav, err)
    out = screens.small(img)
    if has_logo:
        paste_logo(out, logo, *LOGO_HDR, pad=3)
    return out


NAV = {"prev": (4, 46), "list": (52, 112), "1d": (118, 156), "1m": (158, 196), "next": (202, 244), "done": (252, 316)}


def nav_button(d, T, box, label, on=False, glyph=None):
    x0, x1 = box
    y0, y1 = NAV_Y + 4, H - 4
    d.rounded_rectangle([x0 * SS, y0 * SS, x1 * SS, y1 * SS], radius=8 * SS,
                        fill=T["text"] if on else T["card"], outline=T["text"], width=SS * 2)
    col = T["card"] if on else T["text"]
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    if glyph:
        s = 1 if glyph == "right" else -1
        d.polygon([((cx - 6 * s) * SS, (cy - 8) * SS), ((cx + 7 * s) * SS, cy * SS), ((cx - 6 * s) * SS, (cy + 8) * SS)], fill=col)
    else:
        d.text((cx * SS, cy * SS), label, font=font(VERDB, 13), fill=col, anchor="mm")


def nav_bar(d, T, nav, err):
    screens.grad(d, (0, NAV_Y * SS, W * SS, H * SS), T["panel_top"], T["panel_bot"])
    d.line([(0, NAV_Y * SS), (W * SS, NAV_Y * SS)], fill=T["sep"], width=SS)
    nav_button(d, T, NAV["prev"], "", glyph="left")
    nav_button(d, T, NAV["list"], "LIST")
    if nav.get("month") and not err:
        nav_button(d, T, NAV["1d"], "1D", on=not nav.get("on_month"))
        nav_button(d, T, NAV["1m"], "1M", on=bool(nav.get("on_month")))
    nav_button(d, T, NAV["next"], "", glyph="right")
    nav_button(d, T, NAV["done"], "DONE")


ROW_H = 33


def quote_rows(img, d, T, rows, now, logos, y0, rh=ROW_H):
    """The list rows shared by the summary page and the list screens: logo, ticker, sparkline,
    price, % pill. Returns the logo pastes to do after the downscale."""
    tiles = any(logos.get(q["symbol"]) is not None for q in rows)
    ts = min(26, rh - 6)
    tx = 8 + (ts + 7 if tiles else 0)
    pastes = []
    for i, q in enumerate(rows):
        y = y0 + i * rh
        if i:
            d.line([(6 * SS, y * SS), (314 * SS, y * SS)], fill=T["frame2"], width=SS)
        cy = y + rh / 2
        if tiles:
            ty = int(round(cy - ts / 2))
            if logos.get(q["symbol"]) is not None:
                logo_tile(d, T, 7, ty, ts)
                pastes.append((logos[q["symbol"]], 7, ty, ts))
            else:
                monogram(d, T, 7, ty, ts, q["symbol"])
        text_fit(d, (tx, cy), q["symbol"], VERDB, 16, T["text"], 108 - tx, anchor="lm", min_size=12)
        if "error" in q:
            d.text((300 * SS, cy * SS), q["error"], font=font(VERDB, 13), fill=T["amber"], anchor="rm")
            continue
        ch, pct = change(q)
        col = T["flat"] if ch is None or abs(ch) < 1e-9 else T["up"] if ch > 0 else T["down"]
        chart(img, T, (116, y + 7, 160, y + rh - 7), q, col, labels=False, sparkline=True)
        text_fit(d, (252, cy), fmt_price(q["price"], q), VERDB, 16, T["text"], 82, anchor="rm", min_size=12)
        d.rounded_rectangle([256 * SS, (cy - 12) * SS, 314 * SS, (cy + 12) * SS], radius=5 * SS, fill=col)
        text_fit(d, (285, cy), f"{pct:+.1f}%" if pct is not None else "–", VERDB, 14, T["pill_text"], 54,
                 anchor="mm", min_size=11)
    return pastes


def list_header(d, img, T, title):
    screens.grad(d, (0, 0, W * SS, 36 * SS), T["panel_top"], T["panel_bot"])
    d.line([(0, 36 * SS), (W * SS, 36 * SS)], fill=T["sep"], width=SS)
    screens.paste_logo(img, 18 * SS, 18 * SS, 24 * SS)
    d.text((36 * SS, 18 * SS), title, font=font(VERDB, 17), fill=T["text"], anchor="lm")


def summary_page(quotes, now, T=LIGHT, n_pages=1, logos=None, page=None):
    """Every stock in rows. page=None: the slideshow page (up to 6 rows). page=(k, pages):
    list screen k with up to LIST_ROWS rows and a button bar (▲ ▼ DONE)."""
    logos = logos or {}
    img = Image.new("RGB", (W * SS, H * SS))
    d = ImageDraw.Draw(img)
    screens.grad(d, (0, 0, W * SS, H * SS), T["bg_top"], T["bg_bot"])
    list_header(d, img, T, "Stock Quotes")
    if page is None:
        rows = quotes[:6]
        rh = min(ROW_H + 3, 200 // max(1, len(rows)))
    else:
        k, pages = page
        rows = quotes[k * LIST_ROWS:(k + 1) * LIST_ROWS]
        rh = ROW_H
        if pages > 1:
            d.text(((W - 8) * SS, 18 * SS), f"{k + 1}/{pages}", font=font(VERDB, 14), fill=T["sub"], anchor="rm")
    pastes = quote_rows(img, d, T, rows, now, logos, 38, rh)
    if not quotes:
        d.text((160 * SS, 110 * SS), "No stocks chosen yet", font=font(ROUND, 18), fill=T["text"], anchor="mm")
        d.text((160 * SS, 136 * SS), "Settings > APPS shows how", font=font(VERDB, 13), fill=T["sub"], anchor="mm")
    if page is not None:
        screens.grad(d, (0, NAV_Y * SS, W * SS, H * SS), T["panel_top"], T["panel_bot"])
        d.line([(0, NAV_Y * SS), (W * SS, NAV_Y * SS)], fill=T["sep"], width=SS)
        k, pages = page
        if pages > 1:
            for box, up in (((4, 76), True), ((82, 154), False)):
                x0, x1 = box
                d.rounded_rectangle([x0 * SS, (NAV_Y + 4) * SS, x1 * SS, (H - 4) * SS], radius=8 * SS, fill=T["card"],
                                    outline=T["text"], width=SS * 2)
                cx, cy = (x0 + x1) / 2, (NAV_Y + H) / 2
                s = -1 if up else 1
                d.polygon([((cx - 9) * SS, (cy - 5 * s) * SS), ((cx + 9) * SS, (cy - 5 * s) * SS), (cx * SS, (cy + 6 * s) * SS)],
                          fill=T["text"])
        else:
            d.text((10 * SS, (NAV_Y + 18) * SS), "Tap a stock for its chart", font=font(VERDB, 13), fill=T["sub"], anchor="lm")
        nav_button(d, T, NAV["done"], "DONE")
    out = screens.small(img)
    for lg, x, y, size in pastes:
        paste_logo(out, lg, x, y, size, pad=3)
    return out


# ---------------------------------------------------------------- feeder entry point
ENTRY = "stocks_list"                    # the interactive list screen (Home / launcher / pagetap)
LIST_ROWS = 5                            # rows per list screen (33 px each: big tap targets)
MONTH_MAX = 6                            # 1D/1M toggle (2 screens per stock) only up to this many stocks


def _put(config, rgb565, img, out, name):
    """Stable slot names, written only when the pixels change (delta delivery, lazy engine)."""
    if config.write_if_changed(rgb565, img, f"{out}/{name}.raw") or not os.path.exists(f"{out}/{name}.png"):
        img.save(f"{out}/{name}.png")


def render(out, first_page, rgb565, opts=None, slides=None, seconds=12, now=None, cache_path=None,
           night_files=False, screens_too=True):
    """Fetch, draw and write this app's slideshow pages and its interactive screens.
    Returns Rendered(lines, titles, thumbs): lines hold the pages (numbered from
    first_page), a 'pagetap PAGE stocks_list' per page, and the screens with their hots.
    slides: [(subslide_id, pages)] as config.plan() cut them to fit wxshow's 12 pages
    (default: every sub-slide uncut; [] = screens only)."""
    import config
    opts, _ = config.validate(CONFIG_SCHEMA, opts or {})
    now = now or dt.datetime.now(HOME_TZ)
    if slides is None:
        slides = [(s["id"], page_count(s["id"], opts)) for s in APP["subslides"]]
    slides = [(sid, n) for sid, n in slides if n > 0]
    tickers = opts["tickers"]
    if not tickers or (not slides and not screens_too):
        clean_slots(out, set())
        return Rendered([], [], [])
    if cache_path is None:
        cache_path = os.path.join(config.state_dir(), "stocks-cache.json")
        if not os.access(os.path.dirname(cache_path), os.W_OK):
            cache_path = os.path.join(out, "stocks-cache.json")
    month = screens_too and len(tickers) <= MONTH_MAX
    data = get_quotes(tickers, opts["range"], cache_path, extra=("1mo",) if month and opts["range"] != "1mo" else ())
    logos = get_logos(tickers, os.path.join(os.path.dirname(cache_path), "stock-logos"))
    quotes = [data[s] for s in tickers]
    keep, lines, titles, thumbs = set(), [], [], []
    # ---- slideshow pages
    pages = []
    for sid, n in slides:
        if sid == "summary":
            pages.append(("stocks_summary", "Stock Quotes", None))
        else:
            pages += [(f"stocks_{i}", f"Stocks: {q['symbol']}", q) for i, q in enumerate(quotes[:n])]
    for k, (name, title, q) in enumerate(pages):
        for T, sfx in ((LIGHT, ""), (NIGHT, "_night")):
            if sfx and not night_files:
                continue
            img = (summary_page(quotes, now, T, len(pages), logos) if q is None
                   else detail_page(q, now, T, k, len(pages), logos.get(q["symbol"])))
            _put(config, rgb565, img, out, name + sfx)
            keep.add(name + sfx)
            if not sfx:
                thumbs.append(img)
        lines.append(f"page {first_page + k} {name}.raw {seconds} noclock noticker")
        if night_files:                                          # drawn instead in night mode
            lines.append(f"pagenight {first_page + k} {name}_night.raw")
        lines.append(f"pagetap {first_page + k} {ENTRY}")        # a tap opens the list (DONE comes back)
        titles.append(title)
    # ---- interactive screens: list (paged) -> detail (prev/next, 1D/1M) -> back
    if screens_too:
        n = len(quotes)
        npg = max(1, math.ceil(n / LIST_ROWS))
        lname = lambda k: ENTRY if k == 0 else f"{ENTRY}{k}"
        dname = lambda i, m: f"stocks_d{i % n}" + ("_m" if m else "")
        done = "slideshow"
        for T, sfx in ((LIGHT, ""), (NIGHT, "_night")):
            for k in range(npg):
                scr = lname(k) + sfx
                _put(config, rgb565, summary_page(quotes, now, T, 1, logos, page=(k, npg)), out, scr)
                keep.add(scr)
                lines.append(f"screen {scr} {scr}.raw")
                for r, i in enumerate(range(k * LIST_ROWS, min(n, (k + 1) * LIST_ROWS))):
                    lines.append(f"hot {scr} 0 {38 + r * ROW_H} {W} {ROW_H} screen {dname(i, False)}")
                if npg > 1:
                    lines.append(f"hot {scr} 0 {NAV_Y} 80 {H - NAV_Y} screen {lname((k - 1) % npg)}")
                    lines.append(f"hot {scr} 80 {NAV_Y} 78 {H - NAV_Y} screen {lname((k + 1) % npg)}")
                lines.append(f"hot {scr} 248 {NAV_Y} {W - 248} {H - NAV_Y} {done}")
            for i, q in enumerate(quotes):
                for m in ((False, True) if month and "error" not in q else (False,)):
                    scr = dname(i, m) + sfx
                    img = detail_page(q, now, T, i, n, logos.get(q["symbol"]), nav={"month": month, "on_month": m})
                    _put(config, rgb565, img, out, scr)
                    keep.add(scr)
                    lines.append(f"screen {scr} {scr}.raw")
                    bar = H - NAV_Y
                    lines += [f"hot {scr} 0 {NAV_Y} 49 {bar} screen {dname(i - 1, m and 'error' not in quotes[(i - 1) % n])}",
                              f"hot {scr} 49 {NAV_Y} 66 {bar} screen {lname(i // LIST_ROWS)}",
                              f"hot {scr} 199 {NAV_Y} 49 {bar} screen {dname(i + 1, m and 'error' not in quotes[(i + 1) % n])}",
                              f"hot {scr} 248 {NAV_Y} {W - 248} {bar} {done}"]
                    if month and "error" not in q:
                        lines += [f"hot {scr} 115 {NAV_Y} 42 {bar} screen {dname(i, False)}",
                                  f"hot {scr} 157 {NAV_Y} 42 {bar} screen {dname(i, True)}"]
    clean_slots(out, keep)
    return Rendered(lines, titles, thumbs)


def clean_slots(out, keep):
    """Remove slot files left by an earlier render with more stocks, so they aren't delivered."""
    for f in os.listdir(out):
        m = re.fullmatch(r"(stocks_(?:summary|\d+|list\d*|d\d+(?:_m)?)(?:_night)?)\.(raw|png)", f)
        if m and m.group(1) not in keep:
            try:
                os.unlink(os.path.join(out, f))
            except OSError:
                pass
