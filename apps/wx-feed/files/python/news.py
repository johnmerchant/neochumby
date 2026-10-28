"""news: an ABC News reader for wxshow, pre-rendered like every other screen.

A slideshow app (config.py registry: APP, CONFIG_SCHEMA, page_count): one slide per
enabled source with its newest headlines; tapping the slide (pagetap) opens that
source's list. Each enabled source is a sub-slide, and a tab in the reader:
  nat   ABC Top Stories RSS (curated; "Just In" is a firehose of live blogs)
  adl   ABC Adelaide RSS
  eyre  ABC Eyre Peninsula: no RSS exists, so the links are scraped from
        https://www.abc.net.au/eyre and each story's title, date and photo come from
        the article page itself

Screens (stable slot names, so a re-render every 10 minutes just replaces pixels):
  news_slide_<src>          the slideshow page (no times on it: it changes with the headlines)
  news_<src>_l<k>[_night]   headline list page k: 5 rows of thumbnail + headline + time
                            ("4:12 pm", "Yesterday", "Tue": no ages, so a re-render with the
                            same stories rewrites nothing). DONE and the logo: slideshow
  news_<src>_s<i>_p<j>      story i, page j: photo + headline + body text, paginated.
                            Always in the ABC deep blue, which reads by day and at
                            night, so story pages need no _night copies (that halves
                            the bytes; set STORY_NIGHT = True to render light + dark)

Story pages are full screens (SHARED_CHROME = True switches to one shared chrome file
plus a 320x170 text pane per page, once wxshow can share a file between screens). The
full article body is extracted from the page's __NEXT_DATA__ JSON; the RSS summary is
the fallback.

Budget: changed .raws are delivered over WiFi, and wxshow has MAXSCR 96, so the reader caps itself (STORIES, STORY_PAGE_BUDGET): each story gets a first page, then the
spare pages go round-robin to the top stories. A story cut short ends with a
"continues at abc.net.au" line.

Fetched web content is untrusted: it is only ever drawn as pixels. File and screen
names come from slot numbers, never from the content.

API:  r = render_slides(out, first_page, rgb565, opts, slides, seconds, now=now)
                                         # Rendered(lines, titles, thumbs): pages, pagetaps
                                         # and the reader screens; never raises
      lines = render(out, rgb565)        # the reader screens alone (every source)
      python3 news.py assets BANNER.png [DIR]  # cut the header logo and swirls out of a
                                         # banner you have the right to use, into DIR

The header's logo and swirls are not bundled (they are the ABC's): without them in the
asset directories (wxassets), the header shows a plain NEWS mark of our own.
"""
import datetime as dt
import email.utils
import hashlib
import html
import io
import json
import os
import re
import sys
import time
import urllib.request
from collections import namedtuple
from concurrent.futures import ThreadPoolExecutor
from zoneinfo import ZoneInfo

from PIL import Image, ImageDraw, ImageFont

import config
import wxassets

W, H = 320, 240
TZ = ZoneInfo(config.DEFAULT_TZ)                 # wxfeed.set_timezone sets the configured one
UA = config.USER_AGENT
HERE = os.path.dirname(os.path.abspath(__file__))
ASSETS = HERE + "/assets"
FD = ASSETS + "/fonts/"
HEAD = FD + "LiberationSans-Bold.ttf"
NARROW = FD + "LiberationSansNarrow-Bold.ttf"
BODY = FD + "DejaVuSans.ttf"
BODYB = FD + "DejaVuSans-Bold.ttf"

ENTRY = "news_nat_l0"
SOURCES = [  # (key, tab label, long name, kind, url)
    ("nat", "National", "ABC News", "rss", "https://www.abc.net.au/news/feed/45910/rss.xml"),
    ("adl", "Adelaide", "ABC Adelaide", "rss", "https://www.abc.net.au/news/feed/8057540/rss.xml"),
    ("eyre", "Eyre", "ABC Eyre Peninsula", "eyre", "https://www.abc.net.au/eyre"),
]
STORIES = {"nat": 8, "adl": 8, "eyre": 5}    # headlines per source (5 per list page)
PER_LIST = 5
STORY_PAGE_BUDGET = 30                        # story pages in total, across all sources
STORY_NIGHT = False                           # True: light story pages + _night copies (2x bytes)
# True: story pages = one shared chrome file + a 320x170 text pane each (delivery 20% smaller).
# Leave False until wxshow shares a file that several screens name: today load() slurps
# the chrome once PER SCREEN, so shared chrome costs MORE RAM (262 KB/page vs 154 KB).
SHARED_CHROME = False

# ---- the app, for config.py's registry (one sub-slide per source; its tab exists only if on)
APP = {"id": "news", "name": "ABC News", "icon": "news.png", "seconds": 12,
       "description": "The latest ABC headlines with photos. Tap a slide to read the stories.",
       "subslides": [{"id": "nat", "label": "National (ABC Top Stories)", "default": True},
                     {"id": "adl", "label": "ABC Adelaide", "default": True},
                     {"id": "eyre", "label": "ABC Eyre Peninsula", "default": True}]}
CONFIG_SCHEMA = [
    {"key": "stories", "type": "number", "label": "Stories per source", "default": 8, "min": 3, "max": 10,
     "integer": True, "help": "How many headlines each source's list shows when you tap in. "
                              "Eyre Peninsula often has fewer."},
]
SLIDE_ROWS = 3                                # headlines on a slide: big enough to read across the room


def page_count(subslide, opts):
    """Every enabled source is one slide."""
    return 1


Rendered = namedtuple("Rendered", "lines titles thumbs")
TIMEOUT = 12
WORKERS = 4
ARTICLE_TTL = 3600                            # article bodies rarely change: refetch hourly
IMAGE_TTL = 86400
MAX_PHOTO_PIXELS = 4_000_000                  # untrusted JPEGs: refuse decompression bombs (checked per photo,
                                              # not via Image.MAX_IMAGE_PIXELS, which is global to every module)

# geometry
HDR = 36                                      # header band (ABC banner)
FOOT = 206                                    # footer band starts here
ROW = (FOOT - HDR) // PER_LIST                # 34 px rows
TAB_X0, TAB_W = 104, 72                       # three tabs after the logo
PANE_Y, PANE_H = HDR, FOOT - HDR              # story text pane: 320 x 170

# palettes: the ABC banner's teal-to-deep-blue, light and night
LIGHT = dict(hdr=((0, 69, 112), (0, 109, 142)), swirl=0.55, logo=1.0, tab=(0, 52, 88), tab_text=(200, 225, 238),
             sel=(240, 246, 249), sel_text=(0, 74, 116), bg=(240, 246, 249), sep=(205, 220, 230),
             head=(16, 36, 64), age=(80, 115, 140), foot=((222, 235, 243), (194, 214, 228)), foot_line=(160, 190, 210),
             btn=((60, 150, 190), (0, 92, 138)), btn_off=((205, 214, 222), (180, 190, 200)), btn_ink=(255, 255, 255),
             label=(40, 70, 100), thumb_edge=(170, 195, 212), none=(200, 214, 224), msg=(40, 70, 100))
NIGHT = dict(hdr=((0, 22, 40), (0, 42, 64)), swirl=0.22, logo=0.8, tab=(0, 16, 30), tab_text=(110, 150, 175),
             sel=(12, 24, 38), sel_text=(150, 210, 235), bg=(12, 24, 38), sep=(30, 48, 66),
             head=(214, 228, 240), age=(110, 145, 168), foot=((16, 30, 46), (8, 18, 30)), foot_line=(34, 56, 78),
             btn=((20, 90, 125), (0, 50, 80)), btn_off=((30, 42, 56), (22, 32, 44)), btn_ink=(200, 228, 240),
             label=(130, 165, 190), thumb_edge=(40, 66, 88), none=(26, 42, 58), msg=(150, 180, 200))
STORY = dict(bg=((0, 52, 86), (0, 30, 54)), text=(228, 238, 245), head=(255, 255, 255), meta=(110, 205, 230),
             sub=(255, 255, 255), more=(110, 205, 230), photo_edge=(70, 130, 165))
STORY_LIGHT = dict(bg=((244, 249, 251), (232, 241, 246)), text=(30, 44, 60), head=(0, 52, 90), meta=(0, 110, 150),
                   sub=(0, 52, 90), more=(0, 110, 150), photo_edge=(170, 195, 212))
SS = 3


def font(path, size):
    return ImageFont.truetype(path, size)


# ---------------------------------------------------------------- fetching (polite, cached)
class Fetcher:
    """One GET per URL per render, with a small on-disk cache (never delivered: it isn't .raw)."""

    def __init__(self, cache_dir):
        self.cache = cache_dir
        os.makedirs(cache_dir, exist_ok=True)
        self.count = 0
        for name in os.listdir(cache_dir):                       # keep the cache from growing forever
            p = os.path.join(cache_dir, name)
            try:
                if time.time() - os.path.getmtime(p) > 2 * IMAGE_TTL:
                    os.remove(p)
            except OSError:
                pass

    def get(self, url, ttl=0):
        path = os.path.join(self.cache, hashlib.sha1(url.encode()).hexdigest())
        if ttl and os.path.exists(path) and time.time() - os.path.getmtime(path) < ttl:
            with open(path, "rb") as fh:
                return fh.read()
        req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept-Encoding": "identity"})
        self.count += 1
        with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
            data = r.read(3_000_000)                   # nothing we want is bigger
        if ttl:
            with open(path + ".tmp", "wb") as fh:
                fh.write(data)
            os.replace(path + ".tmp", path)
        return data


def clean(s):
    """Untrusted text -> one tidy line: unescaped, no control chars, no glyphs our fonts lack."""
    s = html.unescape(s or "")
    s = "".join(ch if ch.isprintable() and ord(ch) < 0x2E00 else " " for ch in s)
    return re.sub(r"\s+", " ", s).strip()


def small_image_url(url, w=240, h=135):
    """ABC's image CDN resizes on request: ask for what we draw, not 862 px."""
    url = html.unescape(url)
    if "abc-cdn.net.au" not in url:
        return url
    url = re.sub(r"([?&])width=\d+", rf"\g<1>width={w}", url)
    return re.sub(r"([?&])height=\d+", rf"\g<1>height={h}", url)


def parse_rss(xml, limit):
    import xml.etree.ElementTree as ET
    ns = {"media": "http://search.yahoo.com/mrss/"}
    ch = ET.fromstring(xml).find("channel")
    out = []
    for it in ch.findall("item"):
        link = (it.findtext("link") or "").strip()
        title = clean(it.findtext("title"))
        if not title or not link.startswith("https://www.abc.net.au/"):
            continue
        img = None
        media = it.findall(".//media:content", ns)
        best = [m for m in media if m.get("isDefault") == "true"] or [m for m in media if m.get("medium") == "image"]
        if best:
            img = best[0].get("url")
        try:
            pub = email.utils.parsedate_to_datetime(it.findtext("pubDate"))
        except Exception:
            pub = None
        out.append({"title": title, "link": link, "summary": clean(it.findtext("description")), "pub": pub, "img": img})
        if len(out) >= limit:
            break
    return out


EYRE_LINK = re.compile(r"/news/(\d{4}-\d{2}-\d{2})/[a-z0-9-]+/(\d+)")


def parse_eyre(page, limit):
    """Local story links on the Eyre Peninsula page, newest first by the date in the URL.

    The page also carries SA Sport, SA News and National News blocks, so not every
    /news/ link is local. The local ones are (1) the page's own JSON-LD ItemList (its
    "Latest" block, ~7 stories) and (2) links above the first non-local heading (Watch,
    Features). Fragile by nature: this depends on ABC's page structure and its
    /news/YYYY-MM-DD/slug/ID links."""
    local = ""
    for m in re.finditer(r'<script type="application/ld\+json">(.*?)</script>', page, re.S):
        try:
            for el in json.loads(m.group(1)).get("mainEntity", {}).get("itemListElement", []):
                local += " " + str(el.get("url", ""))
        except Exception:
            continue
    cut = re.search(r"<h2[^>]*>(?:<[^>]+>)*\s*(SA Sport|SA News|National News|Sport|Just In)\b", page)
    local += page[:cut.start()] if cut else ""
    if not local.strip():
        raise ValueError("no local stories found on the Eyre page (layout changed?)")
    seen, links = set(), []
    for m in EYRE_LINK.finditer(local):
        if m.group(2) in seen:
            continue
        seen.add(m.group(2))
        links.append((m.group(1), int(m.group(2)), "https://www.abc.net.au" + m.group(0)))
    links.sort(reverse=True)
    return [{"title": "", "link": u, "summary": "", "pub": None, "img": None,
             "date": dt.date.fromisoformat(d)} for d, _, u in links[:limit]]


def _text_of(node):
    if isinstance(node, dict):
        if node.get("type") == "text":
            return node.get("content", "")
        return "".join(_text_of(c) for c in node.get("children", []))
    if isinstance(node, list):
        return "".join(_text_of(c) for c in node)
    return ""


def parse_article(page):
    """Title, date, photo and body paragraphs [(kind, text)] from an ABC article page."""
    art = {}
    m = re.search(r'<script id="__NEXT_DATA__"[^>]*>(.*?)</script>', page, re.S)
    if m:
        try:
            d = json.loads(m.group(1))["props"]["pageProps"]["document"]["loaders"]["articledetail"]
        except Exception:
            d = None
        if d:
            art["title"] = clean(d.get("title"))
            try:
                art["pub"] = dt.datetime.fromisoformat(d["datelinePrepared"]["publishedDate"])
            except Exception:
                pass
            paras = []
            try:
                kids = d["text"]["descriptor"]["children"]
            except Exception:
                kids = []
            for c in kids:
                if not isinstance(c, dict) or c.get("type") != "tagname":
                    continue                                        # embeds: video, images, links
                key = c.get("key")
                if key in ("p", "@@standfirst"):
                    t = clean(_text_of(c))
                    if t:
                        paras.append(("p", t))
                elif key in ("h1", "h2", "h3", "h4"):
                    t = clean(_text_of(c))
                    if t:
                        paras.append(("h", t))
                elif key in ("ul", "ol"):
                    for li in c.get("children", []):
                        t = clean(_text_of(li))
                        if t:
                            paras.append(("p", "• " + t))
            art["paras"] = paras
    for prop, key in (("og:title", "og_title"), ("og:image", "img"), ("og:description", "summary")):
        mm = re.search(rf'<meta property="{prop}" content="([^"]*)"', page)
        if mm:
            art[key] = html.unescape(mm.group(1)) if key == "img" else clean(mm.group(1))
    if art.get("summary", "").startswith("Follow the latest headlines from ABC News"):
        art.pop("summary")                                          # the site-wide boilerplate, not this story
    art["title"] = art.get("title") or art.get("og_title", "")
    return art


def slug_title(url):
    m = EYRE_LINK.search(url)
    slug = url.rstrip("/").split("/")[-2] if m else ""
    return slug.replace("-", " ").capitalize()


def gather(cache_dir, now, keys=None, stories=None):
    """Fetch the sources (keys, default all), their articles and photos.
    Returns {src: {"ok": bool, "items": [...]}}; a source not in keys is absent."""
    f = Fetcher(cache_dir)
    data = {}
    stories = stories or STORIES
    for key, _tab, _name, kind, url in SOURCES:
        if keys is not None and key not in keys:
            continue
        try:
            raw = f.get(url).decode("utf-8", "replace")
            # Eyre: a few spare candidates, because its video pages have no text and are dropped below
            items = parse_rss(raw, STORIES[key]) if kind == "rss" else parse_eyre(raw, STORIES[key] + 4)
            data[key] = {"ok": bool(items), "items": items}
        except Exception as e:                                   # one dead source must not kill the rest
            print(f"news: {key} unavailable: {e}", file=sys.stderr)
            data[key] = {"ok": False, "items": []}

    def enrich(it):
        try:
            art = parse_article(f.get(it["link"], ARTICLE_TTL).decode("utf-8", "replace"))
        except Exception as e:
            print(f"news: article failed {it['link']}: {e}", file=sys.stderr)
            art = {}
        it["paras"] = art.get("paras") or []
        it["title"] = it["title"] or art.get("title") or slug_title(it["link"])
        it["summary"] = it["summary"] or art.get("summary", "")
        it["pub"] = it["pub"] or art.get("pub")
        if not it["pub"] and it.get("date"):
            it["pub"] = dt.datetime.combine(it["date"], dt.time(12), TZ)
        it["img"] = it["img"] or art.get("img")
        it["photo"] = None
        if it["img"]:
            try:
                im = Image.open(io.BytesIO(f.get(small_image_url(it["img"]), IMAGE_TTL)))
                if im.width * im.height > MAX_PHOTO_PIXELS:
                    raise ValueError(f"{im.width}x{im.height} is too big")
                it["photo"] = im.convert("RGB")
            except Exception as e:
                print(f"news: photo failed {it['link']}: {e}", file=sys.stderr)
        return it

    allitems = [it for d in data.values() for it in d["items"]]
    with ThreadPoolExecutor(WORKERS) as ex:
        list(ex.map(enrich, allitems))
    for key, _tab, _name, kind, _url in SOURCES:
        if kind == "eyre" and key in data and data[key]["ok"]:              # local stories with real text first, then cap
            items = [it for it in data[key]["items"] if it["paras"]][:stories[key]]
            data[key]["items"] = items or data[key]["items"][:stories[key]]
    data["_fetches"] = f.count
    return data


# ---------------------------------------------------------------- drawing helpers
def grad(img, box, top, bot):
    d = ImageDraw.Draw(img)
    x0, y0, x1, y1 = box
    n = max(1, y1 - y0)
    for i in range(n):
        t = i / max(1, n - 1)
        d.line([(x0, y0 + i), (x1 - 1, y0 + i)], fill=tuple(int(a + (b - a) * t) for a, b in zip(top, bot)))


def wrap(text, fnt, width):
    """Greedy word wrap by pixel width; a word wider than the line is split by character."""
    lines, line = [], ""
    for word in text.split():
        t = f"{line} {word}" if line else word
        if fnt.getlength(t) <= width:
            line = t
            continue
        if line:
            lines.append(line)
        while fnt.getlength(word) > width:                       # very long URL-ish words
            k = len(word)
            while k > 1 and fnt.getlength(word[:k]) > width:
                k -= 1
            lines.append(word[:k])
            word = word[k:]
        line = word
    if line:
        lines.append(line)
    return lines


def ellipsize(lines, n, fnt, width):
    """At most n lines; if cut, the last one ends in an ellipsis that fits."""
    if len(lines) <= n:
        return lines
    out = lines[:n]
    last = out[-1]
    while last and fnt.getlength(last + "…") > width:
        last = last[:-1].rstrip()
    out[-1] = last + "…"
    return out


def when(pub, now):
    """When a story was published, as text that changes only when the data does (or at
    midnight): '4:12 pm' today, 'Yesterday', 'Tue' this week, else '21 Oct'."""
    if not pub:
        return ""
    p, n = pub.astimezone(TZ), now.astimezone(TZ)
    days = (n.date() - p.date()).days
    if days <= 0:
        return p.strftime("%-I:%M %p").lower()
    if days == 1:
        return "Yesterday"
    if days < 7:
        return p.strftime("%a")
    return p.strftime("%-d %b")


def crop_to(img, w, h):
    """Centre-crop to the w:h aspect, then resize."""
    sw, sh = img.size
    if sw * h > sh * w:
        nw = sh * w // h
        img = img.crop(((sw - nw) // 2, 0, (sw - nw) // 2 + nw, sh))
    else:
        nh = sw * h // w
        img = img.crop((0, (sh - nh) // 2, sw, (sh - nh) // 2 + nh))
    return img.resize((w, h), Image.LANCZOS)


def rounded(img, box, r, fill, outline=None):
    """Anti-aliased rounded rectangle: drawn at SS x and pasted down."""
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    m = Image.new("L", (w * SS, h * SS), 0)
    ImageDraw.Draw(m).rounded_rectangle([0, 0, w * SS - 1, h * SS - 1], radius=r * SS, fill=255)
    m = m.resize((w, h), Image.LANCZOS)
    if isinstance(fill, tuple) and len(fill) == 2 and isinstance(fill[0], tuple):
        g = Image.new("RGB", (w, h))
        grad(g, (0, 0, w, h), *fill)
    else:
        g = Image.new("RGB", (w, h), fill)
    if outline:
        o = Image.new("L", (w * SS, h * SS), 0)
        ImageDraw.Draw(o).rounded_rectangle([0, 0, w * SS - 1, h * SS - 1], radius=r * SS, outline=255, width=SS)
        o = o.resize((w, h), Image.LANCZOS)
        g.paste(Image.new("RGB", (w, h), outline), (0, 0), o)
    img.paste(g, (x0, y0), m)


def button(img, box, label, T, enabled=True, arrow=None, fnt=None):
    """A glossy 2008-style pill in the ABC blue."""
    rounded(img, box, (box[3] - box[1]) // 2, T["btn"] if enabled else T["btn_off"], outline=T["foot_line"])
    x0, y0, x1, y1 = box
    gloss = Image.new("RGBA", (x1 - x0, (y1 - y0) // 2), (255, 255, 255, 0))
    ImageDraw.Draw(gloss).rounded_rectangle([3, 1, x1 - x0 - 4, (y1 - y0) // 2], radius=(y1 - y0) // 3,
                                            fill=(255, 255, 255, 46 if enabled else 20))
    img.paste(gloss, (x0, y0 + 1), gloss)
    d = ImageDraw.Draw(img)
    ink = T["btn_ink"] if enabled else T["label"]
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    if arrow:
        _tri(img, cx, cy, 6, arrow, ink)
    if label:
        d.text((cx, cy), label, font=fnt or font(BODYB, 9), fill=ink, anchor="mm")


def _tri(img, cx, cy, s, kind, col):
    big = Image.new("L", (s * 2 * SS, s * 2 * SS), 0)
    q = s * SS
    pts = {"up": [(0, 1.6 * q), (q, 0.3 * q), (2 * q, 1.6 * q)], "down": [(0, 0.4 * q), (q, 1.7 * q), (2 * q, 0.4 * q)],
           "left": [(1.6 * q, 0), (0.3 * q, q), (1.6 * q, 2 * q)], "right": [(0.4 * q, 0), (1.7 * q, q), (0.4 * q, 2 * q)]}[kind]
    ImageDraw.Draw(big).polygon(pts, fill=255)
    m = big.resize((s * 2, s * 2), Image.LANCZOS)
    img.paste(Image.new("RGB", m.size, col), (int(cx - s), int(cy - s)), m)


# ---------------------------------------------------------------- ABC branding
_assets = {}


def asset(name):
    """An RGBA asset from the asset directories, else our placeholder of the same size."""
    if name not in _assets:
        path = wxassets.find(name)
        _assets[name] = Image.open(path).convert("RGBA") if path else placeholder(name)
    return _assets[name]


def placeholder(name):
    """Stand-ins of our own, the size of the files they replace: the logo is a round mark
    (the left 64 x 50, which the lists use alone) and NEWS; the swirls are faint arcs."""
    if name == "abc-swirls.png":
        im = Image.new("RGBA", (545 * SS, 81 * SS), (0, 0, 0, 0))
        d = ImageDraw.Draw(im)
        for k in range(5):
            r = (120 + k * 46) * SS
            d.arc([(420 * SS) - r, (190 * SS) - r, (420 * SS) + r, (190 * SS) + r], 200, 340,
                  fill=(255, 255, 255, 120 - k * 18), width=3 * SS)
        return im.resize((545, 81), Image.LANCZOS)
    im = Image.new("RGBA", (152 * SS, 50 * SS), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    for k, r in enumerate((21, 14, 7)):
        d.ellipse([(32 - r) * SS, (25 - r) * SS, (32 + r) * SS, (25 + r) * SS], outline=(255, 255, 255, 255),
                  width=(3 if k else 4) * SS)
    d.text((68 * SS, 25 * SS), "NEWS", font=font(HEAD, 26 * SS), fill=(255, 255, 255, 255), anchor="lm")
    return im.resize((152, 50), Image.LANCZOS)


def header(T, height=HDR):
    """The ABC banner as a 320 x height band: its gradient, a faint swirl, the logo."""
    img = Image.new("RGB", (W, height))
    grad(img, (0, 0, W, height), *T["hdr"])
    sw = asset("abc-swirls.png")
    sw = sw.resize((round(sw.width * height / sw.height), height), Image.LANCZOS)
    a = sw.getchannel("A").point(lambda v: int(v * T["swirl"]))
    img.paste(sw.convert("RGB"), (W - sw.width + 40, 0), a)
    lg = asset("abc-logo.png")
    lh = height - 12
    lg = lg.resize((round(lg.width * lh / lg.height), lh), Image.LANCZOS)
    img.paste(lg.convert("RGB"), (8, 6), lg.getchannel("A").point(lambda v: int(v * T["logo"])))
    return img


def make_assets(banner_path, out_dir="."):
    """Cut the ABC lockup and the light-trail swirls out of the classic abc.net.au banner
    (990x81, white logo + trails on a teal-to-deep-blue gradient) as RGBA PNGs.
    Alpha comes from colour: the background is saturated blue (R ~0), the logo and trails
    are neutral greys/whites, so alpha = 1 - blueness / background blueness."""
    src = Image.open(banner_path).convert("RGB")
    bw = 745                                                     # the banner proper; the rest is canvas

    def cut(box, bg_x):
        x0, y0, x1, y1 = box
        out = Image.new("RGBA", (x1 - x0, y1 - y0))
        for y in range(y0, y1):
            bg = src.getpixel((bg_x, y))
            bb = max(1, bg[2] - bg[0])
            for x in range(x0, x1):
                p = src.getpixel((x, y))
                a = max(0.0, min(1.0, 1 - (p[2] - p[0]) / bb))
                if a < 0.04:
                    out.putpixel((x - x0, y - y0), (0, 0, 0, 0))
                    continue
                fg = tuple(max(0, min(255, round((p[i] - (1 - a) * bg[i]) / a))) for i in range(3))
                out.putpixel((x - x0, y - y0), (*fg, round(a * 255)))
        return out

    logo = cut((32, 17, 184, 67), 25)
    logo.save(f"{out_dir}/abc-logo.png")
    cut((200, 0, bw, 81), 25).save(f"{out_dir}/abc-swirls.png")
    # a ready-made light header, for reference and the contact sheet
    _assets.clear()
    header(LIGHT).save(f"{out_dir}/abc-header.png")
    print("wrote abc-logo.png, abc-swirls.png, abc-header.png")


# ---------------------------------------------------------------- screens
def tab_geom(srcs):
    """x and width of each tab: up to three in the space after the logo."""
    tw = min(TAB_W, (W - TAB_X0) // max(1, len(srcs)))
    x0 = W - tw * len(srcs)
    return [(x0 + i * tw, tw) for i in range(len(srcs))]


def tabs(img, T, sel, srcs):
    d = ImageDraw.Draw(img)
    fnt = font(HEAD, 11)
    for (key, label, *_rest), (tx, tw) in zip(srcs, tab_geom(srcs)):
        x0 = tx + 2
        x1 = x0 + tw - 4
        if key == sel:
            rounded(img, (x0, 8, x1, HDR + 8), 6, T["sel"])
            d.text(((x0 + x1) / 2, 22), label, font=fnt, fill=T["sel_text"], anchor="mm")
        else:
            box = Image.new("RGBA", (x1 - x0, HDR - 12), (*T["tab"], 150))
            m = Image.new("L", ((x1 - x0) * SS, (HDR - 12 + 8) * SS), 0)
            ImageDraw.Draw(m).rounded_rectangle([0, 0, m.width - 1, m.height - 1], radius=6 * SS, fill=150)
            m = m.resize((x1 - x0, HDR - 12 + 8), Image.LANCZOS).crop((0, 0, x1 - x0, HDR - 12))
            img.paste(Image.new("RGB", box.size, T["tab"]), (x0, 12), m)
            d.text(((x0 + x1) / 2, 23), label, font=fnt, fill=T["tab_text"], anchor="mm")


def list_footer(img, T, k, npages, n):
    grad(img, (0, FOOT, W, H), *T["foot"])
    d = ImageDraw.Draw(img)
    d.line([(0, FOOT), (W, FOOT)], fill=T["foot_line"])
    button(img, (6, FOOT + 4, 50, H - 4), None, T, enabled=k > 0, arrow="up")
    button(img, (56, FOOT + 4, 100, H - 4), None, T, enabled=k < npages - 1, arrow="down")
    if n:
        a, b = k * PER_LIST + 1, min(n, (k + 1) * PER_LIST)
        d.text((110, FOOT + 17), f"{a}–{b} of {n}", font=font(BODYB, 10), fill=T["label"], anchor="lm")
    button(img, (254, FOOT + 6, 314, H - 6), "DONE", T)


def list_screen(src, k, items, T, now, ok, sfx, story_of, srcs=None, bad_msg=None):
    srcs = srcs or SOURCES
    img = Image.new("RGB", (W, H), T["bg"])
    img.paste(header(T), (0, 0))
    tabs(img, T, src, srcs)
    d = ImageDraw.Draw(img)
    hf, af = font(NARROW, 12), font(BODY, 8)
    page_items = items[k * PER_LIST:(k + 1) * PER_LIST]
    npages = max(1, (len(items) + PER_LIST - 1) // PER_LIST)
    lines = []
    name = f"news_{src}_l{k}{sfx}"
    if not ok:
        long_name = next(s[2] for s in SOURCES if s[0] == src)
        d.text((W / 2, 100), f"{long_name} is unavailable", font=font(HEAD, 13), fill=T["msg"], anchor="mm")
        for j, ln in enumerate(wrap(bad_msg or "Couldn't reach abc.net.au. It will try again at the next update, "
                                    "in about 10 minutes.", font(BODY, 9), 260)):
            d.text((W / 2, 124 + j * 14), ln, font=font(BODY, 9), fill=T["age"], anchor="mm")
    for r, it in enumerate(page_items):
        y = HDR + r * ROW
        if r:
            d.line([(8, y), (W - 8, y)], fill=T["sep"])
        tx = 8
        if it.get("photo") is not None:
            th = crop_to(it["photo"], 44, 30)
            d.rectangle([7, y + 1, 52, y + 32], fill=T["thumb_edge"])
            img.paste(th, (8, y + 2))
        else:
            d.rectangle([8, y + 2, 51, y + 31], fill=T["none"])
            img.paste(_tinted_logo(T, 30), (15, y + 9), _tinted_logo(T, 30))
        tx = 60
        aw = 46
        hl = ellipsize(wrap(it["title"], hf, W - tx - aw - 6), 2, hf, W - tx - aw - 6)
        ty = y + ROW / 2 - (len(hl) * 14) / 2
        for j, ln in enumerate(hl):
            d.text((tx, ty + j * 14 + 1), ln, font=hf, fill=T["head"])
        d.text((W - 8, y + ROW / 2), when(it.get("pub"), now), font=af, fill=T["age"], anchor="rm")
        si = k * PER_LIST + r
        if si in story_of:
            lines.append(f"hot {name} 0 {y} {W} {ROW} screen {story_of[si]}")
    list_footer(img, T, k, npages, len(items))
    # tabs, then the footer. The logo and DONE go back to the slideshow: News is a slide app
    geom = tab_geom(srcs)
    for (key, *_r), (tx, tw) in zip(srcs, geom):
        if key != src or k:
            lines.append(f"hot {name} {tx} 0 {tw} {HDR + 4} screen news_{key}_l0")
    lines.append(f"hot {name} 0 0 {geom[0][0]} {HDR} slideshow")
    if k > 0:
        lines.append(f"hot {name} 0 {FOOT} 53 {H - FOOT} screen news_{src}_l{k - 1}")
    if k < npages - 1:
        lines.append(f"hot {name} 53 {FOOT} 53 {H - FOOT} screen news_{src}_l{k + 1}")
    lines.append(f"hot {name} 244 {FOOT} 76 {H - FOOT} slideshow")
    return img, lines


_tint = {}


def _tinted_logo(T, w):
    key = (id(T), w)
    if key not in _tint:
        lg = asset("abc-logo.png").crop((0, 0, 64, 50))           # just the lissajous mark
        lg = lg.resize((w, round(lg.height * w / lg.width)), Image.LANCZOS)
        col = Image.new("RGBA", lg.size, (*T["thumb_edge"], 0))
        col.putalpha(lg.getchannel("A"))
        _tint[key] = col
    return _tint[key]


def story_chrome(S, T):
    """The story pages' shared background: header with BACK, footer with PREV / NEXT."""
    img = Image.new("RGB", (W, H))
    grad(img, (0, 0, W, H), *S["bg"])
    img.paste(header(T), (0, 0))
    button(img, (TAB_X0 + 2 + 2 * TAB_W - 30, 6, W - 6, HDR - 6), "BACK TO LIST", T, fnt=font(BODYB, 8))
    grad(img, (0, FOOT, W, H), *T["foot"])
    d = ImageDraw.Draw(img)
    d.line([(0, FOOT), (W, FOOT)], fill=T["foot_line"])
    button(img, (6, FOOT + 4, 96, H - 4), "  PREV", T)
    _tri(img, 22, FOOT + 17, 5, "left", T["btn_ink"])
    button(img, (W - 96, FOOT + 4, W - 6, H - 4), "NEXT  ", T)
    _tri(img, W - 22, FOOT + 17, 5, "right", T["btn_ink"])
    d.text((W / 2, FOOT + 17), "abc.net.au/news", font=font(BODY, 8), fill=T["label"], anchor="mm")
    return img


# story page text layout (inside the 320 x 170 pane)
BF, BL = 10, 13                                   # body font size, line height
PX0, PX1 = 10, 310
PHOTO_W, PHOTO_H = 128, 72


def layout_story(it, src_name, now, max_pages):
    """Paginate a story into pages of draw ops. Returns (pages, truncated)."""
    bf, hb = font(BODY, BF), font(BODYB, BF)
    tf = font(HEAD, 13)
    paras = it["paras"] or ([("p", it["summary"])] if it["summary"] else [("p", "No text available.")])
    # page 1: meta line, photo + headline beside it, then body under the photo
    pages, ops = [], []
    pub = it.get("pub")
    stamp = pub.astimezone(TZ).strftime("%a %-d %b, %-I:%M %p").replace("AM", "am").replace("PM", "pm") if pub else ""
    meta = " · ".join(x for x in (src_name, stamp) if x)
    y = 4
    ops.append(("meta", PX0, y, meta))
    y += 14
    photo = it.get("photo")
    if photo is not None:
        ops.append(("photo", PX0, y))
        hx, hw = PX0 + PHOTO_W + 8, PX1 - PX0 - PHOTO_W - 8
    else:
        hx, hw = PX0, PX1 - PX0
    hl = wrap(it["title"], tf, hw)
    max_hl = 5 if photo is not None else 4
    hl = ellipsize(hl, max_hl, tf, hw)
    for j, ln in enumerate(hl):
        ops.append(("title", hx, y + j * 15, ln))
    y = max(y + (PHOTO_H if photo is not None else 0), y + len(hl) * 15) + 6
    bottom = PANE_H - 4

    # flatten paragraphs to lines
    flow = []                                     # (kind, text) lines, "gap" between paragraphs
    for kind, text in paras:
        f = hb if kind == "h" else bf
        for ln in wrap(text, f, PX1 - PX0):
            flow.append((kind, ln))
        flow.append(("gap", ""))
    while flow and flow[-1][0] == "gap":
        flow.pop()
    i = 0
    truncated = False
    while i < len(flow):
        if y + BL > bottom:                       # page full
            pages.append(ops)
            if len(pages) >= max_pages:
                truncated = True
                break
            ops, y = [("meta", PX0, 4, meta)], 18
        kind, text = flow[i]
        if kind == "gap":
            if y > 22:
                y += 5
        else:
            ops.append(("h" if kind == "h" else "p", PX0, y, text))
            y += BL
        i += 1
    else:
        pages.append(ops)
    if truncated:                                 # last kept page: make room for the "continues" line
        last = pages[-1]
        while last and last[-1][0] in ("p", "h") and last[-1][2] + BL > bottom - BL:
            last.pop()
        last.append(("more", PX0, bottom - BL, "Story continues at abc.net.au …"))
    return pages, truncated


def draw_story_page(ops, it, S, page, npages):
    pane = Image.new("RGB", (W, PANE_H))
    grad(pane, (0, 0, W, PANE_H), *_slice(S["bg"], PANE_Y, PANE_Y + PANE_H))
    d = ImageDraw.Draw(pane)
    fonts = {"meta": font(BODYB, 8), "title": font(HEAD, 13), "p": font(BODY, BF), "h": font(BODYB, BF),
             "more": font(BODYB, 9)}
    cols = {"meta": S["meta"], "title": S["head"], "p": S["text"], "h": S["sub"], "more": S["more"]}
    for op in ops:
        if op[0] == "photo":
            x, y = op[1], op[2]
            d.rectangle([x - 1, y - 1, x + PHOTO_W, y + PHOTO_H], fill=S["photo_edge"])
            pane.paste(crop_to(it["photo"], PHOTO_W, PHOTO_H), (x, y))
        else:
            kind, x, y, text = op
            d.text((x, y), text, font=fonts[kind], fill=cols[kind])
    d.text((PX1, 4), f"{page + 1} / {npages}", font=fonts["meta"], fill=S["meta"], anchor="ra")
    return pane


def _slice(pair, y0, y1):
    """The colours of a full-screen vertical gradient between rows y0 and y1."""
    top, bot = pair
    at = lambda y: tuple(int(a + (b - a) * y / (H - 1)) for a, b in zip(top, bot))
    return at(y0), at(y1 - 1)


def active(data):
    """The SOURCES rows present in data, in tab order."""
    return [row for row in SOURCES if row[0] in data]


def allocate(data):
    """Story slots in reading order and how many pages each gets, within the budget."""
    order = []
    keys = [row[0] for row in active(data)]
    ranks = max((len(data[k]["items"]) for k in keys), default=0)
    for r in range(ranks):                         # interleave: nat0 adl0 eyre0 nat1 ...
        for key in keys:
            if r < len(data[key]["items"]):
                order.append((key, r))
    budget = STORY_PAGE_BUDGET
    alloc = {}
    for slot in order:                             # everyone gets a first page while it lasts
        if budget <= 0:
            break
        alloc[slot] = 1
        budget -= 1
    return order, alloc, budget


def _put(rgb565, img, out, name):
    """Stable slot names, written only when the pixels change (delta delivery): config's
    write_if_changed when it exists, else the caller's rgb565 (wxfeed's also skips identical
    files). The .png preview is rewritten only alongside its .raw."""
    try:
        import config
        wrote = config.write_if_changed(rgb565, img, f"{out}/{name}.raw")
    except (ImportError, AttributeError):
        rgb565(img, f"{out}/{name}.raw")
        wrote = True
    if wrote or not os.path.exists(f"{out}/{name}.png"):
        img.save(f"{out}/{name}.png")


def _cache_dir(out):
    try:
        import config
        d = os.path.join(config.state_dir(), "news-cache")
        os.makedirs(d, exist_ok=True)
        if os.access(d, os.W_OK):
            return d
    except Exception:
        pass
    return os.path.join(out, ".newscache")


def _empty(keys):
    return {k: {"ok": False, "items": []} for k in keys}


def render(out, rgb565, now=None, data=None, cache_dir=None, keys=None, stories=None):
    """Render the reader's screens (lists + story pages) into out; return manifest lines.
    Never raises: on any failure the sources fall back to an 'unavailable' list screen."""
    now = now or dt.datetime.now(TZ)
    keys = keys or [row[0] for row in SOURCES]
    try:
        if data is None:
            data = gather(cache_dir or _cache_dir(out), now, keys, stories)
        return _render(out, rgb565, now, data)
    except Exception as e:                         # last resort: keep the slides' taps landing somewhere
        import traceback
        traceback.print_exc()
        print(f"news: render failed: {e}", file=sys.stderr)
        return _render(out, rgb565, now, _empty(keys))


def _render(out, rgb565, now, data):
    lines = []
    srcs = active(data)
    order, alloc, spare = allocate(data)
    names = {k: n for k, _t, n, *_ in SOURCES}
    # lay out stories first (page counts decide the extra-page round robin)
    layouts = {}
    for slot in alloc:
        key, r = slot
        layouts[slot] = layout_story(data[key]["items"][r], names[key], now, 99)[0]
    while spare > 0:                               # spare pages, round-robin to stories that need them
        gave = False
        for slot in alloc:
            if spare > 0 and alloc[slot] < len(layouts[slot]):
                alloc[slot] += 1
                spare -= 1
                gave = True
        if not gave:
            break
    story_names = {slot: f"news_{slot[0]}_s{slot[1]}_p0" for slot in alloc}

    themes = [("", LIGHT), ("_night", NIGHT)]
    # single-theme story pages: the banner on top, a footer as dark as the page (night-friendly)
    dark_foot = dict(LIGHT, foot=((0, 34, 60), (0, 20, 38)), foot_line=(20, 70, 105), label=(110, 160, 190))
    story_themes = [("", STORY_LIGHT, LIGHT) if STORY_NIGHT else ("", STORY, dark_foot)]
    if STORY_NIGHT:
        story_themes.append(("_night", STORY, NIGHT))
    # list screens
    for key, *_ in srcs:
        d = data[key]
        items = d["items"]
        npages = max(1, (len(items) + PER_LIST - 1) // PER_LIST)
        story_of = {r: story_names[(key, r)] for r in range(len(items)) if (key, r) in story_names}
        for sfx, T in themes:
            for k in range(npages):
                img, hl = list_screen(key, k, items, T, now, d["ok"], sfx, story_of, srcs)
                name = f"news_{key}_l{k}{sfx}"
                _put(rgb565, img, out, name)
                lines.append(f"screen {name} {name}.raw")
                lines += hl
    # story screens
    for sfx, S, T in story_themes:
        chrome = story_chrome(S, T)
        cname = f"news_story_bg{sfx}"
        if SHARED_CHROME:
            _put(rgb565, chrome, out, cname)
        for slot in order:
            if slot not in alloc:
                continue
            key, r = slot
            it = data[key]["items"][r]
            pages, _trunc = layout_story(it, names[key], now, alloc[slot])
            list_name = f"news_{key}_l{r // PER_LIST}"
            nxt = order[order.index(slot) + 1:]
            nxt = next((s for s in nxt if s[0] == key and s in alloc), None)
            for j, ops in enumerate(pages):
                name = f"news_{key}_s{r}_p{j}{sfx}"
                pane = draw_story_page(ops, it, S, j, len(pages))
                if SHARED_CHROME:
                    _put(rgb565, pane, out, name)
                    lines.append(f"screen {name} {cname}.raw")
                    lines.append(f"pane {name} {name}.raw {W} {PANE_H} 0 {PANE_Y} night 1")
                else:
                    full = chrome.copy()
                    full.paste(pane, (0, PANE_Y))
                    _put(rgb565, full, out, name)
                    lines.append(f"screen {name} {name}.raw")
                prev = f"news_{key}_s{r}_p{j - 1}" if j else list_name
                if j < len(pages) - 1:
                    nx = f"news_{key}_s{r}_p{j + 1}"
                else:                              # last page: on to the next story from this source
                    nx = f"news_{nxt[0]}_s{nxt[1]}_p0" if nxt else list_name
                lines += [f"hot {name} 0 {FOOT} 110 {H - FOOT} screen {prev}",
                          f"hot {name} {W - 110} {FOOT} 110 {H - FOOT} screen {nx}",
                          f"hot {name} {TAB_X0 + 2 * TAB_W - 40} 0 {W - TAB_X0 - 2 * TAB_W + 40} {HDR + 4} screen {list_name}"]
    lines.append(f"# news: {sum(alloc.values())} story pages for {len(alloc)} stories, "
                 f"{data.get('_fetches', 0)} fetches")
    return lines


# ---------------------------------------------------------------- slideshow slides
SLIDE = {  # high contrast for the real 3.5" LCD (washed out, blue cast): bold, dark on white
    "light": dict(T=LIGHT, bg=(255, 255, 255), head=(6, 22, 56), sep=(196, 214, 228), foot=((0, 86, 128), (0, 64, 102)),
                  foot_text=(255, 255, 255), none=(214, 226, 236), edge=(150, 180, 200), msg=(6, 22, 56), sub=(40, 70, 100)),
    "night": dict(T=NIGHT, bg=(10, 20, 34), head=(240, 246, 252), sep=(36, 56, 78), foot=((0, 44, 70), (0, 30, 50)),
                  foot_text=(200, 230, 245), none=(26, 42, 58), edge=(50, 80, 105), msg=(240, 246, 252), sub=(150, 185, 210)),
}
S_HDR, S_FOOT = 40, 214
S_ROW = (S_FOOT - S_HDR - 2) // SLIDE_ROWS        # 57 px
THUMB_W, THUMB_H = 72, 54


def slide_image(key, d, theme="light"):
    """One source's slide: the ABC banner with the source name, its newest SLIDE_ROWS
    headlines (photo + 15 px bold), and a 'tap to read' strip. No times on it, so it only
    changes when the headlines do."""
    P = SLIDE[theme]
    T = P["T"]
    img = Image.new("RGB", (W, H), P["bg"])
    img.paste(header(T, S_HDR), (0, 0))
    dr = ImageDraw.Draw(img)
    label = {"nat": "Top Stories", "adl": "Adelaide", "eyre": "Eyre Peninsula"}[key]
    dr.text((W - 9, S_HDR / 2 + 1), label, font=font(HEAD, 18), fill=(0, 30, 50), anchor="rm")   # lifts it off the swirls
    dr.text((W - 10, S_HDR / 2), label, font=font(HEAD, 18), fill=(255, 255, 255), anchor="rm")
    hf = font(HEAD, 15)
    tx = 8 + THUMB_W + 10
    tw = W - tx - 8
    items = d["items"][:SLIDE_ROWS]
    if not d["ok"] or not items:
        name = next(r[2] for r in SOURCES if r[0] == key)
        dr.text((W / 2, 110), f"{name}", font=font(HEAD, 17), fill=P["msg"], anchor="mm")
        dr.text((W / 2, 136), "is unavailable right now", font=font(HEAD, 15), fill=P["sub"], anchor="mm")
        dr.text((W / 2, 162), "It will try again soon.", font=font(HEAD, 13), fill=P["sub"], anchor="mm")
    for r, it in enumerate(items):
        y = S_HDR + 2 + r * S_ROW
        if r:
            dr.line([(8, y), (W - 8, y)], fill=P["sep"])
        ty0 = y + (S_ROW - THUMB_H) // 2
        if it.get("photo") is not None:
            dr.rectangle([7, ty0 - 1, 8 + THUMB_W, ty0 + THUMB_H], fill=P["edge"])
            img.paste(crop_to(it["photo"], THUMB_W, THUMB_H), (8, ty0))
        else:
            dr.rectangle([8, ty0, 8 + THUMB_W - 1, ty0 + THUMB_H - 1], fill=P["none"])
            mark = _tinted_logo(dict(T, thumb_edge=P["edge"]), 44)
            img.paste(mark, (8 + (THUMB_W - mark.width) // 2, ty0 + (THUMB_H - mark.height) // 2), mark)
        hl = ellipsize(wrap(it["title"], hf, tw), 3, hf, tw)
        lh = 17
        yy = y + (S_ROW - len(hl) * lh) / 2
        for j, ln in enumerate(hl):
            dr.text((tx, yy + j * lh), ln, font=hf, fill=P["head"])
    grad(img, (0, S_FOOT, W, H), *P["foot"])
    dr.text((10, (S_FOOT + H) / 2), "ABC News", font=font(HEAD, 12), fill=P["foot_text"], anchor="lm")
    if d["ok"] and items:
        dr.text((W - 24, (S_FOOT + H) / 2), "Tap to read", font=font(HEAD, 13), fill=P["foot_text"], anchor="rm")
        _tri(img, W - 13, (S_FOOT + H) / 2, 5, "right", P["foot_text"])
    return img


def render_slides(out, first_page, rgb565, opts=None, slides=None, seconds=12, now=None, cache_dir=None,
                  night_files=False, screens_too=True):
    """The News app: one slideshow page per enabled source (sub-slide), each with a
    'pagetap PAGE news_<src>_l0' that opens that source's list, plus (screens_too) the
    reader's list and story screens, whose tabs are the enabled sources.
    slides: [(subslide_id, pages)] as config.plan() gave them (default: every source).
    Returns Rendered(lines, titles, thumbs), like stocks.render. Never raises."""
    now = now or dt.datetime.now(TZ)
    try:
        import config
        opts, _ = config.validate(CONFIG_SCHEMA, opts or {})
    except ImportError:
        opts = dict({f["key"]: f["default"] for f in CONFIG_SCHEMA}, **(opts or {}))
    if slides is None:
        slides = [(s["id"], 1) for s in APP["subslides"] if s["default"]]
    known = [row[0] for row in SOURCES]
    slides = [(sid, n) for sid, n in slides if sid in known and n > 0]
    keys = [k for k in known if k in {sid for sid, _ in slides}] or known
    n = int(opts.get("stories", 8))
    stories = {k: n for k in known}
    try:
        data = gather(cache_dir or _cache_dir(out), now, keys, stories)
    except Exception as e:
        print(f"news: gather failed: {e}", file=sys.stderr)
        data = _empty(keys)
    lines, titles, thumbs = [], [], []
    for k, (sid, _n) in enumerate(slides):
        p = first_page + k
        try:
            img = slide_image(sid, data.get(sid, {"ok": False, "items": []}))
        except Exception as e:                     # never lose the page: an unavailable slide instead
            print(f"news: slide {sid} failed: {e}", file=sys.stderr)
            img = slide_image(sid, {"ok": False, "items": []})
        name = f"news_slide_{sid}"
        _put(rgb565, img, out, name)
        lines.append(f"page {p} {name}.raw {seconds} noclock noticker")
        if night_files:                            # the dark slide wxshow draws in night mode
            try:
                nimg = slide_image(sid, data.get(sid, {"ok": False, "items": []}), "night")
            except Exception as e:
                print(f"news: night slide {sid} failed: {e}", file=sys.stderr)
                nimg = slide_image(sid, {"ok": False, "items": []}, "night")
            _put(rgb565, nimg, out, name + "_night")
            lines.append(f"pagenight {p} {name}_night.raw")
        lines.append(f"pagetap {p} news_{sid}_l0")         # engines without pagetap ignore it
        titles.append({"nat": "ABC News", "adl": "ABC Adelaide", "eyre": "ABC Eyre Peninsula"}[sid])
        thumbs.append(img)
    if screens_too:
        try:
            lines += _render(out, rgb565, now, data)
        except Exception as e:
            import traceback
            traceback.print_exc()
            print(f"news: reader failed: {e}", file=sys.stderr)
            lines += _render(out, rgb565, now, _empty(keys))
    return Rendered(lines, titles, thumbs)


if __name__ == "__main__":
    if len(sys.argv) in (3, 4) and sys.argv[1] == "assets":
        make_assets(*sys.argv[2:])
    else:
        print(__doc__)
