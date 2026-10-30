#!/usr/bin/env python3
"""Tests of dev/emulate that need neither firmware nor QEMU: the images it
makes (through qemu-chumby's chumby_machine), and its whole control path
against a stub QEMU that speaks QMP and has a console (so what the real
machine would be sent is checked, not what it would do with it). CI runs
them; so can a Mac.

    python3 dev/test_emulate.py
"""
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
import socket
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.dont_write_bytecode = True
sys.path.insert(0, HERE)
# chumby_machine, from the qemu-chumby submodule (or CHUMBY_MACHINE_PATH, as
# dev/emulate takes it).
sys.path.insert(0, os.environ.get("CHUMBY_MACHINE_PATH") or os.path.join(REPO, "emu", "qemu", "qemu-chumby"))
import chumby_machine as cm  # noqa: E402

EMULATE = os.path.join(HERE, "emulate")
# Not your own settings (.env.local): the tests choose what they have.
os.environ["NEOCHUMBY_ENV_LOCAL"] = os.devnull


def load_emulate():
    loader = importlib.machinery.SourceFileLoader("emulate", EMULATE)
    spec = importlib.util.spec_from_loader("emulate", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


emulate = load_emulate()


def fake_dir(root):
    """A settings directory with a file in a subdirectory, an empty file, a
    file over one 4 KiB node, a link and an executable: made the same way
    everywhere, modes included."""
    os.makedirs(os.path.join(root, "rfs1"))
    files = {"hostname": b"kitchen\n", "empty": b"", "rfs1/big": bytes(range(256)) * 20,
             "rfs1/run": b"#!/bin/sh\necho hi\n"}
    for name, data in files.items():
        with open(os.path.join(root, name), "wb") as f:
            f.write(data)
        os.chmod(os.path.join(root, name), 0o644)
    os.chmod(os.path.join(root, "rfs1/run"), 0o755)
    os.chmod(os.path.join(root, "rfs1"), 0o755)
    os.symlink("hostname", os.path.join(root, "name"))
    return root


def fake_tree(root):
    os.makedirs(os.path.join(root, "apps", "home"))
    for name in ("neochumby", "userhook2", "apps/home/neochumby-home"):
        with open(os.path.join(root, name), "w") as f:
            f.write("#!/bin/sh\n")
    return root


# chumby-image (qemu-chumby's image/, once neochumby's emu/) `mkpsp` of
# fake_dir: the SHA-256 of its output, so that the Python port is checked
# against it byte for byte.
MKPSP_SHA256 = "d8cd340cee3faf4f6716f82ed1fe14a6dbb02eb1778f8f440ab05233a56a1284"


class Images(unittest.TestCase):
    def test_crc(self):
        # JFFS2's CRC: CRC-32 from 0, not inverted. Of b"123456789", the
        # reflected 0xEDB88320 CRC with those settings is 0x2DFD2D88.
        self.assertEqual(cm.jffs2_crc(b"123456789"), 0x2DFD2D88)

    def test_jffs2_matches_chumby_image(self):
        with tempfile.TemporaryDirectory() as t:
            image = cm.jffs2(fake_dir(os.path.join(t, "psp")), cm.PARTITIONS["PSP"][1])
        self.assertEqual(len(image), cm.PARTITIONS["PSP"][1])
        self.assertEqual(image[:2], b"\x85\x19")
        self.assertEqual(hashlib.sha256(image).hexdigest(), MKPSP_SHA256)

    def test_too_big(self):
        with tempfile.TemporaryDirectory() as t:
            with self.assertRaises(cm.Error):
                cm.jffs2(fake_dir(os.path.join(t, "psp")), 4096)

    def test_nand_with(self):
        with tempfile.TemporaryDirectory() as t:
            nand, out = os.path.join(t, "nand.img"), os.path.join(t, "out.img")
            with open(nand, "wb") as f:
                f.truncate(cm.NAND_BYTES)
            offset, size = cm.PARTITIONS["PSP"]
            cm.nand_with(nand, out, {"PSP": bytes([1]) * size})
            with open(out, "rb") as f:
                chip = f.read()
            self.assertEqual(os.path.getsize(nand), cm.NAND_BYTES)
            with open(nand, "rb") as f:
                self.assertEqual(f.read().count(0), cm.NAND_BYTES, "the original is untouched")
        at = offset // cm.PAGE * cm.RECORD
        records = size // cm.PAGE * cm.RECORD
        self.assertEqual(chip[at - 1], 0, "the partition before is untouched")
        self.assertEqual(chip[at:at + 512], bytes([1]) * 512)
        self.assertEqual(chip[at + 512:at + 528], b"\xff" * 16, "a blank spare area")
        self.assertEqual(chip[at + records], 0, "the partition after is untouched")
        self.assertEqual(chip.count(1), size)

    def test_nand_refuses_wrong_size(self):
        with tempfile.TemporaryDirectory() as t:
            nand = os.path.join(t, "nand.img")
            with open(nand, "wb") as f:
                f.truncate(1024)
            with self.assertRaises(cm.Error):
                cm.nand_with(nand, os.path.join(t, "out"), {})

    def test_touch_events(self):
        ev = cm.pen_events(True, 0, 239)
        self.assertEqual(ev[0], {"type": "abs", "data": {"axis": "x", "value": 51}})
        self.assertEqual(ev[1]["data"]["value"], 32698)
        self.assertEqual(ev[2], {"type": "btn", "data": {"button": "left", "down": True}})
        self.assertLessEqual(cm.axis(319, 320), 32767)

    def test_ppm(self):
        ppm = b"P6\n320 240\n255\n" + bytes([255, 0, 8]) * (320 * 240)
        out = cm.rgb_to_rgb565(cm.ppm_rgb(ppm))
        self.assertEqual(len(out), 320 * 240)
        self.assertEqual(out[0], 0xF801)

    def test_stick(self):
        """The stick start makes: the hook, the tree, the settings and a
        bind, read back from the FAT image."""
        with tempfile.TemporaryDirectory() as t:
            tree = fake_tree(os.path.join(t, "tree"))
            extra = os.path.join(t, "extra")
            os.makedirs(extra)
            with open(os.path.join(extra, "neochumby.conf"), "w") as f:
                f.write("start=classic\n")
            pictures = os.path.join(t, "pictures")
            os.makedirs(pictures)
            with open(os.path.join(pictures, "manifest.txt"), "w") as f:
                f.write("page 1\n")
            img = os.path.join(t, "stick.img")
            emulate.make_stick(img, tree, "dist", extra, [(pictures, "/tmp/classic")])
            files = cm.fat_files(img)
        with open(emulate.HOOK, "rb") as f:
            self.assertEqual(files["userhook2"], f.read())
        self.assertEqual(files["neochumby/neochumby"], b"#!/bin/sh\n")
        self.assertIn("neochumby/apps/home/neochumby-home", files)
        self.assertEqual(files["emulate/psp/neochumby.conf"], b"start=classic\n")
        self.assertEqual(files["emulate/psp/ts_settings"], emulate.NEUTRAL_CALIBRATION.encode() + b"\n")
        self.assertEqual(files["emulate/binds"], b"0 /tmp/classic\n")
        self.assertEqual(files["emulate/bind/0/manifest.txt"], b"page 1\n")

    def test_stock_stick_has_no_settings(self):
        """The stock profile's settings are in the NAND, not on the stick."""
        with tempfile.TemporaryDirectory() as t:
            img = os.path.join(t, "stick.img")
            emulate.make_stick(img, fake_tree(os.path.join(t, "tree")), "stock", None, None)
            self.assertNotIn("emulate/psp", cm.fat_files(img))


# A stand-in for qemu-system-arm -M chumby-classic, run in the machine's
# directory: checks its command line, records it and what the NAND image
# holds, and answers QMP, recording each input event; its console runs each
# line typed as a shell command, as a Chumby's shell would, and logs both.
STUB = textwrap.dedent(r'''
    #!/usr/bin/env python3
    import json, os, select, socket, subprocess, sys
    a = sys.argv[1:]
    def opt(name):
        return a[a.index(name) + 1]
    assert opt("-M") == "chumby-classic" and opt("-display") == "none" and opt("-nic") == "none"
    assert "-netdev" not in a and "-net" not in a
    json.dump(a, open("argv.json", "w"))
    qmp_path = opt("-qmp").split(",")[0].removeprefix("unix:")
    chardev = dict(kv.split("=", 1) for kv in opt("-chardev").split(",")[1:])
    log = open(chardev["logfile"], "ab", buffering=0)
    mtd = [d for d in a if d.startswith("if=mtd")]
    if mtd:
        nand = dict(kv.split("=", 1) for kv in mtd[0].split(","))["file"]
        with open(nand, "rb") as f:
            f.seek(0x100000 // 512 * 528)
            magic = f.read(2)
        log.write(b"stub: nand %d bytes, psp magic %s\n" % (os.path.getsize(nand), magic.hex().encode()))
    log.write(b"emutest: stick mounted\nneochumby: /dev/switch: No such file or directory\n"
              b"neochumby: opening home\n")
    events = open(os.environ["STUB_EVENTS"], "a")
    def listen(path):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.bind(path)
        s.listen(1)
        return s
    qmp, con = listen(qmp_path), listen(chardev["path"])
    clients = {}
    while True:
        ready, _, _ = select.select([qmp, con] + list(clients), [], [])
        for r in ready:
            if r is qmp or r is con:
                c, _ = r.accept()
                clients[c] = [r, b""]
                if r is qmp:
                    c.sendall(b'{"QMP": {"version": {}, "capabilities": []}}\n')
                continue
            got = r.recv(4096)
            if not got:
                del clients[r]
                r.close()
                continue
            kind, buf = clients[r]
            buf += got
            if kind is con:
                while b"\r" in buf:
                    line, buf = buf.split(b"\r", 1)
                    out = subprocess.run(["sh", "-c", line.decode()], capture_output=True).stdout if line else b""
                    log.write(line + b"\r\n" + out.replace(b"\n", b"\r\n") + b"/ # ")
                clients[r][1] = buf
                continue
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                m = json.loads(line)
                cmd, args = m["execute"], m.get("arguments", {})
                reply = {"return": {}}
                if cmd == "input-send-event":
                    for e in args["events"]:
                        events.write(json.dumps(e) + "\n")
                    events.flush()
                    if args["events"][-1] == {"type": "btn", "data": {"button": "left", "down": False}}:
                        log.write(b"neochumby: home asks: chumby-mode\nneochumby exited: 0\n")
                elif cmd == "qom-set":
                    events.write(json.dumps({"qom-set": [args["property"], args["value"]]}) + "\n")
                    events.flush()
                elif cmd == "screendump":
                    assert os.path.isabs(args["filename"]) and args["format"] in ("ppm", "png")
                    with open(args["filename"], "wb") as out:
                        out.write(b"P6\n320 240\n255\n" + bytes([16, 16, 33]) * (320 * 240))
                elif cmd == "query-status":
                    reply = {"return": {"status": "running", "running": True}}
                elif cmd == "query-chumby-state":
                    reply = {"return": {"backlight": 65535, "button": False}}
                if "id" in m:
                    reply["id"] = m["id"]
                r.sendall(b'{"event": "NOISE"}\n' + json.dumps(reply).encode() + b"\n")
                if cmd == "quit":
                    sys.exit(0)
            clients[r][1] = buf
''').lstrip()


class Machine(unittest.TestCase):
    def setUp(self):
        self.t = tempfile.mkdtemp(prefix="emu.")
        self.state = os.path.join(self.t, "s")
        self.stub = os.path.join(self.t, "qemu")
        with open(self.stub, "w") as f:
            f.write(STUB)
        os.chmod(self.stub, 0o755)
        self.events = os.path.join(self.t, "events")
        self.kernel = os.path.join(self.t, "zImage")
        open(self.kernel, "wb").close()
        self.nand = os.path.join(self.t, "nand.img")
        with open(self.nand, "wb") as f:
            f.truncate(cm.NAND_BYTES)
        self.psp = fake_dir(os.path.join(self.t, "psp"))
        self.tree = fake_tree(os.path.join(self.t, "tree"))
        self.env = dict(os.environ, STUB_EVENTS=self.events)
        for k in list(self.env):
            if k.startswith("NEOCHUMBY_EMU") or k == "QEMU":
                del self.env[k]

    def tearDown(self):
        self.emu("stop")
        subprocess.run(["rm", "-rf", self.t])

    def emu(self, *args, profile="dist"):
        return subprocess.run([EMULATE, "--profile", profile, "--state", self.state, *args], env=self.env,
                              capture_output=True, text=True, timeout=60)

    def argv(self):
        with open(os.path.join(self.state, "argv.json")) as f:
            return json.load(f)

    def sent(self):
        with open(self.events) as f:
            return [json.loads(line) for line in f]

    def test_skipped_without_images(self):
        r = self.emu("start", self.tree, profile="stock")
        self.assertEqual(r.returncode, 77)
        self.assertIn("needs your own firmware", r.stdout)
        # A kernel given that isn't there (one not given is fetched or
        # built: Prebuilt, below).
        self.env["NEOCHUMBY_EMU_DIST_KERNEL"] = os.path.join(self.t, "nothing")
        r = self.emu("start", self.tree, "--qemu", self.stub)
        self.assertEqual(r.returncode, 77)
        self.assertIn("needs the dist image", r.stdout)

    def test_no_network(self):
        r = self.emu("start", self.tree, "--kernel", self.kernel, "--qemu", self.stub, "--server", "127.0.0.1:1")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("no network", r.stderr)

    def test_server_over_the_serial_line(self):
        # A short path: a Unix socket's must be.
        d = tempfile.mkdtemp(prefix="ncl", dir="/tmp")
        self.addCleanup(shutil.rmtree, d, True)
        path = os.path.join(d, "s")
        sock = socket.socket(socket.AF_UNIX)
        sock.bind(path)
        self.addCleanup(sock.close)
        r = self.emu("start", self.tree, "--kernel", self.kernel, "--qemu", self.stub, "--server", path)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        a = self.argv()
        self.assertEqual(a[a.index("-nic") + 1], "none")
        self.assertIn(f"socket,id=link,path={path},server=off,reconnect-ms=1000", a)
        self.assertEqual(a[len(a) - a[::-1].index("-serial")], "chardev:link")
        files = cm.fat_files(os.path.join(self.state, "stick.img"))
        self.assertEqual(files["neochumby.conf"], b"server=serial\n")
        self.assertIn("emulate/kept", files)
        self.emu("stop")

    def test_dist(self):
        r = self.emu("start", self.tree, "--kernel", self.kernel, "--qemu", self.stub)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.emu("wait", "log", "neochumby: opening home", "20").returncode, 0)
        a = self.argv()
        self.assertEqual(a[a.index("-append") + 1], "console=ttymxc0,115200 rdinit=/init")
        self.assertIn("usb-storage,drive=stick", a)
        self.assertFalse([d for d in a if d.startswith("if=mtd")], "no NAND")
        stick = [d for d in a if d.startswith("if=none,id=stick")][0]
        self.assertIn(f"file={os.path.join(self.state, 'stick.img')},", stick)
        files = cm.fat_files(os.path.join(self.state, "stick.img"))
        self.assertIn("neochumby/apps/home/neochumby-home", files)

        status = self.emu("status").stdout
        self.assertIn("profile: dist", status)
        self.assertIn("status: running", status)
        self.assertIn("button_open: False", status, "the session said /dev/switch failed")
        self.assertEqual(self.emu("pixel", "0", "0").stdout.strip(), "#101021")
        self.assertEqual(self.emu("shot", os.path.join(self.t, "s.png")).returncode, 0)
        self.assertEqual(self.emu("wait", "screen", "10").returncode, 0)

        r = self.emu("exec", "echo", "hello there")
        self.assertEqual((r.returncode, r.stdout), (0, "hello there\n"), r.stderr)
        self.assertEqual(self.emu("exec", "false").returncode, 1)

        self.assertEqual(self.emu("button", "press").returncode, 0)
        self.assertEqual(self.emu("tap", "160", "120").returncode, 0)
        self.assertEqual(self.sent(), [{"qom-set": ["button", True]}, {"qom-set": ["button", False]}]
                         + cm.pen_events(True, 160, 120) + cm.pen_events(False))
        r = self.emu("wait", "exit", "20")
        self.assertEqual(r.stdout.strip(), "exit 0", r.stdout + r.stderr)
        r = self.emu("wait", "log", "never", "5")
        self.assertIn("the session ended", r.stderr)

        cut = os.path.join(self.t, "cut.img")
        self.assertEqual(self.emu("powercut", cut).returncode, 0)
        with open(cut, "rb") as a, open(os.path.join(self.state, "stick.img"), "rb") as b:
            self.assertEqual(a.read(), b.read())
        self.assertIn("stopped", self.emu("status").stdout)

    def test_stock(self):
        """The stock profile: your NAND, copied with its settings partition
        rewritten (and never changed itself), the stock command line."""
        extra = os.path.join(self.t, "extra")
        os.makedirs(extra)
        with open(os.path.join(extra, "neochumby.conf"), "w") as f:
            f.write("start=classic\n")
        r = self.emu("start", self.tree, "--kernel", self.kernel, "--nand", self.nand, "--factory-psp", self.psp,
                     "--qemu", self.stub, "--psp", extra, profile="stock")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.emu("wait", "log", "neochumby: opening home", "20", profile="stock").returncode, 0)
        self.assertIn(f"stub: nand {cm.NAND_BYTES} bytes, psp magic 8519",
                      self.emu("log", profile="stock").stdout)
        a = self.argv()
        self.assertNotIn("-append", a)
        copy = os.path.join(self.state, "nand.img")
        self.assertIn(f"if=mtd,file={copy},format=raw", a)
        with open(self.nand, "rb") as f:
            self.assertEqual(f.read().count(0), cm.NAND_BYTES, "your NAND image is untouched")
        with tempfile.TemporaryDirectory() as w:
            psp = emulate.psp_dir(w, self.psp, extra)
            for name in ("disable_intro", "neochumby.conf", "hostname", "rfs1/big"):
                self.assertTrue(os.path.exists(os.path.join(psp, name)), name)
            with open(os.path.join(psp, "ts_settings")) as f:
                self.assertEqual(f.read().strip(), emulate.NEUTRAL_CALIBRATION)
            want = os.path.join(w, "want.img")
            cm.nand_with(self.nand, want, {"PSP": cm.jffs2(psp, cm.PARTITIONS["PSP"][1])})
            with open(want, "rb") as x, open(copy, "rb") as y:
                self.assertEqual(hashlib.sha256(x.read()).digest(), hashlib.sha256(y.read()).digest())
        self.assertEqual(self.emu("stop", profile="stock").returncode, 0)


# A stand-in for neochumbyd's developer link, in its state directory as
# tools/deploy installs it: one paired Chumby, "kitchen", whose log is a
# file (with a line from before, which dev/emulate must let pass).
FAKE_NEOCHUMBYD = textwrap.dedent(r'''
    #!/usr/bin/env python3
    import os, struct, sys, time, zlib
    a = sys.argv[1:]
    state = a[a.index("--state") + 1]
    a = a[:a.index("--state")]
    log = os.path.join(state, "fake-log")
    def say(line):
        with open(log, "a") as f:
            f.write(line + "\n")
    def usage():
        # As neochumbyd's: an older one's lacks tap and button.
        cmds = "serve|log NAME [-f]|restart NAME|shot NAME OUT.png"
        if not os.environ.get("FAKE_WITHOUT_INPUT"):
            cmds += "|tap NAME X Y [--hold MS]|button NAME press|release|tap"
        print(f"usage: neochumbyd {cmds} [--state DIR]", file=sys.stderr)
        sys.exit(2)
    cmd, name = a[0], a[1] if len(a) > 1 else ""
    if name != "kitchen":
        print(f"no Chumby paired as {name}", file=sys.stderr)
        sys.exit(1)
    if cmd == "log":
        with open(log) as f:
            sys.stdout.write(f.read())
            sys.stdout.flush()
            while "-f" in a:
                line = f.readline()
                if line:
                    sys.stdout.write(line)
                    sys.stdout.flush()
                else:
                    time.sleep(0.05)
    elif cmd == "restart":
        say("neochumby: opening home")
        print(f"{name}: restarting")
    elif cmd == "shot":
        rows = b"".join(b"\x00" + bytes([16, 16, 33]) * 320 for _ in range(240))
        def chunk(t, d):
            return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
        png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 320, 240, 8, 2, 0, 0, 0))
        png += chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")
        open(a[2], "wb").write(png)
    elif cmd in ("tap", "button"):
        if os.environ.get("FAKE_WITHOUT_INPUT"):
            usage()
        if os.environ.get("FAKE_DEV_OFF"):
            print(f"neochumbyd: {name} takes no taps or button presses from here: its owner turns that on "
                  f"with `neochumbyd set {name} dev=on` (and off with `dev=`)", file=sys.stderr)
            sys.exit(1)
        if cmd == "button" and a[2:] not in (["press"], ["release"], ["tap"]):
            usage()
        with open(os.path.join(state, "fake-input"), "a") as f:
            f.write(" ".join(a) + "\n")
        if a[:4] == ["tap", "kitchen", "160", "220"]:
            say("neochumby: home asks: chumby-mode")
            say("neochumby: CHUMBY MODE")
    else:
        usage()
''').lstrip()


# Stubs of the fetch and build scripts, for Prebuilt's checkout: each logs
# what it was asked to STUB_LOG and does what STUB_FETCH, STUB_BUILD,
# STUB_QEMU_FETCH or STUB_QEMU_BUILD says: ok; noruntime, as a build that
# can't run here (exit 2); else a refusal with that message, as the real
# ones word theirs. dist/fetch --inputs is the real thing's: the last
# commit that changed the image's directory.
STUBS = {
    "dist/fetch": r'''
        image=dist kernel=5.9.16 mode=fetch out=
        while [ $# -gt 0 ]; do
          case $1 in
          --classic) image=classic kernel=2.6.16 ;;
          --kernel) kernel=$2; shift ;;
          --inputs) mode=inputs ;;
          --pathspecs) mode=pathspecs ;;
          *) out=$1 ;;
          esac
          shift
        done
        cd "$(dirname "$0")/.."
        case $mode in
        inputs) git log -1 --format=%H -- "$image"; exit ;;
        pathspecs) echo "$image"; exit ;;
        esac
        echo "fetch $image $kernel ${out#"$PWD/"}" >>"$STUB_LOG"
        [ "$STUB_FETCH" = ok ] || { echo "fetch: $STUB_FETCH" >&2; exit 1; }
        files="sideload-zImage sideload-rootfs.cpio.gz"
        [ "$image" = dist ] || files="classic-zImage classic-rootfs.cpio.gz classic-config"
        mkdir -p "$out"
        for f in $files; do echo "fetched $kernel" >"$out/$f"; done
        echo "fetched the $image image"
    ''',
    "classic/fetch": r'''
        exec "$(dirname "$0")/../dist/fetch" --classic "$@"
    ''',
    "dist/build": r'''
        cd "$(dirname "$0")/.."
        echo "build dist ${NEOCHUMBY_DIST_KERNEL:-5.9.16} ${1#"$PWD/"}" >>"$STUB_LOG"
        case $STUB_BUILD in
        ok) ;;
        noruntime) echo "dist/build: needs Apple's container, docker or podman" >&2; exit 2 ;;
        *) exit 1 ;;
        esac
        mkdir -p "$1"
        for f in sideload-zImage sideload-rootfs.cpio.gz; do echo "built ${NEOCHUMBY_DIST_KERNEL:-5.9.16}" >"$1/$f"; done
    ''',
    "classic/build": r'''
        cd "$(dirname "$0")/.."
        echo "build classic 2.6.16 ${1#"$PWD/"}" >>"$STUB_LOG"
        case $STUB_BUILD in
        ok) ;;
        noruntime) echo "classic/build: needs Apple's container, docker or podman" >&2; exit 2 ;;
        *) exit 1 ;;
        esac
        mkdir -p "$1"
        for f in classic-zImage classic-rootfs.cpio.gz classic-config; do echo built >"$1/$f"; done
    ''',
    "emu/qemu/fetch": r'''
        here=$(cd "$(dirname "$0")" && pwd)
        echo "fetch qemu" >>"$STUB_LOG"
        [ "$STUB_QEMU_FETCH" = ok ] || { echo "fetch: $STUB_QEMU_FETCH" >&2; exit 1; }
        mkdir -p "$here/out/obj"
        cp "$STUB_QEMU" "$here/out/obj/qemu-system-arm"
    ''',
    "emu/qemu/build": r'''
        here=$(cd "$(dirname "$0")" && pwd)
        echo "build qemu" >>"$STUB_LOG"
        case $STUB_QEMU_BUILD in
        ok) ;;
        noruntime) echo "emu/qemu/build: needs pkg-config" >&2; exit 2 ;;
        *) exit 1 ;;
        esac
        mkdir -p "$here/out/obj"
        cp "$STUB_QEMU" "$here/out/obj/qemu-system-arm"
    ''',
}


class Prebuilt(unittest.TestCase):
    """What dev/emulate fetches, builds or uses as it is (dev/prebuilt.py),
    in a checkout of its own: dev/emulate, and stubs of the fetch and build
    scripts, in a git repository that pins a qemu-chumby commit."""

    PIN = "da1a67884b564206fd218ff3c29144b25b2160f7"

    def git(self, *args):
        subprocess.run(["git", "-C", self.repo, "-c", "user.name=t", "-c", "user.email=t@example.com",
                        "-c", "commit.gpgsign=false", *args], check=True, capture_output=True)

    def commit(self, path, text):
        self.write(path, text)
        self.git("add", path)
        self.git("commit", "-qm", path)

    def write(self, path, text, mode=0o644):
        path = os.path.join(self.repo, path)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
        os.chmod(path, mode)

    def setUp(self):
        self.t = tempfile.mkdtemp(prefix="prebuilt.")
        self.repo = os.path.join(self.t, "repo")
        os.makedirs(os.path.join(self.repo, "dev"))
        # dev/emulate takes its checkout from where it's run from.
        for f in ("emulate", "e2e", "device.py", "prebuilt.py", "emulate-userhook2"):
            os.symlink(os.path.join(HERE, f), os.path.join(self.repo, "dev", f))
        for path, body in STUBS.items():
            self.write(path, "#!/bin/sh\n" + textwrap.dedent(body).lstrip(), 0o755)
        self.write("dist/Config.in", "one\n")
        self.write("classic/linux.fragment", "one\n")
        self.git("init", "-q")
        self.git("add", "-A")
        self.git("update-index", "--add", "--cacheinfo", f"160000,{self.PIN},emu/qemu/qemu-chumby")
        self.git("commit", "-qm", "the checkout")
        self.qemu = os.path.join(self.t, "qemu")
        with open(self.qemu, "w") as f:
            f.write(STUB)
        os.chmod(self.qemu, 0o755)
        self.log = os.path.join(self.t, "log")
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("NEOCHUMBY_EMU", "NEOCHUMBY_DIST", "NEOCHUMBY_BUILD", "NEOCHUMBY_NO_"))
                    and k != "QEMU"}
        self.env.update(STUB_LOG=self.log, STUB_QEMU=self.qemu, STUB_EVENTS=os.path.join(self.t, "events"),
                        STUB_FETCH="ok", STUB_BUILD="ok", STUB_QEMU_FETCH="ok", STUB_QEMU_BUILD="ok",
                        CHUMBY_MACHINE_PATH=os.path.dirname(cm.__file__))
        self.state = os.path.join(self.t, "s")

    def tearDown(self):
        self.emu("stop")
        subprocess.run(["rm", "-rf", self.t])

    def emu(self, *args, profile="dist"):
        return subprocess.run([os.path.join(self.repo, "dev", "emulate"), "--profile", profile,
                               "--state", self.state, *args], env=self.env, capture_output=True,
                              text=True, timeout=60)

    def asked(self):
        """What the stubs were asked since last time."""
        try:
            with open(self.log) as f:
                lines = f.read().splitlines()
        except FileNotFoundError:
            return []
        os.unlink(self.log)
        return lines

    def read(self, path):
        with open(os.path.join(self.repo, path)) as f:
            return f.read()

    def prepare(self, *args, profile="dist", want=0):
        r = self.emu("prepare", *args, profile=profile)
        self.assertEqual(r.returncode, want, r.stdout + r.stderr)
        return r

    def test_fetches_then_uses_what_it_fetched(self):
        r = self.prepare()
        self.assertEqual(self.asked(), ["fetch qemu", "fetch dist 5.9.16 build/sideload-dist"])
        self.assertEqual(self.read("build/sideload-dist/sideload-zImage"), "fetched 5.9.16\n")
        self.assertIn("ready: dist boots", r.stdout)
        # Current: nothing fetched or built again.
        r = self.prepare()
        self.assertEqual(self.asked(), [])
        self.assertNotIn("fetching", r.stderr)

    def test_falls_back_to_building(self):
        self.env["STUB_FETCH"] = "no prebuilt dist image for inputs 123 (5.9.16): CI publishes"
        self.env["STUB_QEMU_FETCH"] = "the prebuilt QEMU doesn't run here:"
        r = self.prepare()
        self.assertEqual(self.asked(), ["fetch qemu", "build qemu",
                                        "fetch dist 5.9.16 build/sideload-dist", "build dist 5.9.16 build/sideload-dist"])
        self.assertIn("QEMU: the prebuilt binary doesn't run here; building from source (emu/qemu/build)", r.stderr)
        self.assertIn("the dist image (5.9.16): no prebuilt for these inputs (or no network); "
                      "building from source (dist/build build/sideload-dist)", r.stderr)
        self.assertEqual(self.read("build/sideload-dist/sideload-zImage"), "built 5.9.16\n")
        self.prepare()
        self.assertEqual(self.asked(), [], "what it built is current")

    def test_new_inputs(self):
        self.prepare()
        self.asked()
        self.commit("dist/Config.in", "two\n")
        self.prepare()
        self.assertEqual(self.asked(), ["fetch dist 5.9.16 build/sideload-dist"])
        # A new qemu-chumby pinned.
        self.git("update-index", "--cacheinfo", f"160000,{'1' * 40},emu/qemu/qemu-chumby")
        self.git("commit", "-qm", "qemu-chumby")
        self.prepare()
        self.assertEqual(self.asked(), ["fetch qemu"])
        # Changes elsewhere leave the image's inputs as they were.
        self.commit("README.md", "hello\n")
        self.prepare()
        self.assertEqual(self.asked(), [])

    def test_local_changes_are_built(self):
        self.prepare()
        self.asked()
        self.write("dist/Config.in", "mine\n")
        r = self.prepare()
        self.assertEqual(self.asked(), ["build dist 5.9.16 build/sideload-dist"], "no prebuilt matches them")
        self.assertIn("its inputs have local changes; building from source", r.stderr)
        self.prepare()
        self.assertEqual(self.asked(), [], "built from these changes already")
        self.write("dist/new-package", "new\n")
        self.prepare()
        self.assertEqual(self.asked(), ["build dist 5.9.16 build/sideload-dist"], "a new file is a change")
        self.write("dist/new-package", "newer\n")
        self.prepare()
        self.assertEqual(self.asked(), ["build dist 5.9.16 build/sideload-dist"])
        os.unlink(os.path.join(self.repo, "dist/new-package"))
        self.write("dist/Config.in", "one\n")
        self.prepare()
        self.assertEqual(self.asked(), ["fetch dist 5.9.16 build/sideload-dist"], "the committed inputs again")

    def test_build_and_no_fetch(self):
        self.prepare()
        self.asked()
        r = self.prepare("--build")
        self.assertEqual(self.asked(), ["build qemu", "build dist 5.9.16 build/sideload-dist"],
                         "what was fetched is built")
        self.assertIn("QEMU: --build; building from source", r.stderr)
        self.prepare("--build")
        self.assertEqual(self.asked(), [], "what was built from these inputs stays")
        self.prepare()
        self.assertEqual(self.asked(), [], "and is current without --build too")
        self.commit("dist/Config.in", "two\n")
        self.env["NEOCHUMBY_NO_FETCH"] = "1"
        self.prepare()
        self.assertEqual(self.asked(), ["build dist 5.9.16 build/sideload-dist"])
        del self.env["NEOCHUMBY_NO_FETCH"]
        self.commit("dist/Config.in", "three\n")
        self.env["NEOCHUMBY_BUILD"] = "1"
        self.prepare()
        self.assertEqual(self.asked(), ["build dist 5.9.16 build/sideload-dist"])

    def test_skipped_when_it_cant_be_built(self):
        self.env.update(STUB_FETCH="gh isn't logged in, which verifying the prebuilt image's attestation needs:",
                        STUB_BUILD="noruntime")
        r = self.prepare(want=77)
        self.assertIn("skipped: the dist profile needs the dist image (dist/fetch, else dist/build): "
                      "the dist image (5.9.16): not fetched (gh isn't logged in, which verifying the "
                      "prebuilt needs), and it can't be built here", r.stdout)
        self.assertIn("dist/build: needs Apple's container", r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.repo, "build/sideload-dist/.neochumby-inputs")))
        # Stale, and neither can make it current: not booted.
        self.env.update(STUB_FETCH="ok", STUB_BUILD="ok")
        self.prepare()
        self.commit("dist/Config.in", "two\n")
        self.env.update(STUB_FETCH="no prebuilt", STUB_BUILD="noruntime")
        r = self.prepare(want=77)
        self.assertIn("build/sideload-dist is there, but made from other inputs", r.stdout)

    def test_qemu_skipped_when_it_cant_be_built(self):
        if shutil.which("qemu-system-arm"):
            self.skipTest("a qemu-system-arm on PATH stands in")
        self.env.update(STUB_QEMU_FETCH="no prebuilt QEMU for qemu-chumby 123 on linux-x86_64",
                        STUB_QEMU_BUILD="noruntime")
        r = self.prepare(want=77)
        self.assertIn("no qemu-system-arm with the chumby-classic machine: QEMU: not fetched "
                      "(no prebuilt for these inputs (or no network)), and it can't be built here", r.stdout)
        self.assertEqual(self.asked(), ["fetch qemu", "build qemu"], "no image made without QEMU")

    def test_a_failed_build_is_an_error(self):
        self.env.update(STUB_FETCH="no prebuilt", STUB_BUILD="fail")
        r = self.prepare(want=1)
        self.assertIn("dist/build build/sideload-dist failed (exit 1)", r.stderr)

    def test_made_by_hand_is_used_as_it_is(self):
        self.write("build/sideload-dist/sideload-zImage", "mine\n")
        r = self.prepare()
        self.assertEqual(self.asked(), ["fetch qemu"])
        self.assertIn("using build/sideload-dist as it is", r.stderr)
        self.prepare()
        self.asked()
        # Rebuilt by hand after dev/emulate fetched it: by hand again.
        os.unlink(os.path.join(self.repo, "build/sideload-dist/sideload-zImage"))
        self.prepare()
        self.assertEqual(self.asked(), ["fetch dist 5.9.16 build/sideload-dist"])
        self.write("build/sideload-dist/sideload-zImage", "mine, longer\n")
        self.prepare()
        self.assertEqual(self.asked(), [])
        self.assertEqual(self.read("build/sideload-dist/sideload-zImage"), "mine, longer\n")
        # Unless building is asked for.
        self.prepare("--build")
        self.assertEqual(self.asked(), ["build qemu", "build dist 5.9.16 build/sideload-dist"],
                         "QEMU was fetched, too")

    def test_given_images_and_qemu_are_never_made(self):
        kernel = os.path.join(self.t, "zImage")
        open(kernel, "w").close()
        self.prepare("--kernel", kernel, "--qemu", self.qemu)
        self.env["NEOCHUMBY_EMU_DIST_KERNEL"] = kernel
        self.env["QEMU"] = self.qemu
        self.prepare()
        self.assertEqual(self.asked(), [])

    def test_stock_images_are_never_fetched(self):
        r = self.prepare(profile="stock", want=77)
        self.assertIn("needs your own firmware", r.stdout)
        self.assertEqual(self.asked(), [])

    def test_kernel_and_classic(self):
        self.env["NEOCHUMBY_DIST_KERNEL"] = "stable"
        self.env["STUB_FETCH"] = "no prebuilt"
        self.prepare()
        self.assertEqual(self.asked(), ["fetch qemu", "fetch dist stable build/sideload-dist-stable",
                                        "build dist stable build/sideload-dist-stable"])
        self.assertEqual(self.read("build/sideload-dist-stable/sideload-zImage"), "built stable\n")
        self.env["STUB_FETCH"] = "ok"
        self.prepare(profile="classic")
        self.assertEqual(self.asked(), ["fetch classic 2.6.16 build/classic"])
        self.commit("dist/Config.in", "two\n")
        self.prepare(profile="classic")
        self.assertEqual(self.asked(), [], "dist's inputs aren't classic's")

    def test_prepared(self):
        """After dev/e2e's prepare, start neither fetches nor builds."""
        self.env["NEOCHUMBY_EMU_PREPARED"] = "1"
        r = self.prepare(want=77)
        self.assertEqual(self.asked(), [])
        self.assertIn("no emu/qemu/out/obj/qemu-system-arm", r.stdout)
        self.env["QEMU"] = self.qemu
        r = self.prepare(want=77)
        self.assertIn("no build/sideload-dist/sideload-zImage", r.stdout)
        self.assertEqual(self.asked(), [])

    def test_start_boots_what_it_fetched(self):
        r = self.emu("start", fake_tree(os.path.join(self.t, "tree")))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        with open(os.path.join(self.state, "argv.json")) as f:
            a = json.load(f)
        self.assertEqual(self.asked(), ["fetch qemu", "fetch dist 5.9.16 build/sideload-dist"])
        self.assertEqual(a[a.index("-kernel") + 1], os.path.join(self.repo, "build/sideload-dist/sideload-zImage"))

    def test_e2e_prepares_once(self):
        tree = fake_tree(os.path.join(self.t, "tree"))
        self.env["E2E_SCENARIOS"] = "none"
        r = subprocess.run([os.path.join(self.repo, "dev", "e2e"), "--no-fetch", tree, tree,
                            os.path.join(self.t, "e2e")], env=self.env, capture_output=True, text=True,
                           timeout=60)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.asked(), ["build qemu", "build dist 5.9.16 build/sideload-dist"])
        self.env.update(STUB_FETCH="no prebuilt", STUB_BUILD="fail")
        self.commit("dist/Config.in", "two\n")
        r = subprocess.run([os.path.join(self.repo, "dev", "e2e"), tree, tree, os.path.join(self.t, "e2e")],
                           env=self.env, capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 1)
        self.assertIn("dev/emulate prepare failed", r.stdout)


# gh, for DistFetch: logged in, and its attestation verify passes but for
# the files named in STUB_UNSIGNED.
GH_STUB = r'''#!/bin/sh
case $1 in
auth) exit 0 ;;
attestation)
  case " $STUB_UNSIGNED " in *" $(basename "$3") "*) echo "no attestation" >&2; exit 1 ;; esac
  echo "verify $(basename "$3")" >>"$STUB_LOG"; exit 0 ;;
esac
exit 1
'''


class DistFetch(unittest.TestCase):
    """The real dist/fetch, in a checkout of its own, against a file://
    mirror of the release and a stub gh: mainline falls back to the newest
    mainline build when its inputs have none; the others never do."""

    def git(self, *args):
        return subprocess.run(["git", "-C", self.repo, "-c", "user.name=t", "-c", "user.email=t@example.com",
                               "-c", "commit.gpgsign=false", *args], check=True, capture_output=True,
                              text=True).stdout.strip()

    def commit(self, text, path="dist/Config.in", days_ago=0):
        """A commit of TEXT to PATH, its committer time DAYS_AGO."""
        os.makedirs(os.path.dirname(os.path.join(self.repo, path)), exist_ok=True)
        with open(os.path.join(self.repo, path), "w") as f:
            f.write(text)
        self.git("add", path)
        when = f"@{int(time.time() - days_ago * 86400)} +0000"
        subprocess.run(["git", "-C", self.repo, "-c", "user.name=t", "-c", "user.email=t@example.com",
                        "-c", "commit.gpgsign=false", "commit", "-qm", text], check=True, capture_output=True,
                       env=dict(os.environ, GIT_COMMITTER_DATE=when, GIT_AUTHOR_DATE=when))
        return self.git("rev-parse", "HEAD")

    def setUp(self):
        self.t = tempfile.mkdtemp(prefix="fetch.")
        self.repo = os.path.join(self.t, "repo")
        self.mirror = os.path.join(self.t, "mirror")
        bin_ = os.path.join(self.t, "bin")
        for d in (self.repo + "/dist", self.mirror, bin_):
            os.makedirs(d)
        shutil.copy(os.path.join(REPO, "dist", "fetch"), os.path.join(self.repo, "dist", "fetch"))
        with open(os.path.join(self.repo, "dist", "Config.in"), "w") as f:
            f.write("zero\n")
        self.git("init", "-q")
        self.git("add", "-A")
        self.git("commit", "-qm", "the checkout")
        with open(os.path.join(bin_, "gh"), "w") as f:
            f.write(GH_STUB)
        os.chmod(os.path.join(bin_, "gh"), 0o755)
        self.log = os.path.join(self.t, "log")
        self.env = dict(os.environ, PATH=bin_ + os.pathsep + os.environ["PATH"], STUB_LOG=self.log,
                        STUB_UNSIGNED="", NEOCHUMBY_PREBUILT="file://" + self.mirror)
        self.when = time.time() - 1000

    def tearDown(self):
        shutil.rmtree(self.t)

    def publish(self, inputs, kernel, manifest_inputs=None):
        """A build on the mirror, each newer than the last."""
        stem = f"neochumby-dist-{inputs}-{kernel}"
        path = os.path.join(self.mirror, stem + ".tar.xz")
        files = {"MANIFEST": f"image: dist\nkernel: {kernel}\ninputs: {manifest_inputs or inputs}\n",
                 "sideload-zImage": f"zImage {inputs[:7]}\n", "sideload-rootfs.cpio.gz": "rootfs\n"}
        with tarfile.open(path, "w:xz") as tar:
            for name, text in files.items():
                data = text.encode()
                info = tarfile.TarInfo(f"{stem}/{name}")
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
        self.when += 10
        os.utime(path, (self.when, self.when))
        return stem + ".tar.xz"

    def fetch(self, kernel):
        out = os.path.join(self.t, "out")
        r = subprocess.run([os.path.join(self.repo, "dist", "fetch"), "--kernel", kernel, out],
                           env=self.env, capture_output=True, text=True, timeout=60)
        zimage = os.path.join(out, "sideload-zImage")
        if not os.path.exists(zimage):
            return r, None
        with open(zimage) as f:
            return r, f.read().split()[1]

    def test_mainline_for_its_inputs(self):
        old = self.commit("one\n")
        now = self.commit("two\n")
        self.publish(old, "mainline")
        self.publish(now, "mainline")
        r, got = self.fetch("mainline")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(got, now[:7])
        self.assertNotIn("newest", r.stderr)

    def test_mainline_falls_back_to_the_newest(self):
        older = self.commit("one\n")
        newer = self.commit("two\n")
        now = self.commit("three\n")
        self.publish(older, "mainline")
        self.publish(newer, "mainline")
        self.publish(now, "stable")
        r, got = self.fetch("mainline")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(got, newer[:7])
        self.assertIn(f"no prebuilt mainline image for inputs {now}", r.stderr)
        self.assertIn(f"the newest on dist-prebuilt in this checkout's history instead, of inputs {newer}", r.stderr)
        self.assertIn("0 days (committer time) behind HEAD", r.stderr)
        with open(self.log) as f:
            self.assertEqual(f.read().split(), ["verify", f"neochumby-dist-{newer}-mainline.tar.xz"])

    def test_the_fallback_is_checked_too(self):
        older = self.commit("one\n")
        self.commit("two\n")
        name = self.publish(older, "mainline")
        self.env["STUB_UNSIGNED"] = name
        r, got = self.fetch("mainline")
        self.assertEqual(r.returncode, 1)
        self.assertIn("isn't attested", r.stderr)
        self.assertIsNone(got)
        self.env["STUB_UNSIGNED"] = ""
        self.publish(older, "mainline", manifest_inputs="0" * 40)
        r, got = self.fetch("mainline")
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"doesn't say \"inputs: {older}\"", r.stderr)
        self.assertIsNone(got)

    def test_no_fallback_for_the_others_or_local_changes(self):
        older = self.commit("one\n")
        self.commit("two\n")
        self.publish(older, "stable")
        self.publish(older, "mainline")
        r, got = self.fetch("stable")
        self.assertEqual(r.returncode, 1)
        self.assertIn("no prebuilt dist image for inputs", r.stderr)
        self.assertIsNone(got)
        with open(os.path.join(self.repo, "dist", "Config.in"), "w") as f:
            f.write("mine\n")
        r, got = self.fetch("mainline")
        self.assertEqual(r.returncode, 1)
        self.assertIn("local changes", r.stderr)
        self.assertIsNone(got)

    def test_only_from_this_checkouts_history(self):
        base = self.commit("one\n")
        elsewhere = self.commit("theirs\n")
        self.git("reset", "-q", "--hard", base)
        self.commit("ours\n")
        self.publish(elsewhere, "mainline")
        r, got = self.fetch("mainline")
        self.assertEqual(r.returncode, 1)
        self.assertIn("none of the mainline builds on dist-prebuilt is of a commit in this checkout's history",
                      r.stderr)
        self.assertIn("or wait for the weekly build", r.stderr)
        self.assertIsNone(got)
        # A newer one elsewhere is passed over for one in the history.
        self.publish(base, "mainline")
        self.publish(elsewhere, "mainline")
        r, got = self.fetch("mainline")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(got, base[:7])

    def test_only_a_recent_one(self):
        old = self.commit("one\n", days_ago=20)
        self.commit("two\n")
        self.publish(old, "mainline")
        r, got = self.fetch("mainline")
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"of inputs {old}, is 20 days behind HEAD (more than 14)", r.stderr)
        self.assertIsNone(got)
        recent = self.commit("three\n", days_ago=10)
        self.commit("four\n")
        self.publish(recent, "mainline")
        r, got = self.fetch("mainline")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(got, recent[:7])
        self.assertIn("10 days (committer time) behind HEAD", r.stderr)

    def test_only_one_with_the_current_pin(self):
        before = self.commit("one\n")
        pin = self.commit("hash\n", path="dist/kernel/mainline/linux/linux.hash")
        self.publish(before, "mainline")
        r, got = self.fetch("mainline")
        self.assertEqual(r.returncode, 1)
        self.assertIn(f"predates the mainline pin's last change ({pin})", r.stderr)
        self.assertIsNone(got)

    def test_none_at_all(self):
        self.commit("one\n")
        r, got = self.fetch("mainline")
        self.assertEqual(r.returncode, 1)
        self.assertIn("no prebuilt dist image for inputs", r.stderr)


class Device(unittest.TestCase):
    def setUp(self):
        self.t = tempfile.mkdtemp(prefix="emu.")
        self.nd = os.path.join(self.t, "nd")
        os.makedirs(os.path.join(self.nd, "tree"))
        exe = os.path.join(self.nd, "neochumbyd")
        with open(exe, "w") as f:
            f.write(FAKE_NEOCHUMBYD)
        os.chmod(exe, 0o755)
        with open(os.path.join(self.nd, "fake-log"), "w") as f:
            f.write("neochumby: opening home\nneochumby: CHUMBY MODE\n")
        self.tree = os.path.join(self.t, "tree")
        os.makedirs(self.tree)
        with open(os.path.join(self.tree, "neochumby"), "w") as f:
            f.write("session\n")
        self.env = dict(os.environ, NEOCHUMBYD_DIR=self.nd, NEOCHUMBY_EMU_DEVICE="kitchen")
        for k in ("NEOCHUMBY_EMU_PROFILE", "NEOCHUMBY_EMU_HOST", "NEOCHUMBY_EMU", "NEOCHUMBY_EMU_SLOW"):
            self.env.pop(k, None)
        self.state = os.path.join(self.t, "s")

    def tearDown(self):
        self.emu("stop")
        subprocess.run(["rm", "-rf", self.t])

    def emu(self, *args, env=None):
        return subprocess.run([EMULATE, "--state", self.state, *args], env=env or self.env, capture_output=True,
                              text=True, timeout=60)

    def test_backends(self):
        self.assertEqual(self.emu("backend").stdout.strip(), "device")
        self.assertEqual(self.emu("scale").stdout.strip(), "5")
        emulated = {k: v for k, v in self.env.items() if k != "NEOCHUMBY_EMU_DEVICE"}
        self.assertEqual(self.emu("backend", env=emulated).stdout.strip(), "dist")
        self.assertEqual(self.emu("prompt", "pull the power", env=emulated).returncode, 77)
        for profile in ("stock", "dist", "classic"):
            env = dict(emulated, NEOCHUMBY_EMU_PROFILE=profile)
            self.assertEqual(self.emu("backend", env=env).stdout.strip(), profile)
            self.assertEqual(self.emu("scale", env=env).stdout.strip(), str(emulate.SCALE[profile]))
        self.assertNotEqual(self.emu("backend", env=dict(emulated, NEOCHUMBY_EMU_PROFILE="user")).returncode, 0)

    def test_unpaired(self):
        r = self.emu("start", self.tree, env=dict(self.env, NEOCHUMBY_EMU_DEVICE="lounge"))
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("no Chumby paired as lounge", r.stderr)

    def test_a_session(self):
        r = self.emu("start", self.tree)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        with open(os.path.join(self.nd, "tree", "neochumby")) as f:
            self.assertEqual(f.read(), "session\n", "the tree was installed")
        self.assertEqual(self.emu("wait", "log", "opening home", "10").returncode, 0)
        # Only the restart's line: the history before it was let pass.
        self.assertEqual(self.emu("log").stdout, "neochumby: opening home\n")
        self.assertIn("kitchen, a Chumby", self.emu("status").stdout)
        self.assertEqual(self.emu("pixel", "5", "5").stdout.strip(), "#101021")
        self.assertEqual(self.emu("exec", "/bin/true").returncode, 77)
        # dev/emulate's press, down and up are neochumbyd's tap, press and release.
        for how in ("press", "down", "up"):
            self.assertEqual(self.emu("button", how).returncode, 0)
        self.assertEqual(self.emu("tap", "160", "220", "--hold", "150").returncode, 0)
        with open(os.path.join(self.nd, "fake-input")) as f:
            self.assertEqual(f.read(), "button kitchen tap\nbutton kitchen press\nbutton kitchen release\n"
                                       "tap kitchen 160 220 --hold 150\n")
        self.assertEqual(self.emu("wait", "exit", "10").stdout.strip(), "exit 0")

    def test_without_remote_input(self):
        self.assertEqual(self.emu("start", self.tree).returncode, 0)
        env = dict(self.env, FAKE_WITHOUT_INPUT="1")
        r = self.emu("tap", "1", "1", env=env)
        self.assertEqual(r.returncode, 77)
        self.assertIn("no `tap` yet", r.stdout)
        self.assertEqual(self.emu("button", "press", env=env).returncode, 77)

    def test_input_refused(self):
        """A Chumby not set dev=on: a failure, saying so, not a skip."""
        self.assertEqual(self.emu("start", self.tree).returncode, 0)
        r = self.emu("tap", "1", "1", env=dict(self.env, FAKE_DEV_OFF="1"))
        self.assertEqual(r.returncode, 1)
        self.assertIn("neochumbyd set kitchen dev=on", r.stderr)
        self.assertNotIn("(is kitchen set dev=on?)", r.stderr)

    def test_png(self):
        import zlib
        # Each of PNG's filters, on rows that need them.
        rows = b""
        for y in range(240):
            rows += bytes([y % 5]) + bytes([(x * 7 + y) & 0xFF for x in range(320 * 3)])
        png = b"\x89PNG\r\n\x1a\n"
        for t, d in ((b"IHDR", (320).to_bytes(4, "big") + (240).to_bytes(4, "big") + bytes([8, 2, 0, 0, 0])),
                     (b"IDAT", zlib.compress(rows)), (b"IEND", b"")):
            png += len(d).to_bytes(4, "big") + t + d + zlib.crc32(t + d).to_bytes(4, "big")
        import device
        out = device.png_to_rgb565(png)
        self.assertEqual(len(out), 320 * 240 * 2)
        # Row 0 is unfiltered: its first pixel is (0, 7, 14).
        self.assertEqual(int.from_bytes(out[:2], "little"), (0 >> 3) << 11 | (7 >> 2) << 5 | 14 >> 3)
        # And chumby_machine's PNG reader agrees with device's.
        rgb, w, h = cm.png_rgb(png)
        self.assertEqual(cm.rgb_to_rgb565(rgb), list(memoryview(out).cast("H")))


if __name__ == "__main__":
    unittest.main()
