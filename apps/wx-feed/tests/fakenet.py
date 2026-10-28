"""fakenet: canned data for every source the renderer fetches, so a render runs offline.

install(now) replaces urllib.request.urlopen with a function that answers each URL the
widgets ask for (BOM's FTP XML, the news RSS and article pages, Yahoo's chart JSON, the
logo hosts, SomaFM) with made-up data of the same shape, and refuses anything else. It
also freezes the clock (datetime.now, date.today, time.time) at `now`, so two renders
with the same data are byte-identical. Everything here is synthetic: no third-party
content is stored in the repository.
"""
import datetime as dt
import io
import json
import math
import re
import time
import urllib.error
import urllib.parse
import urllib.request
import zlib

from PIL import Image, ImageDraw

REQUESTS = []                                  # every URL asked for, in order (for tests)


# ---------------------------------------------------------------- the clock
def freeze(now):
    """Every datetime.now / date.today / time.time answers `now` (an aware datetime)."""
    base_dt, base_date = dt.datetime, dt.date
    epoch = now.timestamp()

    class FrozenDateTime(base_dt):
        @classmethod
        def now(cls, tz=None):
            return base_dt.fromtimestamp(epoch, tz) if tz else base_dt.fromtimestamp(epoch)

        @classmethod
        def today(cls):
            return base_dt.fromtimestamp(epoch)

        @classmethod
        def utcnow(cls):
            return base_dt.fromtimestamp(epoch, dt.timezone.utc).replace(tzinfo=None)

    class FrozenDate(base_date):
        @classmethod
        def today(cls):
            return base_dt.fromtimestamp(epoch).date()

    dt.datetime = FrozenDateTime
    dt.date = FrozenDate
    time.time = lambda: epoch


# ---------------------------------------------------------------- made-up data
def _seed(url):
    return zlib.crc32(url.encode())


def picture(url, w=240, h=135):
    """A JPEG that differs per URL: a gradient and a few shapes."""
    s = _seed(url)
    c1 = (s & 0xFF, (s >> 8) & 0xFF, (s >> 16) & 0xFF)
    c2 = (255 - c1[1], 255 - c1[2], 255 - c1[0])
    im = Image.new("RGB", (w, h))
    d = ImageDraw.Draw(im)
    for y in range(h):
        t = y / (h - 1)
        d.line([(0, y), (w, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(c1, c2)))
    for k in range(4):
        x, y, r = (s >> (k * 5)) % w, (s >> (k * 3)) % h, 12 + (s >> k) % 30
        d.ellipse([x - r, y - r, x + r, y + r], fill=c2 if k % 2 else c1)
    buf = io.BytesIO()
    im.save(buf, "JPEG", quality=85)
    return buf.getvalue()


def logo(sym):
    im = Image.new("RGBA", (64, 64), (0, 0, 0, 0))
    s = _seed(sym)
    ImageDraw.Draw(im).ellipse([4, 4, 60, 60], fill=(s & 0xFF, (s >> 8) & 0x7F, 160, 255))
    buf = io.BytesIO()
    im.save(buf, "PNG")
    return buf.getvalue()


def bom_observations(now, wmo):
    t = now.strftime("%Y-%m-%dT%H:%M:00+10:30")
    els = {"apparent_temp": "19.4", "air_temperature": "21.3", "rel-humidity": "48", "wind_dir": "SSW",
           "wind_spd_kmh": "19", "gust_kmh": "28", "msl_pres": "1016.2", "rainfall": "0.0",
           "maximum_air_temperature": "24.1", "minimum_air_temperature": "12.8", "weather": "-", "cloud": "Partly cloudy"}
    e = "".join(f'<element units="" type="{k}">{v}</element>' for k, v in els.items())
    return (f'<?xml version="1.0"?><product><observations>'
            f'<station wmo-id="00000" stn-name="ELSEWHERE"><period index="0" time-local="{t}"><level index="0">'
            f'<element type="air_temperature">5.0</element></level></period></station>'
            f'<station wmo-id="{wmo}" stn-name="TOWN AERO"><period index="0" time-local="{t}"><level index="0">{e}'
            f'</level></period></station></observations></product>').encode()


PRECIS = ["Partly cloudy.", "Shower or two. Light winds.", "Sunny.", "Possible thunderstorm. Windy.",
          "Mostly sunny.", "Cloudy. Chance of rain.", "Fog then sunny.", "Hazy sunshine and a gusty wind."]


def bom_forecast(now, aac):
    days = []
    for i in range(7):
        d = (now + dt.timedelta(days=i)).strftime("%Y-%m-%dT05:00:00+10:30")
        code = [3, 11, 1, 16, 1, 4, 6, 9][i]
        lo = "" if i == 0 else f'<element type="air_temperature_minimum" units="Celsius">{10 + i}</element>'
        days.append(f'<forecast-period index="{i}" start-time-local="{d}">'
                    f'<element type="forecast_icon_code">{code}</element>{lo}'
                    f'<element type="air_temperature_maximum" units="Celsius">{22 + i}</element>'
                    f'<text type="precis">{PRECIS[i]}</text>'
                    f'<text type="probability_of_precipitation">{(i * 17) % 90}%</text></forecast-period>')
    return (f'<?xml version="1.0"?><product><forecast><area aac="SA_PT001" description="Elsewhere" type="location">'
            f'</area><area aac="{aac}" description="Town" type="location">{"".join(days)}</area>'
            f'</forecast></product>').encode()


HEADLINES = ["Council approves new playground for the foreshore", "Rain gauge overflows after a wet weekend",
             "Local bakery wins a state award for its pasties", "Wind farm opens its doors for a community day",
             "Train line upgrade to finish ahead of schedule", "School robotics team heads to the national final",
             "Researchers count a record number of cuttlefish", "Library extends opening hours over summer",
             "Festival announces its biggest line-up yet", "Jetty repairs begin next month",
             "Footy club celebrates a hundred years", "New bus timetable starts on Monday"]


def rss(url, now):
    """An RSS 2.0 feed with media:content pictures and links to article pages."""
    s = _seed(url)
    items = []
    for i in range(10):
        title = HEADLINES[(s + i) % len(HEADLINES)]
        pub = (now - dt.timedelta(hours=i * 5 + 1)).strftime("%a, %d %b %Y %H:%M:%S +1030")
        link = f"https://www.abc.net.au/news/{now:%Y-%m-%d}/story-{s % 997}-{i}/{100000 + s % 9000 + i}"
        img = f"https://live-production.wcms.abc-cdn.net.au/fake{s % 97}{i}?width=862&amp;height=485"
        items.append(f"<item><title>{title}</title><link>{link}</link><description>{title}. More soon.</description>"
                     f"<pubDate>{pub}</pubDate><media:content url=\"{img}\" medium=\"image\" isDefault=\"true\"/></item>")
    return ('<?xml version="1.0"?><rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/"><channel>'
            f'<title>Feed</title>{"".join(items)}</channel></rss>').encode()


def town_page(url, now):
    """A regional page with a JSON-LD ItemList of local story links."""
    links = [f"https://www.abc.net.au/news/{(now - dt.timedelta(days=i)):%Y-%m-%d}/local-story-{i}/{200000 + i}"
             for i in range(9)]
    ld = json.dumps({"@type": "ItemList", "mainEntity": {"itemListElement": [{"url": u} for u in links]}})
    return f'<html><head><script type="application/ld+json">{ld}</script></head><body></body></html>'.encode()


def article(url, now):
    s = _seed(url)
    title = HEADLINES[s % len(HEADLINES)]
    paras = [f"Paragraph {k + 1} of a made-up story, long enough to wrap across a few lines on the "
             f"Chumby's little screen, which is what the reader paginates." for k in range(6)]
    kids = [{"type": "tagname", "key": "p", "children": [{"type": "text", "content": p}]} for p in paras]
    kids.insert(2, {"type": "tagname", "key": "h2", "children": [{"type": "text", "content": "A subheading"}]})
    doc = {"props": {"pageProps": {"document": {"loaders": {"articledetail": {
        "title": title, "datelinePrepared": {"publishedDate": (now - dt.timedelta(hours=3)).isoformat()},
        "text": {"descriptor": {"children": kids}}}}}}}}
    img = f"https://live-production.wcms.abc-cdn.net.au/art{s % 89}?width=862&amp;height=485"
    return (f'<html><head><meta property="og:title" content="{title}"><meta property="og:image" content="{img}">'
            f'<meta property="og:description" content="{title}."></head><body>'
            f'<script id="__NEXT_DATA__" type="application/json">{json.dumps(doc)}</script></body></html>').encode()


def yahoo_chart(url, now):
    path = urllib.parse.urlsplit(url).path
    sym = urllib.parse.unquote(path.rsplit("/", 1)[1])
    if sym.startswith("NOPE"):
        raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
    q = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
    rng = q.get("range", ["1d"])[0]
    s = _seed(sym)
    base = 20 + s % 300
    end = int(now.timestamp()) // 300 * 300
    n, step = {"1d": (78, 300), "5d": (65, 1800), "1mo": (22, 86400), "6mo": (126, 86400), "1y": (252, 86400)}[rng]
    ts = [end - (n - 1 - k) * step for k in range(n)]
    closes = [round(base * (1 + 0.03 * math.sin(k / 7 + s % 5) + 0.01 * math.cos(k / 3)), 2) for k in range(n)]
    tz = "Australia/Sydney" if sym.endswith(".AX") else "America/New_York"
    meta = {"symbol": sym, "longName": f"{sym.split('.')[0].title()} Holdings Ltd", "currency": "AUD" if ".AX" in sym else "USD",
            "fullExchangeName": "ASX" if ".AX" in sym else "NasdaqGS", "exchangeTimezoneName": tz, "priceHint": 2,
            "regularMarketPrice": closes[-1], "previousClose": closes[0] * 0.99, "chartPreviousClose": closes[0] * 0.99,
            "regularMarketDayHigh": max(closes), "regularMarketDayLow": min(closes), "regularMarketVolume": 1234567 + s % 99999,
            "fiftyTwoWeekLow": min(closes) * 0.8, "fiftyTwoWeekHigh": max(closes) * 1.2, "regularMarketTime": ts[-1],
            "currentTradingPeriod": {"pre": {"start": ts[0] - 3600, "end": ts[0]}, "regular": {"start": ts[0], "end": ts[0] + 23400},
                                     "post": {"start": ts[0] + 23400, "end": ts[0] + 30000}},
            "tradingPeriods": [[{"start": ts[0], "end": ts[0] + 23400}]]}
    res = {"meta": meta, "timestamp": ts, "indicators": {"quote": [{"close": closes, "open": [c * 0.999 for c in closes]}]}}
    return json.dumps({"chart": {"result": [res], "error": None}}).encode()


SOMA = ["groovesalad", "dronezone", "deepspaceone", "lush", "secretagent", "spacestation"]


def somafm(url):
    if url.endswith("channels.json"):
        return json.dumps({"channels": [{"id": c, "title": c.title(), "description": f"Made-up description of {c}.",
                                         "image": f"https://api.somafm.com/img/{c}.png"} for c in SOMA]}).encode()
    if url.endswith(".pls"):
        sid = url.rsplit("/", 1)[1][:-4]
        return f"[playlist]\nnumberofentries=1\nFile1=http://ice1.somafm.com/{sid}-128-mp3\n".encode()
    return picture(url, 120, 120)


def answer(url, now):
    u = urllib.parse.urlsplit(url)
    host, path = u.hostname or "", u.path
    if u.scheme == "ftp" and host == "ftp.bom.gov.au":
        if path.endswith("IDS60920.xml") or "obs" in path:
            return bom_observations(now, FAKE["wmo"])
        return bom_forecast(now, FAKE["aac"])
    if host.endswith("finance.yahoo.com"):
        return yahoo_chart(url, now)
    if host in ("financialmodelingprep.com", "assets.parqet.com"):
        sym = urllib.parse.unquote(path.rsplit("/", 1)[1]).removesuffix(".png")
        if _seed(sym) % 3 == 0:                                # some symbols have no logo
            raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
        return logo(sym)
    if host.endswith("somafm.com"):
        return somafm(url)
    if "abc-cdn.net.au" in host:
        return picture(url)
    if host == "www.abc.net.au":
        if path.endswith("rss.xml"):
            return rss(url, now)
        if re.match(r"/news/\d{4}-\d{2}-\d{2}/", path):
            return article(url, now)
        return town_page(url, now)
    raise urllib.error.URLError(f"fakenet: no canned answer for {url}")


FAKE = {"wmo": "95664", "aac": "SA_PT055"}


class _Resp(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *a):
        self.close()


def install(now, wmo=None, aac=None):
    """Freeze the clock at `now` and answer every fetch from the made-up data above."""
    if wmo:
        FAKE["wmo"] = wmo
    if aac:
        FAKE["aac"] = aac
    when = now

    def urlopen(req, *a, **kw):
        url = req.full_url if isinstance(req, urllib.request.Request) else str(req)
        REQUESTS.append(url)
        return _Resp(answer(url, when))

    urllib.request.urlopen = urlopen
    freeze(now)
