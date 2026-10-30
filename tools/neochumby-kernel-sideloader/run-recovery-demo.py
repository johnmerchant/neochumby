#!/usr/bin/env python3
"""Automated end-to-end test for the sideload recovery path.

Boots the sideload zImage directly (no sideloader, no marker) with a
synthetic zeroed NAND image, on qemu-chumby's machine through its
chumby-machine runner.  The built-in initramfs must see no
`neochumby.sideloaded=1` marker and leave the NAND's drivers unloaded:
only root loads them.  Then root does, on the console (modprobe), and
`/proc/mtd` must list mtd0 and `/dev/mtdblock0` exist.

This needs no stock firmware: the QEMU `chumby-classic` NAND model reports
the chip ID itself, so a zeroed 64 MiB image is enough.  The sideloaded path
is `run-demo.py`, which needs the user's own stock zImage and NAND.
"""
import os
import re
import shutil
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
sys.dont_write_bytecode = True
sys.path.insert(0, os.environ.get("CHUMBY_MACHINE_PATH") or os.path.join(REPO, "emu", "qemu", "qemu-chumby"))
import chumby_machine as cm  # noqa: E402


def env(name, default=None):
    v = os.environ.get(name, default)
    if v is None:
        sys.exit(f"set {name}")
    return v


def main():
    qemu = os.environ.get("QEMU")
    kernel = env("KERNEL", os.path.join(REPO, "build", "sideload-zImage"))
    work = env("WORK", os.path.join(HERE, "build", "recovery-demo"))
    timeout = int(env("TIMEOUT", "120"))
    os.makedirs(work, exist_ok=True)
    if not os.path.exists(kernel):
        sys.exit(f"{kernel}: no sideload zImage (run dist/build first)")

    nand = os.path.join(work, "zero-nand.img")
    with open(nand, "wb") as f:
        f.truncate(cm.NAND_BYTES)
    log_path = os.path.join(work, "recovery.log")
    checks = [
        ("init: no sideload marker; this kernel was flashed", "no marker -> flashed"),
        ("init: NAND modules stay unloaded; root loads them", "NAND modules left to root"),
        ("init: starting shell", "recovery shell started"),
    ]
    # What root's modprobe must give.
    loaded_checks = [
        (r"^mtd0: 04000000 00004000 \"mxc_nand\"$", "/proc/mtd has mtd0"),
        (r"^/dev/mtdblock0$", "/dev/mtdblock0 exists"),
        (r"^mxc_nand ", "mxc_nand module is live"),
    ]
    m = cm.Machine(tempfile.mkdtemp(prefix="nc-recovery-"))
    ok, text, uname, loaded = True, "", "", ""
    try:
        m.start(kernel=kernel, nand=nand, append="console=ttymxc0,115200 rdinit=/init loglevel=7", fast=True,
                qemu=qemu, extra=["-no-reboot"])
        try:
            m.wait_log("~ # ", timeout)
            # The prompt means busybox is up; ask the kernel what it is.
            _, uname = m.exec("uname -a", 30)
            # Nothing loaded the NAND's drivers; root does.
            _, before = m.exec("grep -c '^mxc_nand ' /proc/modules || true", 30)
            if before.strip() != "0":
                print("ERROR: recovery check failed: mxc_nand loaded before root asked", file=sys.stderr)
                ok = False
            _, loaded = m.exec(
                "modprobe cmdlinepart; modprobe mxc_nand && modprobe mtdblock && "
                "cat /proc/mtd && ls /dev/mtdblock0 && grep '^mxc_nand ' /proc/modules",
                60,
            )
        except cm.Error as e:
            print(f"ERROR: recovery check failed: no busybox prompt ({e})", file=sys.stderr)
            ok = False
        text = m.log()
    finally:
        m.stop()
        with open(log_path, "w") as f:
            f.write(m.log() if os.path.exists(m.at(cm.LOG)) else "")
        shutil.rmtree(m.path, ignore_errors=True)
    for marker, what in checks:
        if marker not in text:
            print(f"ERROR: recovery check failed: {what}", file=sys.stderr)
            ok = False
    for pattern, what in loaded_checks:
        if not re.search(pattern, loaded, re.M):
            print(f"ERROR: recovery check failed after root's modprobe: {what}", file=sys.stderr)
            ok = False
    if not re.search(r"^Linux \(none\) (5\.9\.16|7\.2\.13)-mx21-sideload #\d+ .* armv5tejl GNU/Linux$", uname, re.M):
        print("ERROR: recovery check failed: uname -a is not a sideload kernel (5.9.16 or 7.2.13)", file=sys.stderr)
        ok = False
    print(f"sideload recovery demo: {'PASS' if ok else 'FAIL'}")
    print(f"log: {log_path}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
