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
import json
import os
import subprocess
import sys
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
        r = self.emu("start", self.tree, "--qemu", self.stub)
        self.assertEqual(r.returncode, 77)
        self.assertIn("needs the dist image", r.stdout)

    def test_no_network(self):
        r = self.emu("start", self.tree, "--kernel", self.kernel, "--qemu", self.stub, "--server", "127.0.0.1:1")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("no network", r.stderr)

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
