"""photos: the Photos app for wxshow. You upload pictures on the config page; each render
plays a few of them, full screen, in order or at random.

Two halves, so the 10-minute render stays cheap:

  ingest_image(src)   once per upload (the web page calls it). Decodes with Pillow (and
                      pillow-heif for iPhone HEIC, if installed), rejects anything that isn't
                      an image, applies the EXIF rotation, converts to sRGB, throws away ALL
                      metadata (GPS, camera, ICC), and stores a small JPEG preview plus a
                      thumbnail and a sidecar with the few facts the captions need.
  render(...)         every run. Picks N stored photos, fits each to 320x240 (crop to fill,
                      fit on a blurred copy of itself, or fit with black bars), optionally
                      captions it, and dithers it to RGB565 so skies and skin don't band.

Store (STORE_DIR): $NEOCHUMBY_WX_PHOTOS, else photos/ next to the config file
(in the app's state directory). Per photo, keyed by a random 16-hex id:
    <id>.jpg     preview, long edge ~640 (panoramas keep a 360 px short edge, up to 1920 long)
    <id>_t.jpg   thumbnail, long edge 240, for the web page's grid
    <id>.json    {"id", "name", "taken", "width", "height", "orig_width", "orig_height", "added"}
The config keeps the ordered id list (the image_library field); files not in it are
removed by delete_image() / prune().

Cursor and shuffle deck: photos-state.json next to the config file, so "in order"
carries on through the library across runs, and "random" shows every photo once before
any repeats.

    lines, titles, thumbs = photos.render(out, first_page, rgb565, opts, slides, seconds,
                                          config_url=cfg["general"]["config_url"])

Memory: every photo page is a full 153,600-byte frame that wxshow keeps in RAM (and in
/tmp), so N defaults to 4 and is capped at 8.

python3 photos.py --icon     redraws assets/icons/photos.png
"""
import datetime as dt
import io
import json
import os
import random
import re
import secrets
import struct
import sys
import tempfile
import warnings
from collections import namedtuple

from PIL import Image, ImageDraw, ImageFilter, ImageFont, ImageOps

try:                                     # iPhone HEIC; optional (pip install pillow-heif)
    import pillow_heif
    pillow_heif.register_heif_opener()
    HEIF = True
except Exception:                        # not installed, or no libheif: JPEG/PNG/... still work
    HEIF = False

W, H = 320, 240
HERE = os.path.dirname(os.path.abspath(__file__))
FD = HERE + "/assets/fonts/"
BOLD, REG = FD + "DejaVuSans-Bold.ttf", FD + "DejaVuSans.ttf"

ID_RE = re.compile(r"[0-9a-f]{16}")
MAX_PER_CYCLE = 8                        # THE cap. wxshow holds every page in RAM (153,600 bytes each);
                                         # raise it once the engine loads pages lazily
MAX_LIBRARY = 200
MAX_UPLOAD = 40 * 1024 * 1024            # bytes; a 48 MP iPhone JPEG/HEIC is ~10-25 MB
MAX_PIXELS = 120_000_000                 # decompression-bomb guard (6000x4000 = 24 MP)
PREVIEW_LONG, PREVIEW_SHORT, PREVIEW_MAX = 640, 360, 1920
THUMB_LONG = 240
DITHER = "fs"                            # "fs" (Floyd-Steinberg), "bayer" (ordered 8x8) or "none"

APP = {"id": "photos", "name": "Photos", "icon": "photos.png", "seconds": 10,
       "description": "Your own pictures. Upload them here and the Chumby plays a few each cycle, "
                      "resized and dithered for its little screen.",
       # one sub-slide only so config.plan() asks page_count() how many pages it needs
       "subslides": [{"id": "slideshow", "label": "Photo slideshow", "default": True}]}
CONFIG_SCHEMA = [
    {"key": "library", "type": "image_library", "label": "Photos", "default": [], "max_items": MAX_LIBRARY,
     "max_file_mb": MAX_UPLOAD // 1024 // 1024, "max_total_mb": 500, "accept": "image/*", "help": "JPEG, PNG, GIF, WebP" + (", HEIC" if HEIF else "") +
     ". They're shrunk for the Chumby when you upload them, and location data is removed."},
    {"key": "order", "type": "choice", "label": "Order", "default": "order",
     "choices": [["order", "In order"], ["random", "Random"]],
     "help": "In order carries on where the last cycle stopped. Random shows every photo once before repeating."},
    {"key": "per_cycle", "type": "number", "label": "Photos per cycle", "default": 4, "min": 1,
     "max": MAX_PER_CYCLE, "integer": True,
     "help": f"How many play each time round. Each one uses some of the Chumby's memory, so at most {MAX_PER_CYCLE}."},
    {"key": "change", "type": "choice", "label": "New photos", "default": "60",
     "choices": [["0", "Every refresh (10 minutes)"], ["60", "Every hour"], ["180", "Every 3 hours"],
                 ["1440", "Once a day"]],
     "help": "How often the set on the Chumby changes. Less often means less to send it."},
    {"key": "fit", "type": "choice", "label": "Fit", "default": "blur",
     "choices": [["crop", "Crop to fill the screen"], ["blur", "Fit, with a blurred background"],
                 ["bars", "Fit, with black bars"]]},
    {"key": "caption", "type": "bool", "label": "Show captions", "default": False,
     "help": "The photo's name, or the date it was taken."},
]

Rendered = namedtuple("Rendered", "lines titles thumbs")


# ---------------------------------------------------------------- store
def storage_dir(override=None):
    if override:
        return override
    if os.environ.get("NEOCHUMBY_WX_PHOTOS"):
        return os.environ["NEOCHUMBY_WX_PHOTOS"]
    import config                        # lazily: config imports the app modules
    return os.path.join(config.state_dir(), "photos")


def _check_id(pid):
    if not (isinstance(pid, str) and ID_RE.fullmatch(pid)):
        raise ValueError(f"bad photo id {pid!r}")
    return pid


def preview_path(pid, sdir=None):
    return os.path.join(storage_dir(sdir), _check_id(pid) + ".jpg")


def thumb_path(pid, sdir=None):
    return os.path.join(storage_dir(sdir), _check_id(pid) + "_t.jpg")


def image_info(pid, sdir=None):
    """The sidecar dict, or None if the photo isn't stored."""
    try:
        with open(os.path.join(storage_dir(sdir), _check_id(pid) + ".json")) as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return None


def exists(pid, sdir=None):
    try:
        return os.path.exists(preview_path(pid, sdir))
    except ValueError:
        return False


def delete_image(pid, sdir=None):
    """Remove a photo's files. Missing files are fine."""
    d = storage_dir(sdir)
    _check_id(pid)
    for sfx in (".jpg", "_t.jpg", ".json"):
        try:
            os.unlink(os.path.join(d, pid + sfx))
        except FileNotFoundError:
            pass


def prune(keep_ids, sdir=None):
    """Delete stored photos that aren't in keep_ids (e.g. after the library is edited). Returns the ids removed."""
    d, keep = storage_dir(sdir), set(keep_ids)
    gone = []
    try:
        names = os.listdir(d)
    except FileNotFoundError:
        return gone
    for n in names:
        m = re.fullmatch(r"([0-9a-f]{16})\.json", n)
        if m and m.group(1) not in keep:
            delete_image(m.group(1), d)
            gone.append(m.group(1))
    return gone


def _atomic_write(path, data):
    d = os.path.dirname(path)
    fd, tmp = tempfile.mkstemp(prefix=".photo.", dir=d)
    try:
        with os.fdopen(fd, "wb") as fh:
            fh.write(data)
        os.chmod(tmp, 0o644)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass
        raise


def _jpeg(img, quality):
    buf = io.BytesIO()
    img.save(buf, "JPEG", quality=quality, optimize=True)     # no exif=, icc_profile=: nothing carried over
    return buf.getvalue()


CAMERA_NAME = re.compile(r"(img|dsc[nf]?|pxl|mvimg|dcim|photo|image|screenshot|p)[\s_-]*\d.*"
                         r"|\d{8}[\s_-]?\d+.*|[0-9a-f-]{20,}|\d+", re.I)


def _clean_name(name):
    """A caption-worthy name from the uploaded file name, or "" for camera defaults (IMG_1234)."""
    if not name:
        return ""
    stem = os.path.splitext(os.path.basename(str(name)))[0]
    stem = re.sub(r"[\x00-\x1f\x7f]", "", stem).replace("_", " ").strip()[:60]
    return "" if not stem or CAMERA_NAME.fullmatch(stem.replace(" ", "_")) else stem


def _taken(exif):
    """'YYYY-MM-DD' from EXIF DateTimeOriginal (or DateTime), else None. The only EXIF kept."""
    try:
        v = exif.get_ifd(0x8769).get(36867) or exif.get(306)
        m = re.match(r"(\d{4}):(\d\d):(\d\d)", str(v or ""))
        if m and 1900 < int(m.group(1)) < 2200:
            return f"{m.group(1)}-{m.group(2)}-{m.group(3)}"
    except Exception:
        pass
    return None


def _to_srgb(img):
    """Convert by the embedded ICC profile (iPhones shoot Display P3), then forget it."""
    icc = img.info.get("icc_profile")
    if not icc or img.mode not in ("RGB", "RGBA"):
        return img
    try:
        from PIL import ImageCms
        src = ImageCms.ImageCmsProfile(io.BytesIO(icc))
        return ImageCms.profileToProfile(img, src, ImageCms.createProfile("sRGB"), outputMode=img.mode)
    except Exception:                    # no littlecms, or a broken profile: use the pixels as they are
        return img


def _preview_size(w, h):
    s = min(1.0, max(PREVIEW_LONG / max(w, h), PREVIEW_SHORT / min(w, h)))
    s = min(s, PREVIEW_MAX / max(w, h))
    return max(1, round(w * s)), max(1, round(h * s))


def ingest_image(src, name=None, sdir=None):
    """Store one uploaded image; returns a new random id. Raises ValueError("...") with a message
    fit for the user if it isn't a usable image. configweb never passes the client's file name;
    name= is only for local tools (it becomes the caption when it isn't a camera default)."""
    d = storage_dir(sdir)
    try:
        size = os.path.getsize(src)
    except OSError:
        raise ValueError("upload missing")
    if size == 0:
        raise ValueError("the file is empty")
    if size > MAX_UPLOAD:
        raise ValueError(f"too big (over {MAX_UPLOAD // 1024 // 1024} MB)")
    pid = secrets.token_hex(8)           # a fresh random id per upload (config.py's contract)
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", Image.DecompressionBombWarning)   # we apply our own limit
            img = Image.open(src)
            ow, oh = img.size
            if ow < 8 or oh < 8:
                raise ValueError("too small (under 8 pixels)")
            if ow * oh > MAX_PIXELS:
                raise ValueError(f"too many pixels ({ow}x{oh})")
            exif = img.getexif()
            orient = exif.get(0x0112, 1)
            pw, ph = _preview_size(*((oh, ow) if orient in (5, 6, 7, 8) else (ow, oh)))
            if img.format == "JPEG":     # let libjpeg decode at 1/2../1/8 scale: 6000 px in a fraction of the time
                img.draft("RGB", (pw, ph) if orient not in (5, 6, 7, 8) else (ph, pw))
            img.seek(0) if getattr(img, "n_frames", 1) > 1 else None          # animated: first frame
            img.load()
    except ValueError:
        raise
    except (OSError, SyntaxError, EOFError, Image.DecompressionBombError, TypeError, IndexError,
            struct.error) as e:
        raise ValueError("not an image Pillow can read" + ("" if HEIF else " (HEIC needs pillow-heif)")) from e
    taken = _taken(exif)
    img = ImageOps.exif_transpose(img)
    if img.mode in ("RGBA", "LA", "PA") or (img.mode == "P" and "transparency" in img.info):
        img = _to_srgb(img.convert("RGBA"))
        bg = Image.new("RGB", img.size, (0, 0, 0))
        bg.paste(img, (0, 0), img.getchannel("A"))
        img = bg
    elif img.mode in ("I;16", "I;16B", "I;16L", "I"):
        img = img.point(lambda v: v / 256).convert("L").convert("RGB")
    else:
        if img.mode != "RGB":
            img = img.convert("RGB")
        img = _to_srgb(img)
    pw, ph = _preview_size(*img.size)
    prev = img.resize((pw, ph), Image.LANCZOS, reducing_gap=3.0) if (pw, ph) != img.size else img.copy()
    thumb = prev.copy()
    thumb.thumbnail((THUMB_LONG, THUMB_LONG), Image.LANCZOS)
    prev.info.clear()
    thumb.info.clear()
    os.makedirs(d, exist_ok=True)
    meta = {"id": pid, "name": _clean_name(name), "taken": taken, "width": pw, "height": ph,
            "orig_width": img.size[0], "orig_height": img.size[1],
            "added": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")}
    _atomic_write(os.path.join(d, pid + ".jpg"), _jpeg(prev, 86))
    _atomic_write(os.path.join(d, pid + "_t.jpg"), _jpeg(thumb, 80))
    _atomic_write(os.path.join(d, pid + ".json"), json.dumps(meta).encode())   # last: marks it complete
    return pid


def set_caption(pid, text, sdir=None):
    """Optional: a hand-written caption (the web page may offer it). "" clears it."""
    meta = image_info(pid, sdir)
    if meta is None:
        raise ValueError("no such photo")
    meta["caption"] = re.sub(r"[\x00-\x1f\x7f]", "", str(text)).strip()[:80]
    _atomic_write(os.path.join(storage_dir(sdir), pid + ".json"), json.dumps(meta).encode())


def caption_text(meta):
    if not meta:
        return ""
    if meta.get("caption"):
        return meta["caption"]
    if meta.get("name"):
        return meta["name"]
    if meta.get("taken"):
        y, m, d = map(int, meta["taken"].split("-"))
        return dt.date(y, m, d).strftime("%-d %B %Y")
    return ""


# ---------------------------------------------------------------- library + picking
def library(opts, sdir=None):
    """Stored ids in the order the config lists them. Accepts ["id", ...] or [{"id": ...}, ...];
    drops bad and missing ids, keeps the first of any duplicate."""
    raw = (opts or {}).get("library") or []
    out, seen = [], set()
    for it in raw if isinstance(raw, list) else []:
        pid = it.get("id") if isinstance(it, dict) else it
        if isinstance(pid, str) and ID_RE.fullmatch(pid) and pid not in seen and exists(pid, sdir):
            seen.add(pid)
            out.append(pid)
    return out


def _per_cycle(opts):
    try:
        return max(1, min(MAX_PER_CYCLE, int(round(float((opts or {}).get("per_cycle", 4))))))
    except (TypeError, ValueError):
        return 4


def page_count(subslide, opts):
    """Pages the slideshow needs: N photos (no more than the library has), or 1 for the
    "add photos" page when the library is empty."""
    try:
        n = len(library(opts))
    except Exception:
        n = 0
    return min(_per_cycle(opts), n) if n else 1


def load_state(path):
    try:
        with open(path) as fh:
            st = json.load(fh)
        return st if isinstance(st, dict) else {}
    except (OSError, ValueError):
        return {}


def save_state(path, st):
    try:
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        _atomic_write(os.path.abspath(path), json.dumps(st).encode())
    except OSError as e:
        print(f"photos: can't save state {path}: {e}", file=sys.stderr)


def pick(lib, n, order, st, rng=random):
    """Choose this cycle's photos and update st in place.
    in order: n photos from the cursor (remembered as the next photo's id, so adding or
              removing photos elsewhere in the library doesn't make it jump), wrapping.
    random:   a shuffle deck; every photo is shown once before any repeats, and a new deck
              doesn't start with what was just on screen."""
    if not lib:
        return []
    n = min(n, len(lib))
    if order == "random":
        libset = set(lib)
        deck = [p for p in st.get("deck", []) if p in libset]
        out = []
        while len(out) < n:
            if not deck:
                deck = [p for p in lib if p not in out]
                rng.shuffle(deck)
                recent = set(st.get("last", []))            # last cycle's photos go to the back
                deck = [p for p in deck if p not in recent] + [p for p in deck if p in recent]
            out.append(deck.pop(0))
        st["deck"] = deck
    else:
        nxt = st.get("next")
        i = lib.index(nxt) if nxt in lib else int(st.get("cursor", 0) or 0) % len(lib)
        out = [lib[(i + k) % len(lib)] for k in range(n)]
        st["cursor"] = (i + n) % len(lib)
        st["next"] = lib[st["cursor"]]
        st.pop("deck", None)
    st["last"] = out
    return out


# ---------------------------------------------------------------- frames
def _contain(img, w, h, max_up=2.0):
    s = min(w / img.width, h / img.height, max_up)          # tiny images: at most 2x, so they stay sharp
    size = (max(1, round(img.width * s)), max(1, round(img.height * s)))
    return img.resize(size, Image.LANCZOS)


def fit_frame(img, fit):
    """A 320x240 RGB frame from a preview image."""
    img = img.convert("RGB")
    if fit == "crop":
        # slightly above centre: portraits keep heads, landscapes lose a little ground
        return ImageOps.fit(img, (W, H), Image.LANCZOS, centering=(0.5, 0.42))
    fg = _contain(img, W, H)
    if fit == "blur":
        bg = ImageOps.fit(img, (W // 4, H // 4), Image.BILINEAR)        # blur small: cheap and smoother
        bg = bg.filter(ImageFilter.GaussianBlur(4)).resize((W, H), Image.BICUBIC)
        bg = Image.blend(Image.new("RGB", (W, H), (0, 0, 0)), bg, 0.55)  # darker, so the photo stands out
        if fg.size == (W, H):
            return fg
        # a soft shadow under the photo
        sh = Image.new("L", (W, H), 0)
        x, y = (W - fg.width) // 2, (H - fg.height) // 2
        ImageDraw.Draw(sh).rectangle([x - 1, y + 1, x + fg.width + 1, y + fg.height + 3], fill=150)
        bg.paste((0, 0, 0), (0, 0), sh.filter(ImageFilter.GaussianBlur(4)))
    else:
        bg = Image.new("RGB", (W, H), (0, 0, 0))
    bg.paste(fg, ((W - fg.width) // 2, (H - fg.height) // 2))
    return bg


def add_caption(frame, text):
    """White text on a soft dark fade along the bottom edge."""
    if not text:
        return frame
    frame = frame.convert("RGBA")
    fnt = ImageFont.truetype(BOLD, 13)
    while fnt.getlength(text) > W - 20 and len(text) > 4:
        text = text[:-2].rstrip() + "…"
    fade = Image.new("L", (1, 46))
    for y in range(46):
        fade.putpixel((0, y), int(175 * (y / 45) ** 1.4))
    ov = Image.new("RGBA", (W, 46), (0, 0, 0, 255))
    ov.putalpha(fade.resize((W, 46)))
    frame.alpha_composite(ov, (0, H - 46))
    d = ImageDraw.Draw(frame)
    d.text((11, H - 9), text, font=fnt, fill=(0, 0, 0, 200), anchor="ls")
    d.text((10, H - 10), text, font=fnt, fill=(255, 255, 255), anchor="ls")
    return frame.convert("RGB")


# RGB565: what the screen can show of an 8-bit value with b bits
def _levels(bits):
    return [(k << (8 - bits)) | (k >> (2 * bits - 8)) for k in range(1 << bits)]


_LEVELS = {5: _levels(5), 6: _levels(6)}
_BAYER8 = None


def _bayer8():
    global _BAYER8
    if _BAYER8 is None:
        m = [[0]]
        while len(m) < 8:
            n = len(m)
            m = [[4 * m[y % n][x % n] + (0, 2, 3, 1)[(y // n) * 2 + (x // n)] for x in range(2 * n)] for y in range(2 * n)]
        _BAYER8 = m
    return _BAYER8


def quantise(img, mode=DITHER):
    """RGB -> RGB holding only RGB565-representable values (so the feeder's >>3/>>2 packer is
    exact). mode: "fs" error diffusion per channel (Pillow's C quantiser against a 32/64-level
    grey palette), "bayer" 8x8 ordered dither, "none" plain truncation (what rgb565() alone does)."""
    img = img.convert("RGB")
    out = []
    for ch, bits in zip(img.split(), (5, 6, 5)):
        lv = _LEVELS[bits]
        n = len(lv)
        if mode == "fs":
            pal = Image.new("P", (1, 1))
            flat = []
            for v in lv:
                flat += [v, v, v]
            pal.putpalette(flat + flat[-3:] * (256 - n))         # pad with the top level
            q = ch.convert("RGB").quantize(palette=pal, dither=Image.Dither.FLOYDSTEINBERG)
            lut = bytes(lv[min(i, n - 1)] for i in range(256))
            out.append(Image.frombytes("L", ch.size, q.tobytes().translate(lut)))
        elif mode == "bayer":
            step = 256 // n
            b = _bayer8()
            # threshold map scaled to one quantisation step, centred on 0 (offset 128)
            tile = Image.new("L", (8, 8))
            tile.putdata([128 + int((b[y][x] + 0.5) / 64 * step - step / 2) for y in range(8) for x in range(8)])
            noise = Image.new("L", ch.size)
            for y in range(0, ch.size[1], 8):
                for x in range(0, ch.size[0], 8):
                    noise.paste(tile, (x, y))
            from PIL import ImageChops
            moved = ImageChops.add(ch, noise, scale=1.0, offset=-128)
            near = [lv[min(n - 1, max(0, round((v - lv[0]) / 255 * (n - 1))))] for v in range(256)]
            out.append(moved.point(near))
        else:
            out.append(ch.point([lv[v >> (8 - bits)] for v in range(256)]))
    return Image.merge("RGB", out)


def photo_frame(pid, fit="blur", caption=False, dither=DITHER, sdir=None):
    """The quantised 320x240 RGB frame for one photo (deterministic: same inputs, same pixels)."""
    with Image.open(preview_path(pid, sdir)) as im:
        im.load()
        fr = fit_frame(im, fit)
    if caption:
        fr = add_caption(fr, caption_text(image_info(pid, sdir)))
    return quantise(fr, dither)


def pack565(img):
    """RGB -> RGB565 little-endian bytes, the same as wxfeed.rgb565() writes, but in C
    (Pillow channel ops) instead of a Python loop over 76,800 pixels."""
    r, g, b = img.convert("RGB").split()
    from PIL import ImageChops
    hi = ImageChops.add(r.point(lambda v: v & 0xF8), g.point(lambda v: v >> 5))
    lo = ImageChops.add(g.point(lambda v: ((v >> 2) & 7) << 5), b.point(lambda v: v >> 3))
    return Image.merge("LA", (lo, hi)).tobytes()


def unpack565(data, size=(W, H)):
    """RGB565 bytes -> RGB as the screen shows it (for PNG previews of rendered pages)."""
    lo = Image.frombytes("L", size, data[0::2])
    hi = Image.frombytes("L", size, data[1::2])
    r = hi.point(lambda v: (v & 0xF8) | (v >> 5))
    gh = hi.point(lambda v: (v & 7) << 5)
    gl = lo.point(lambda v: (v >> 5) << 2)
    from PIL import ImageChops
    g = ImageChops.add(gh, gl)
    g = g.point(lambda v: v | (v >> 6))
    b = lo.point(lambda v: ((v & 31) << 3) | ((v & 31) >> 2))
    return Image.merge("RGB", (r, g, b))


CACHE_VERSION = 1                        # bump when fit/caption/quantise output changes


def cached_frame(pid, fit, caption, dither, cache, sdir=None):
    """RGB565 bytes for one photo, from cache/ when the same photo, fit, caption text and
    dither were rendered before. Same inputs always give byte-identical output, so an
    unchanged slot never changes on disk (delivery sends only changed files)."""
    cap = caption_text(image_info(pid, sdir)) if caption else ""
    key = f"{pid}-{fit}-{dither}-{CACHE_VERSION}-" + (format(zlib_crc(cap), "08x") if cap else "nocap")
    path = os.path.join(cache, key + ".raw") if cache else None
    if path:
        try:
            with open(path, "rb") as fh:
                data = fh.read()
            if len(data) == W * H * 2:
                return data
        except OSError:
            pass
    data = pack565(photo_frame(pid, fit, caption, dither, sdir))
    if path:
        try:
            os.makedirs(cache, exist_ok=True)
            _atomic_write(path, data)
        except OSError as e:
            print(f"photos: can't cache {key}: {e}", file=sys.stderr)
    return data


def zlib_crc(text):
    import zlib
    return zlib.crc32(text.encode())


def prune_cache(cache, keep_ids):
    try:
        for n in os.listdir(cache):
            if n.endswith(".raw") and n[:16] not in keep_ids:
                os.unlink(os.path.join(cache, n))
    except OSError:
        pass


def write_if_changed(path, data):
    """True if it wrote. An unchanged file keeps its bytes and mtime; a changed one is
    written beside and renamed into place, so no reader sees half a file."""
    try:
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


def choose(lib, n, opts, st, now, rng=random):
    """This cycle's photos. The set only changes every opts["change"] minutes (or when the
    library, N or the order no longer match it), so most 10-minute renders reproduce the
    same pages byte for byte."""
    period = int(opts.get("change", "60")) * 60
    cur = [p for p in st.get("set", []) if p in lib]
    still_ok = (len(cur) == len(st.get("set", [])) == min(n, len(lib)) and st.get("order") == opts["order"])
    due = now - float(st.get("picked_at", 0) or 0) >= period - 90      # the timer drifts a little
    if still_ok and not due:
        return cur, False
    chosen = pick(lib, n, opts["order"], st, rng)
    st.update(set=chosen, order=opts["order"], picked_at=now)
    return chosen, True


# ---------------------------------------------------------------- the "add photos" page
def empty_page(config_url):
    """No photos yet: the 2008 panel look, a QR code for the config page, and its address."""
    import qrscreen
    import screens
    saved = screens.T
    screens.T = screens.LIGHT
    try:
        T, SS, f = screens.T, screens.SS, screens.f
        img, d = screens.panel_background("Photos", "none")
        draw_glyph(d, 12 * SS, 12 * SS, 18 * SS)
        box = (20, 50, 140, 170)
        d.rounded_rectangle([(box[0] - 4) * SS, (box[1] - 4) * SS, (box[2] + 4) * SS, (box[3] + 4) * SS],
                            radius=8 * SS, fill=(255, 255, 255), outline=T["frame1"], width=SS)
        tx = 158
        d.text((tx * SS, 52 * SS), "No photos yet", font=f(screens.VERDB, 12), fill=T["text"])
        for i, s in enumerate(["Scan the code, or", "visit the address", "below, and upload", "some pictures.",
                               "", "They'll play here", "after the next refresh."]):
            d.text((tx * SS, (74 + i * 13) * SS), s, font=f(screens.VERD, 8), fill=T["body"])
        shown = config_url.replace("http://", "").replace("https://", "").rstrip("/")
        fnt = f(screens.VERDB, 10)
        while fnt.getlength(shown) > 292 * SS and fnt.size > 6 * SS:
            fnt = f(screens.VERDB, fnt.size / SS - 0.5)
        d.text((W / 2 * SS, 196 * SS), "Add photos at", font=f(screens.VERD, 8), fill=T["hint"], anchor="ms")
        d.text((W / 2 * SS, 214 * SS), shown, font=fnt, fill=T["text"], anchor="ms")
        im = screens.small(img)
        qr, ok = qrscreen.qr_image(config_url, box[2] - box[0])
        if ok:
            im.paste(qr, box[:2])
        return im
    finally:
        screens.T = saved


def draw_glyph(d, cx, cy, s, col=None):
    """Two stacked photos (outline), for the page header."""
    import screens
    col = col or screens.T["text"]
    w = max(2, int(s * .09))
    d.rounded_rectangle([cx - s * .42, cy - s * .30, cx + s * .26, cy + s * .22], radius=s * .05, outline=col, width=w)
    d.rounded_rectangle([cx - s * .26, cy - s * .16, cx + s * .44, cy + s * .38], radius=s * .05, fill=screens.T["panel_bot"],
                        outline=col, width=w)
    d.polygon([(cx - s * .18, cy + s * .30), (cx + s * .02, cy + s * .04), (cx + s * .16, cy + s * .20),
               (cx + s * .24, cy + s * .12), (cx + s * .38, cy + s * .30)], fill=col)
    r = s * .06
    d.ellipse([cx + s * .22 - r, cy - s * .04 - r, cx + s * .22 + r, cy - s * .04 + r], fill=col)


# ---------------------------------------------------------------- feeder entry point
def render(out, first_page, rgb565=None, opts=None, slides=None, seconds=10, now=None, state_path=None,
           sdir=None, config_url=None, dither=DITHER, rng=random, previews=True):
    """Pick, draw and write this cycle's photo pages. Returns Rendered(lines, titles, thumbs).
    slides: [(subslide_id, pages)] as config.plan() cut them (caps N); default uncut.
    Files are stable per slot (photos_0.raw, photos_1.raw, ...) and only rewritten when their
    bytes change; slots beyond this cycle's count are removed. Pages play with no clock or
    ticker: the clock glyphs are pre-drawn on the weather header's colours and would sit on
    the photo. rgb565 is accepted for the common app signature but not needed (pack565).
    now: epoch seconds (default time.time())."""
    import time
    import config
    now = time.time() if now is None else (now.timestamp() if hasattr(now, "timestamp") else float(now))
    raw = opts or {}
    clean, _ = config.validate([f for f in CONFIG_SCHEMA if f["type"] != "image_library"], raw)
    lib = library(raw, sdir)
    n = _per_cycle(clean)
    if slides is not None:
        n = min(n, sum(k for sid, k in slides if sid == "slideshow"))
        if n <= 0:
            _clear_slots(out, 0)
            return Rendered([], [], [])
    sd = config.state_dir()
    if not os.access(sd, os.W_OK):
        sd = out
    state_path = state_path or os.path.join(sd, "photos-state.json")
    cache = os.path.join(os.path.dirname(os.path.abspath(state_path)), "photos-cache")
    frames = []                                                   # (bytes, title)
    if not lib:
        url = config_url or config.defaults(config.GENERAL_SCHEMA)["config_url"]
        frames.append((pack565(quantise(empty_page(url), dither)), "Photos: add some"))
    else:
        st = load_state(state_path)
        chosen, changed = choose(lib, n, clean, st, now, rng)
        if changed:
            save_state(state_path, st)
        for k, pid in enumerate(chosen):
            try:
                data = cached_frame(pid, clean["fit"], clean["caption"], dither, cache, sdir)
            except Exception as e:       # one bad file must not cost the whole slideshow
                print(f"photos: skipping {pid}: {e}", file=sys.stderr)
                continue
            frames.append((data, caption_text(image_info(pid, sdir)) or f"Photo {k + 1} of {len(chosen)}"))
        prune_cache(cache, set(lib))
    lines, titles, thumbs = [], [], []
    for k, (data, title) in enumerate(frames):
        name = f"photos_{k}"
        im = unpack565(data)
        if write_if_changed(f"{out}/{name}.raw", data) or (previews and not os.path.exists(f"{out}/{name}.png")):
            if previews:
                im.save(f"{out}/{name}.png")
        lines.append(f"page {first_page + k} {name}.raw {seconds} noclock noticker")
        titles.append(title)
        thumbs.append(im)
    _clear_slots(out, len(frames))
    return Rendered(lines, titles, thumbs)


def _clear_slots(out, keep):
    """Remove photos_K.raw/.png for K >= keep (a smaller N, or fewer photos than before)."""
    try:
        names = os.listdir(out)
    except OSError:
        return
    for nm in names:
        m = re.fullmatch(r"photos_(\d+)\.(raw|png)", nm)
        if m and int(m.group(1)) >= keep:
            try:
                os.unlink(os.path.join(out, nm))
            except OSError:
                pass


# ---------------------------------------------------------------- app icon
def make_icon(path=HERE + "/assets/icons/photos.png"):
    """80x80, the config page's chumby.com card style (see make_icons.py): a warm gradient,
    a stack of three tilted prints, the top one a sunny landscape, and the 2008 gloss."""
    import make_icons as mi
    Z, K = mi.Z, mi.K
    im = mi.vgrad((Z, Z), (255, 190, 90), (225, 85, 105)).convert("RGBA")

    def card(w, h, picture):
        c = Image.new("RGBA", (w, h), (0, 0, 0, 0))
        cd = ImageDraw.Draw(c)
        cd.rounded_rectangle([0, 0, w - 1, h - 1], radius=K * 2, fill=(255, 255, 255, 255), outline=(200, 200, 205, 255),
                             width=K)
        m = int(w * .09)
        ph = int(h * .74)
        if picture:
            sky = mi.vgrad((w - 2 * m, ph - m), (70, 150, 235), (175, 220, 250))
            sd = ImageDraw.Draw(sky)
            sw, shh = sky.size
            sd.ellipse([sw * .62, shh * .14, sw * .84, shh * .14 + sw * .22], fill=(255, 215, 60))
            sd.polygon([(0, shh), (0, shh * .62), (sw * .3, shh * .4), (sw * .55, shh * .7), (sw * .55, shh)], fill=(70, 160, 80))
            sd.polygon([(sw * .3, shh), (sw * .72, shh * .5), (sw, shh * .72), (sw, shh)], fill=(40, 125, 70))
            c.paste(sky, (m, m))
        else:
            cd.rectangle([m, m, w - m - 1, ph - 1], fill=(205, 215, 230, 255))
        return c

    cw, ch = int(Z * .62), int(Z * .62 * 1.08)
    for ang, dx, dy, pic in ((14, -.07, .02, False), (-9, .06, -.01, False), (3, 0, 0, True)):
        c = card(cw, ch, pic).rotate(ang, Image.BICUBIC, expand=True)
        sh = Image.new("RGBA", c.size, (0, 0, 0, 0))
        sh.putalpha(c.getchannel("A").point(lambda a: a * 0.45))
        sh = sh.filter(ImageFilter.GaussianBlur(K * 2.5))
        x, y = int(Z / 2 - c.width / 2 + dx * Z), int(Z / 2 - c.height / 2 + dy * Z)
        im.alpha_composite(sh, (x + K * 2, y + K * 3))
        im.alpha_composite(c, (x, y))
    out = mi.gloss(im.convert("RGB"), 40).resize((mi.S, mi.S), Image.LANCZOS)
    out.save(path, optimize=True)
    return out


if __name__ == "__main__":
    if "--icon" in sys.argv:
        make_icon()
        print("wrote assets/icons/photos.png")
    else:
        print(__doc__)
