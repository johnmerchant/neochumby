# wx-feed: the wx shell's server half

The wx shell is a 2008-Chumby-style home screen with a slideshow of weather, news,
stocks, photos and an aurora screensaver. The Chumby only copies pixels. A server draws
every page and screen as RGB565, with a `manifest.txt` saying what goes where. This
crate is that server half:

| Binary | Manifest key | What it runs |
| --- | --- | --- |
| `neochumby-wx-feed` | `companion` | `python/wxfeed.py`, the renderer: one run per Chumby every few minutes |
| `neochumby-wx-feed-web` | `daemon` (neochumbyd #9) | `python/configweb.py`, the "my channels" web page, once per server |

The rendering and the web page are **Python** (Pillow for the drawing). The binaries
are thin Rust launchers, so every entry point is still a binary built from this crate.
The device side (the wx engine) and neochumbyd stay Rust.

## Layout

```
apps/wx-feed/
  Cargo.toml, src/        the two launchers (src/py.rs: finding the Python)
  files/python/           the renderer, the widgets and the web page; tools/tree
                          copies files/ into the app's directory, so the launchers
                          find it as python/ beside themselves
    wxfeed.py             renders everything; screens.py (Home, Settings, Music),
                          config.py (the config file and the app registry), and one
                          module per widget: aurora.py, news.py, stocks.py,
                          photos.py, qrscreen.py
    configweb.py          the web page; neochumbyd_admin.py, its one way into
                          neochumbyd (pairing, shells, delivery)
    wxassets.py           where pictures that aren't ours come from (below)
    assets/fonts/         Liberation, DejaVu and SN Pro, with their licences
    assets/icons/         the app icons (make_icons.py draws them)
  tests/                  smoke.py and fakenet.py: every page rendered offline
```

It is one crate, not a renderer app and a web app, because the two share everything:
the web page edits the `config.json` the renderer reads, lists the widgets from the
same modules (`APP`, `CONFIG_SCHEMA`), stores the uploaded photos the renderer shows,
and previews its renders. neochumbyd gives an app one state directory, so one app gets
one shared directory.

## Running it

The server needs Python 3.12 or newer, and `pip install -r python/requirements.txt`
(Pillow, segno; pillow-heif optional).

| Variable | Default | |
| --- | --- | --- |
| `NEOCHUMBY_STATE` | `/var/lib/neochumbyd/apps/wx-feed` | the state directory (neochumbyd sets it): `config.json`, `photos/`, caches, `out/` |
| `NEOCHUMBY_WX_OUT` | `<neochumbyd state>/overlay/{chumby}/apps/wx-feed/out` | where a Chumby's renders go; `{chumby}` stands for its name, so each Chumby has its own directory |
| `NEOCHUMBY_WX_EVERY` | `600` | seconds between renders (at least 60) |
| `NEOCHUMBY_WX_PYTHON` | `python3` | the interpreter (a venv's, say) |
| `NEOCHUMBY_WX_PYTHON_DIR` | `python/` beside the binary | the scripts |
| `NEOCHUMBY_WX_WEB` | `0.0.0.0:8080` | the web page's address |
| `NEOCHUMBY_WX_ASSETS` | none | directories of your own pictures (below) |
| `NEOCHUMBY_ADMIN` | none (neochumbyd sets it) | neochumbyd's admin socket (#9): pairing, each Chumby's settings, the owner token check |
| `NEOCHUMBY_TREE` | `<neochumbyd state>/tree` (neochumbyd sets it) | the tree the web page lists shells from |
| `NEOCHUMBYD`, `NEOCHUMBYD_STATE` | `neochumbyd`, two up from the state | without the socket: the `neochumbyd` command the web page runs instead |
| `NEOCHUMBY_ADMIN_TOKEN_FILE` | `<state>/admin-token` | without the socket: the one-time sign-in token |

**The companion** starts when a Chumby opens the app, and stops when it goes. It
renders at once, then every `NEOCHUMBY_WX_EVERY` seconds, and right away when the web
page's SHOW ON CHUMBY NOW (or any save) touches `<state>/refresh-request`. After a
render that changed something, it sends the Chumby one frame: `[0]` and then the
manifest's version.

**The web page** is a `daemon=` app (neochumbyd #9): neochumbyd starts it once, with
`NEOCHUMBY_STATE`, `NEOCHUMBY_ADMIN` and `NEOCHUMBY_TREE`. Add
`daemon = "neochumby-wx-feed-web"` to `Cargo.toml` once #9 is merged; until then
`neochumby-manifest` refuses the key, so run it by hand. The Chumbys page (pairing
approvals, forgetting a Chumby, each Chumby's shell and delivery) needs a sign-in:
- **With the admin socket:** the server owner's token, which `neochumbyd token` prints.
  You type it in; the page never reads it from disk. neochumbyd checks it, and the page
  keeps it in memory for the session to pass on `approve`.
- **Without it** (run by hand, as a user in neochumbyd's group): a one-time token from
  the file the log names. Signing in writes a new one.

The Chumby setting that names its shell (`home` today; proposal 0002, #8, may rename it)
is one constant, `SHELL_SETTING` in `neochumbyd_admin.py`.

## What it writes

`manifest.txt` and the files it names (`*.raw`, `*.l8`), plus `hashes.txt` (md5sum
format), in the Chumby's output directory. By default that is neochumbyd's overlay for
the Chumby. **sync** (#10) layers the overlay over the shared tree and sends it to that
Chumby alone, so its engine reads `apps/wx-feed/out/manifest.txt` in its tree.
**NFS doesn't serve the overlay yet**, so a Chumby with `delivery=nfs` gets no
pictures until a follow-up does. It follows the wx rendering contract:
- **Stable names.** The same picture keeps the same name on every run.
- **Changes only.** A file is written only when its bytes change, under a name starting
  with `.` (which sync skips) and then renamed into place, so a reader never sees half
  a file.
- **The manifest last.** It is rewritten only when something it names changed.
- **No per-run times in big files.** For example, the Chumby Info screen shows the
  weather's observation time, not the render's.

So a render that changes nothing writes nothing.

## Pictures that aren't ours

Nothing third-party is committed. These files are looked for in `NEOCHUMBY_WX_ASSETS`,
and without them each widget draws its own placeholder:

| File | Used for | Without it |
| --- | --- | --- |
| `logo.png` | the aurora's logo | a "neochumby" wordmark |
| `octopus.png` | the mark on Home, Settings and the leave dialog | a drawn octopus |
| `weather-badge.png` | the weather pages' badge | a drawn sun and cloud |
| `raysclip.png` | the aurora's light rays (`raysclip.py` makes it from a video) | no rays |
| `abc-logo.png`, `abc-swirls.png` | the news header (`news.py assets BANNER.png`) | a plain NEWS mark |

Company logos on the stock pages are fetched at run time and cached in the state
directory, never committed.

## Data sources, and terms to check before this is used widely

| Source | Used by | To check |
| --- | --- | --- |
| Bureau of Meteorology, anonymous FTP (`ftp.bom.gov.au/anon/gen/fwo/`) | weather, ticker | BOM's copyright and data terms for redistribution and display. Its website blocks scrapers; the FTP products are the sanctioned route |
| ABC News RSS, and ABC article pages (`abc.net.au`) | news, ticker | ABC's RSS terms (personal, non-commercial use). The Eyre Peninsula page and the article bodies are **scraped**, which the terms may not allow |
| Yahoo Finance chart endpoint (`query1.finance.yahoo.com/v8/finance/chart`) | stocks | Unofficial and undocumented. Yahoo's terms allow personal use, not redistribution |
| financialmodelingprep.com, assets.parqet.com | stock logos | Unofficial. The logos are the companies' trademarks |
| SomaFM API and streams | music | SomaFM's terms for the API and the channel artwork |

The weather defaults to Whyalla, South Australia, only as an example: set the place,
its BOM station and forecast area in the Weather app's options on the web page.

## Checking it

```sh
pip install -r apps/wx-feed/files/python/requirements.txt ruff
ruff check apps/wx-feed
python3 apps/wx-feed/tests/smoke.py      # every page, offline, made-up data
cargo clippy -p neochumby-wx-feed --all-targets -- -D warnings && cargo test -p neochumby-wx-feed
```
