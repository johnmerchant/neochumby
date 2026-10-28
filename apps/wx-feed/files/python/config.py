"""config: the feeder's settings file, and the registry of apps that can go in it.

The web page (configweb.py) writes it; wxfeed.py reads it at the start of every render.
Shaped like chumby.com's "my channels": a channel is an ordered line-up of apps. An app
is ONE card (Weather, Stock Quotes, ...) that shows one or more sub-slides
(weather: current / local / extended), each of which the user can switch off or reorder
in the app's "customize" dialog, plus the app's own options.

    {
      "version": 2,
      "channel": "default",                      # the channel the Chumby plays
      "channels": {
        "default": {
          "title": "Default",
          "apps": [                              # play order
            {"app": "weather", "enabled": true, "seconds": 12,
             "subslides": [{"id": "current", "enabled": true}, {"id": "local", "enabled": true},
                           {"id": "extended", "enabled": true}],
             "options": {}},
            {"app": "stocks", "enabled": true, "seconds": 12,
             "subslides": [{"id": "summary", "enabled": true}, {"id": "details", "enabled": true}],
             "options": {"tickers": ["BHP.AX", "TSLA"], ...}},
            {"app": "aurora", "enabled": true, "seconds": 24, "subslides": [], "options": {}}
          ]
        }
      },
      "general": {"config_url": "http://neochumbyd.local:8080/", "timezone": "Australia/Adelaide"}
    }

Only the "default" channel exists for now; more channels later is a data change.
"remove" on the web page sets enabled=false (options are kept); "add apps" sets it back.

An app module declares
    APP = {"id": "stocks", "name": "Stock Quotes", "icon": "stocks.png", "description": "...",
           "seconds": 12, "subslides": [{"id": "summary", "label": "Summary", "default": True}, ...]}
    CONFIG_SCHEMA = [fields]                # optional
    def page_count(subslide_id, options)    # optional: pages one sub-slide expands to (default 1);
                                            # subslide_id is None for an app without sub-slides
Fields: {"key", "type", "label", "default", "help"} with type list (of strings: "pattern",
"max_items", "upper"), string ("pattern", "max_len"), bool, choice ("choices": [[value,
label], ...]) or number ("min", "max", "integer"). The web page builds forms from these,
and validate() is the only gate between a form (or a hand-edited file) and the feeder.

IMAGE LIBRARIES (the interface the Photos app, feeder/photos.py, builds on; keep it stable):
    field   {"key": "photos", "type": "image_library", "label": "Photos", "default": [],
             "max_items": 200, "max_file_mb": 25, "max_total_mb": 500, "help": "..."}
    stored  options[key] = ordered list of image ids (str, [A-Za-z0-9_-]{1,64}); nothing else.
    hooks the module provides:
      ingest_image(src_path) -> id   called by configweb once per upload, with a temp file that
                                     Pillow has already decoded successfully (any format Pillow
                                     opens, HEIC too if pillow-heif is installed). The module does
                                     all the processing (EXIF orientation, strip EXIF/GPS, downscale,
                                     dithering), stores the result under storage_dir() and returns a
                                     fresh random id. The temp file is deleted afterwards, so copy
                                     what you need. Raise ValueError("why") to reject an image.
      storage_dir() -> path          where its files live; configweb only ever serves files inside it
      thumb_path(id) -> path         a JPEG/PNG/WebP preview (~160-640 px) for the web page's grid
      delete_image(id)               optional: called after a save drops the id from the library
    configweb enforces max_file_mb per file, max_total_mb on storage_dir()'s total size, max_items,
    a real Pillow decode (never the extension or client type), and never uses the client's file
    name anywhere. Ids in a saved form are only accepted if they were already in the library, so
    a form can reorder or drop images but never point at arbitrary files.

Weather and the aurora live in wxfeed.py / aurora.py, which don't declare APP (yet), so
their entries are BUILTIN below. To add an app: write the module, add it to MODULES.

wxshow plays at most MAXP = 48 pages. plan() expands enabled apps x enabled sub-slides
in channel order and drops whatever doesn't fit, so the feeder truncates predictably and
the web page can say exactly what was dropped.

Path: --config PATH, else $NEOCHUMBY_WX_CONFIG, else config.json in $NEOCHUMBY_STATE (the
app's state directory, which neochumbyd gives its companion and its daemon alike), else
DEFAULT_PATH. A missing or broken file means defaults, never a failed render.
"""
import copy
import importlib
import json
import os
import re
import sys
import tempfile

DEFAULT_PATH = "/var/lib/neochumbyd/apps/wx-feed/config.json"
# Sent with every fetch. Some sources refuse requests without a browser-like agent.
USER_AGENT = "Mozilla/5.0 (compatible; neochumby-wx-feed; +https://github.com/johnmerchant/neochumby)"
DEFAULT_TZ = "Australia/Adelaide"
MAXP = 48                      # wxshow's page limit (MAXP in wxshow.c; pages load lazily since 2026-10-28)
ICON_DIR = os.path.dirname(os.path.abspath(__file__)) + "/assets/icons/"

# The weather app's location. Whyalla, South Australia, is only the example default: any
# Australian place with a BOM observation station and a town forecast works. The ids are in
# BOM's anonymous FTP products (ftp://ftp.bom.gov.au/anon/gen/fwo/): the state's
# observations product names each station's wmo-id, and its town forecast product each
# place's aac.
WEATHER_SCHEMA = [
    {"key": "location", "type": "string", "label": "Place name", "default": "Whyalla", "max_len": 20,
     "pattern": r"[A-Za-z][A-Za-z .'\-]{0,19}", "help": "Shown on the pages and the ticker."},
    {"key": "obs_product", "type": "string", "label": "BOM observations product", "default": "IDS60920",
     "max_len": 8, "pattern": r"ID[A-Z][0-9]{5}",
     "help": "The state's observations XML (South Australia: IDS60920; New South Wales: IDN60920)."},
    {"key": "obs_station", "type": "string", "label": "Observation station (wmo-id)", "default": "95664",
     "max_len": 5, "pattern": r"[0-9]{5}", "help": "The station's wmo-id in that product."},
    {"key": "forecast_product", "type": "string", "label": "BOM town forecasts product", "default": "IDS10044",
     "max_len": 8, "pattern": r"ID[A-Z][0-9]{5}", "help": "The state's town forecasts XML (South Australia: IDS10044)."},
    {"key": "forecast_area", "type": "string", "label": "Forecast area (aac)", "default": "SA_PT055",
     "max_len": 10, "pattern": r"[A-Z]{2,3}_[A-Z]{2}[0-9]{3}", "help": "The place's aac in that product."},
    {"key": "latitude", "type": "number", "label": "Latitude", "default": -33.0328, "min": -90, "max": 90,
     "help": "For sunrise and sunset (night mode)."},
    {"key": "longitude", "type": "number", "label": "Longitude", "default": 137.5648, "min": -180, "max": 180},
]

BUILTIN = [
    {"id": "weather", "name": "Weather", "icon": "weather.png", "seconds": 12,
     "description": "Weather from the Bureau of Meteorology for the place you set, with the scrolling news ticker.",
     "schema": WEATHER_SCHEMA,
     "subslides": [{"id": "current", "label": "Current Conditions", "default": True},
                   {"id": "local", "label": "Local Forecast", "default": True},
                   {"id": "extended", "label": "Extended Forecast", "default": True}]},
    {"id": "aurora", "name": "Aurora Screensaver", "icon": "aurora.png", "seconds": 24,
     "description": "The Vista-style aurora with the Chumby logo.", "subslides": [],
     # written into wxshow's logofx line by aurora.render (names = wxshow's fx_names[])
     "schema": [
         {"key": "logo_fx", "type": "choice", "label": "Logo intro", "default": "random",
          "choices": [["random", "Random (a different one each time)"]]
                     + [[n, n.capitalize()] for n in ("fly", "print", "wobble", "zoom", "drop", "dissolve",
                                                       "raster", "decrunch", "spin", "octopus")]},
         {"key": "logo_mode", "type": "choice", "label": "After the intro", "default": "loop",
          "choices": [["loop", "Keep animating (hold, play it out, next intro)"], ["once", "Stay still"]]},
         {"key": "logo_hold", "type": "number", "label": "Hold the logo for (seconds)", "default": 4,
          "min": 1, "max": 60, "integer": True, "help": "With Keep animating: how long it rests between intro and outro."},
     ]},
]
MODULES = ["stocks", "photos", "news"]  # modules that declare APP
# in the registry but switched off in a fresh config: 3 weather + 5 stocks + 4 photos + aurora
# would be 13 > MAXP, cutting the aurora. An empty photo library has nothing to show anyway.
DEFAULT_OFF = {"photos"}
ORDER = ["weather", "news", "stocks", "photos", "aurora"]   # default play order of whatever exists
IMAGE_ID = r"[A-Za-z0-9_-]{1,64}"

GENERAL_SCHEMA = [
    {"key": "config_url", "type": "string", "label": "Address in the Settings QR code",
     "default": "http://neochumbyd.local:8080/", "max_len": 120,
     "pattern": r"https?://[A-Za-z0-9.\-]+(:[0-9]{1,5})?(/[A-Za-z0-9._~\-/]*)?",
     "help": "Where phones go when they scan the code on the Chumby: this page's address."},
    {"key": "timezone", "type": "string", "label": "Time zone", "default": DEFAULT_TZ, "max_len": 40,
     "pattern": r"[A-Za-z_]+(/[A-Za-z0-9_+\-]+){0,2}", "help": "For the clock and the times on pages (Australia/Sydney)."},
]
SECONDS = (5, 120)             # allowed seconds per slide


def default_path():
    if os.environ.get("NEOCHUMBY_WX_CONFIG"):
        return os.environ["NEOCHUMBY_WX_CONFIG"]
    if os.environ.get("NEOCHUMBY_STATE"):
        return os.path.join(os.environ["NEOCHUMBY_STATE"], "config.json")
    return DEFAULT_PATH


def state_dir(path=None):
    """Where caches live (next to the config file)."""
    return os.path.dirname(os.path.abspath(path or default_path()))


# ---------------------------------------------------------------- registry
_reg = None


def registry():
    """[app dicts] with id, name, icon, description, seconds, subslides, schema, module."""
    global _reg
    if _reg is not None:
        return _reg
    apps = [dict(a, schema=a.get("schema", []), module=None) for a in BUILTIN]
    for name in MODULES:
        try:
            m = importlib.import_module(name)
            a = dict(m.APP)
        except Exception as e:          # a broken module must not take the config page down
            print(f"config: can't load app module {name}: {e}", file=sys.stderr)
            continue
        a.setdefault("subslides", [])
        a.setdefault("seconds", 12)
        a["schema"] = getattr(m, "CONFIG_SCHEMA", [])
        a["module"] = m
        apps.append(a)
    rank = {aid: i for i, aid in enumerate(ORDER)}
    _reg = sorted(apps, key=lambda a: rank.get(a["id"], len(ORDER)))
    return _reg


def app_info(aid):
    return next((a for a in registry() if a["id"] == aid), None)


def defaults(schema):
    return {f["key"]: copy.deepcopy(f["default"]) for f in schema}


def default_entry(a, enabled=True):
    return {"app": a["id"], "enabled": enabled, "seconds": a["seconds"],
            "subslides": [{"id": s["id"], "enabled": bool(s.get("default", True))} for s in a["subslides"]],
            "options": defaults(a["schema"])}


# ---------------------------------------------------------------- validation
def validate(schema, raw):
    """Coerce raw values (JSON types, or form strings) to the schema.
    Returns (clean, errors): clean has every key (defaults where missing or bad),
    errors maps key -> message for values that were present but wrong."""
    clean, errors = defaults(schema), {}
    raw = raw if isinstance(raw, dict) else {}
    for fld in schema:
        k, t = fld["key"], fld["type"]
        if k not in raw:
            continue
        v = raw[k]
        try:
            if t == "bool":
                clean[k] = v if isinstance(v, bool) else str(v).lower() in ("1", "true", "on", "yes")
            elif t == "number":
                n = float(str(v).strip())
                if n != n or n in (float("inf"), float("-inf")):
                    raise ValueError("not a number")
                if fld.get("integer"):
                    n = int(round(n))
                lo, hi = fld.get("min"), fld.get("max")
                if (lo is not None and n < lo) or (hi is not None and n > hi):
                    raise ValueError(f"must be between {lo} and {hi}")
                clean[k] = n
            elif t == "choice":
                if v not in [c[0] for c in fld["choices"]]:
                    raise ValueError("not one of the choices")
                clean[k] = v
            elif t == "string":
                s = str(v).strip()
                if len(s) > fld.get("max_len", 200):
                    raise ValueError("too long")
                if fld.get("pattern") and not re.fullmatch(fld["pattern"], s):
                    raise ValueError("doesn't look right")
                clean[k] = s
            elif t == "image_library":
                if not isinstance(v, list):
                    raise ValueError("not a list of images")
                out = []
                for it in v:
                    if isinstance(it, str) and re.fullmatch(r"[A-Za-z0-9_-]{1,64}", it) and it not in out:
                        out.append(it)
                clean[k] = out[:fld.get("max_items", 200)]
            elif t == "list":
                items = v if isinstance(v, list) else re.split(r"[\s,]+", str(v))
                out, bad = [], []
                for it in items:
                    s = str(it).strip()
                    if not s:
                        continue
                    if fld.get("upper"):
                        s = s.upper()
                    if len(s) > 32 or (fld.get("pattern") and not re.fullmatch(fld["pattern"], s)):
                        bad.append(s[:32])
                    elif s not in out:
                        out.append(s)
                if bad:
                    raise ValueError("not valid: " + ", ".join(bad))
                if len(out) > fld.get("max_items", 50):
                    raise ValueError(f"at most {fld.get('max_items', 50)}")
                clean[k] = out
        except (ValueError, TypeError) as e:
            msg = str(e)
            errors[k] = msg if msg and not msg.startswith("could not convert") else "not a valid value"
    return clean, errors


def validate_seconds(v, dflt):
    try:
        n = int(round(float(str(v))))
    except (ValueError, TypeError, OverflowError):
        return dflt, "not a number"
    if not SECONDS[0] <= n <= SECONDS[1]:
        return dflt, f"must be between {SECONDS[0]} and {SECONDS[1]}"
    return n, None


def normalise_subslides(a, raw):
    """Keep the stored order and on/off of known sub-slides; append new ones with their defaults."""
    known = {s["id"]: s for s in a["subslides"]}
    out, seen = [], set()
    for s in raw if isinstance(raw, list) else []:
        if isinstance(s, dict) and s.get("id") in known and s["id"] not in seen:
            seen.add(s["id"])
            out.append({"id": s["id"], "enabled": s.get("enabled", True) is not False})
    for sid, s in known.items():
        if sid not in seen:
            out.append({"id": sid, "enabled": bool(s.get("default", True))})
    return out


# ---------------------------------------------------------------- load / save
def default_config():
    return {"version": 2, "channel": "default",
            "channels": {"default": {"title": "Default", "apps": [
                default_entry(a, enabled=a["id"] not in DEFAULT_OFF) for a in registry()]}},
            "general": defaults(GENERAL_SCHEMA)}


def normalise(cfg):
    """Any dict -> a complete, valid config. Unknown apps and keys are dropped, and every
    known app appears exactly once per channel (missing ones appended, removed)."""
    reg = {a["id"]: a for a in registry()}
    if not isinstance(cfg, dict):
        return default_config()
    out = {"version": 2, "channels": {}}
    chans = cfg.get("channels") if isinstance(cfg.get("channels"), dict) else {}
    for cid, ch in chans.items():
        if not (isinstance(cid, str) and re.fullmatch(r"[a-z0-9_-]{1,24}", cid) and isinstance(ch, dict)):
            continue
        apps, seen = [], set()
        for e in ch.get("apps") if isinstance(ch.get("apps"), list) else []:
            if not isinstance(e, dict) or e.get("app") not in reg or e["app"] in seen:
                continue
            a = reg[e["app"]]
            seen.add(a["id"])
            opts, _ = validate(a["schema"], e.get("options"))
            secs, _ = validate_seconds(e.get("seconds", a["seconds"]), a["seconds"])
            apps.append({"app": a["id"], "enabled": e.get("enabled", True) is not False, "seconds": secs,
                         "subslides": normalise_subslides(a, e.get("subslides")), "options": opts})
        for aid, a in reg.items():
            if aid not in seen:
                apps.append(default_entry(a, enabled=False))
        out["channels"][cid] = {"title": str(ch.get("title") or cid.title())[:40], "apps": apps}
    if not out["channels"]:
        out["channels"] = default_config()["channels"]
    out["channel"] = cfg.get("channel") if cfg.get("channel") in out["channels"] else next(iter(out["channels"]))
    out["general"], _ = validate(GENERAL_SCHEMA, cfg.get("general"))
    return out


def load(path=None):
    path = path or default_path()
    try:
        with open(path) as fh:
            raw = json.load(fh)
    except FileNotFoundError:
        return default_config()
    except (OSError, ValueError) as e:
        print(f"config: {path} unreadable ({e}); using defaults", file=sys.stderr)
        return default_config()
    return normalise(raw)


def save(cfg, path=None):
    """Atomic: a temp file beside it, then rename, so the feeder never reads half a file."""
    path = path or default_path()
    cfg = normalise(cfg)
    d = os.path.dirname(os.path.abspath(path))
    os.makedirs(d, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=".config.", dir=d)
    try:
        with os.fdopen(fd, "w") as fh:
            json.dump(cfg, fh, indent=2)
            fh.write("\n")
        os.chmod(tmp, 0o644)
        os.replace(tmp, path)
    except BaseException:
        os.unlink(tmp)
        raise
    return cfg


# ---------------------------------------------------------------- for the feeder
def channel(cfg, cid=None):
    return cfg["channels"].get(cid or cfg["channel"]) or next(iter(cfg["channels"].values()))


def app_options(cfg, aid, cid=None):
    """An app's options in the channel (its defaults if it isn't there)."""
    e = next((e for e in channel(cfg, cid)["apps"] if e["app"] == aid), None)
    if e:
        return e["options"]
    a = app_info(aid)
    return defaults(a["schema"]) if a else {}


def page_count(a, sid, options):
    fn = getattr(a.get("module"), "page_count", None)
    try:
        return max(0, int(fn(sid, options))) if fn else 1
    except Exception:
        return 1


def plan(cfg, cid=None, limit=MAXP):
    """What the Chumby will play: (kept, dropped), each a list of slide dicts
        {"app", "subslide" (None for an app without sub-slides), "pages", "seconds", "options"}
    in channel order. A slide that doesn't fit whole in the remaining pages is cut to what
    fits (stocks: fewer detail pages); everything after the limit is dropped. With nothing
    enabled at all it falls back to weather's first sub-slide (wxshow needs a page)."""
    reg = {a["id"]: a for a in registry()}
    kept, dropped, used = [], [], 0
    for e in channel(cfg, cid)["apps"]:
        a = reg.get(e["app"])
        if not a or not e["enabled"]:
            continue
        subs = [s["id"] for s in e["subslides"] if s["enabled"]] if a["subslides"] else [None]
        for sid in subs:
            n = page_count(a, sid, e["options"])          # sid None: an app without sub-slides
            if n <= 0:
                continue
            slide = {"app": a["id"], "subslide": sid, "pages": n, "seconds": e["seconds"], "options": e["options"]}
            room = limit - used
            if room <= 0:
                dropped.append(slide)
            elif n > room:
                kept.append(dict(slide, pages=room))
                dropped.append(dict(slide, pages=n - room))
                used = limit
            else:
                kept.append(slide)
                used += n
    if not kept:
        kept = [{"app": "weather", "subslide": "current", "pages": 1, "seconds": 12, "options": {}}]
    return kept, dropped


def write_if_changed(rgb565, img, path):
    """Write img as RGB565 to path only if the bytes differ from what is there, so unchanged
    pages keep their mtime and delivery skips them. Returns True if it wrote."""
    tmp = os.path.join(os.path.dirname(path), f".{os.path.basename(path)}.new")   # dot: sync skips it
    rgb565(img, tmp)
    try:
        with open(tmp, "rb") as a, open(path, "rb") as b:
            same = a.read() == b.read()
    except OSError:
        same = False
    if same:
        os.unlink(tmp)
        return False
    os.replace(tmp, path)
    return True


def write_render_index(out, index):
    """apps.json in the render dir: {app_id: [page .raw files]}, for the web page's
    previews in the customize dialog."""
    try:
        with open(os.path.join(out, "apps.json"), "w") as fh:
            json.dump(index, fh)
    except OSError as e:
        print(f"config: can't write apps.json: {e}", file=sys.stderr)
