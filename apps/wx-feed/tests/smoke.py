#!/usr/bin/env python3
"""smoke: render every page and screen offline, with made-up data, and check the result.

  python3 apps/wx-feed/tests/smoke.py [OUTDIR]     (needs Pillow; CI runs it)

Every app is switched on (the photo library gets two made-up pictures), the network is
fakenet's, and the clock is frozen, so the run is the same everywhere. It checks that:
  - the manifest names only files that exist, with the sizes their directives say;
  - every app put its pages in the slideshow, and every screen a tap can open exists;
  - a second render with the same data writes nothing (the rendering contract: a file is
    rewritten only when its bytes change, and then the manifest is left alone too);
  - the web page draws its pages, the one-time token works once, and its client speaks
    neochumbyd's admin socket protocol (against a stand-in).
"""
import datetime as dt
import os
import sys
import tempfile
from zoneinfo import ZoneInfo

HERE = os.path.dirname(os.path.abspath(__file__))
PY = os.path.join(os.path.dirname(HERE), "files", "python")
sys.path[:0] = [HERE, PY]

import fakenet  # noqa: E402

NOW = dt.datetime(2026, 10, 28, 19, 42, 17, tzinfo=ZoneInfo("Australia/Adelaide"))
fakenet.install(NOW)

FAILS = []


def check(ok, what):
    print(("ok    " if ok else "FAIL  ") + what)
    if not ok:
        FAILS.append(what)


def manifest(out):
    with open(os.path.join(out, "manifest.txt")) as fh:
        return [ln.split() for ln in fh.read().splitlines() if ln and not ln.startswith("#")]


def sizes_ok(out, lines):
    """Each picture's size from its directive: (file, expected bytes) pairs."""
    want = []
    for t in lines:
        if t[0] in ("page", "pagenight", "screen"):              # page N FILE ..., screen NAME FILE
            want.append((t[2], 320 * 240 * 2))
        elif t[0] == "pane":                                    # pane SCREEN FILE W H X Y VAR N
            want.append((t[2], int(t[3]) * int(t[4]) * int(t[8]) * 2))
        elif t[0] == "sprite":                                  # sprite PAGE FILE W H FRAMES X Y FPS
            want.append((t[2], int(t[3]) * int(t[4]) * int(t[5]) * 2))
    bad = [(f, n, os.path.getsize(os.path.join(out, f)) if os.path.exists(os.path.join(out, f)) else None)
           for f, n in want if not os.path.exists(os.path.join(out, f)) or os.path.getsize(os.path.join(out, f)) != n]
    return want, bad


def snapshot(out):
    return {f: os.stat(os.path.join(out, f)).st_mtime_ns for f in os.listdir(out)
            if f.endswith((".raw", ".l8", ".txt"))}


def admin_socket(work, admin, configweb):
    """The web page's client against a stand-in for neochumbyd's admin socket (#9's protocol)."""
    import socketserver
    import threading
    owner = "0123456789abcdef0123456789abcdef"
    seen = []

    class H(socketserver.StreamRequestHandler):
        def handle(self):
            for raw in self.rfile:
                w = raw.decode().split()
                seen.append(w)
                if w == ["paired"]:
                    out = ["paired kitchen " + "ab" * 32 + " home=wx delivery=sync", "ok"]
                elif w == ["pending"]:
                    out = ["pending 123456 lounge 192.0.2.7 " + "cd" * 32, "ok"]
                elif w[:1] == ["check"]:
                    out = ["ok" if w[1:] == [owner] else "error wrong token"]
                elif w[:1] == ["approve"]:
                    out = ["ok lounge" if w[2:] == [owner] else "error wrong token"]
                elif w[:1] in (["set"], ["forget"], ["refuse"]):
                    out = ["ok"]
                else:
                    out = ["error unknown command"]
                self.wfile.write(("\n".join(out) + "\n").encode())

    path = os.path.join(work, "admin.sock")
    class Srv(socketserver.ThreadingUnixStreamServer):
        daemon_threads = True                                    # the client keeps its connection open

    srv = Srv(path, H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    os.environ["NEOCHUMBY_ADMIN"] = path
    try:
        chumbys, pending = admin.listing()
        check([c.name for c in chumbys] == ["kitchen"] and chumbys[0].settings == {"home": "wx", "delivery": "sync"},
              "socket: paired Chumbys and their settings")
        check([(p.name, p.code, p.addr) for p in pending] == [("lounge", "123456", "192.0.2.7")], "socket: requests to pair")
        check(not admin.login("f" * 32) and not admin.login(owner + " x") and admin.login(owner),
              "socket: only the owner token signs in")
        check(admin.approve("123456", owner) == "lounge", "socket: approve passes the token")
        try:
            admin.approve("123456 x", owner)
            check(False, "socket: a code with a space is refused")
        except admin.AdminError:
            check(True, "socket: a code with a space is refused")
        admin.set_delivery("kitchen", "nfs")
        check(seen[-1] == ["set", "kitchen", "delivery=nfs"], "socket: set")
        sid, err = configweb.log_in(owner)
        check(sid is not None and configweb.SESSIONS[sid][2] == owner, "socket: a session keeps the owner token")
    finally:
        os.environ.pop("NEOCHUMBY_ADMIN", None)
        srv.shutdown()
        srv.server_close()


def main():
    work = tempfile.mkdtemp(prefix="wx-smoke-")
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(work, "out")
    cfg_path = os.path.join(work, "config.json")
    os.environ["NEOCHUMBY_WX_CONFIG"] = cfg_path
    os.environ["NEOCHUMBY_STATE"] = work
    os.environ.pop("NEOCHUMBY_WX_ASSETS", None)                # the placeholders, as published

    import config
    import photos
    import wxfeed

    # every app on, two pictures in the library
    ids = []
    for k in range(2):
        src = os.path.join(work, f"in{k}.jpg")
        with open(src, "wb") as fh:
            fh.write(fakenet.picture(f"photo{k}", 1200, 900))
        ids.append(photos.ingest_image(src))
    cfg = config.default_config()
    for e in config.channel(cfg)["apps"]:
        e["enabled"] = True
        if e["app"] == "photos":
            e["options"]["library"] = ids
    config.save(cfg, cfg_path)
    kept, dropped = config.plan(config.load(cfg_path))
    check(not dropped, f"plan fits the page limit ({sum(s['pages'] for s in kept)} pages)")

    sys.argv = ["wxfeed.py", out, "--config", cfg_path]
    wxfeed.main()
    lines = manifest(out)
    pages = [t for t in lines if t[0] == "page"]
    check(len(pages) == sum(s["pages"] for s in kept), f"{len(pages)} slideshow pages, as planned")
    for app, prefix in (("weather", "weather_"), ("news", "news_slide_"), ("stocks", "stocks_"), ("photos", "photo"),
                        ("aurora", "aurora_")):
        check(any(t[2].startswith(prefix) for t in pages), f"{app} has a page")
    screens = {t[1] for t in lines if t[0] == "screen"}
    for want in ("home", "home_night", "settings", "timing", "network", "about", "configure", "music", "confirm",
                 "stocks_list", "news_nat_l0"):
        check(want in screens, f"screen {want}")
    targets = {t[-1] for t in lines if t[0] == "hot" and t[-2] == "screen"} | {t[2] for t in lines if t[0] == "pagetap"}
    check(targets <= screens, f"every screen a tap opens exists ({len(targets)})")
    want, bad = sizes_ok(out, lines)
    check(not bad, f"{len(want)} pictures have the sizes their directives say {bad[:3]}")
    named = {x for t in lines for x in t if x.endswith((".raw", ".l8"))}
    missing = [f for f in named if not os.path.exists(os.path.join(out, f))]
    check(not missing, f"all {len(named)} files the manifest names exist {missing[:3]}")
    with open(os.path.join(out, "hashes.txt")) as fh:
        hashed = {ln.split()[1] for ln in fh if ln.strip()}
    check(named <= hashed, "hashes.txt covers every file")
    check(not any(f.startswith(".") for f in os.listdir(out)), "no temp files left")

    before = snapshot(out)
    wxfeed.main()
    after = snapshot(out)
    changed = sorted(f for f in after if before.get(f) != after[f])
    check(not changed, f"a second render with the same data writes nothing {changed[:5]}")

    # the web page
    import argparse
    import configweb
    import neochumbyd_admin
    configweb.ARGS = argparse.Namespace(config=cfg_path, out=out, hello="")
    code, body = configweb.page(config.load(cfg_path))
    check(code == 200 and "my channels" in body, "the channels page renders")
    for a in config.registry():
        check(f'id="cz-{a["id"]}"' in body, f"customize dialog for {a['id']}")
    code, body = configweb.login_page()
    check(code == 200 and 'name="token"' in body, "the login page renders")
    neochumbyd_admin.new_token()
    with open(neochumbyd_admin.token_path()) as fh:
        tok = fh.read().strip()
    check(oct(os.stat(neochumbyd_admin.token_path()).st_mode & 0o777) == "0o600", "the admin token file is 0600")
    check(not neochumbyd_admin.check_token("x" + tok), "a wrong token is refused")
    check(neochumbyd_admin.check_token(tok), "the admin token works")
    check(not neochumbyd_admin.check_token(tok), "...once")
    check(configweb.host_ok("192.168.1.9:8080") and not configweb.host_ok("evil.example.com"), "Host check")

    admin_socket(work, neochumbyd_admin, configweb)

    print(f"\n{len(FAILS)} failed" if FAILS else "\nall passed")
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
