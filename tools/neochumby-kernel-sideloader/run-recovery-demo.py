#!/usr/bin/env python3
"""Automated end-to-end test for the sideload recovery path.

Boots the sideload zImage directly (no sideloader, no marker) with a
synthetic zeroed NAND image.  The built-in initramfs must see no
`neochumby.sideloaded=1` marker, load the NAND modules, and print
`/proc/mtd` and `/dev/mtdblock0`.

This needs no stock firmware: the QEMU `chumby-classic` NAND model reports
the chip ID itself, so a zeroed 64 MiB image is enough.  The sideloaded path
is `run-demo.py`, which needs the user's own stock zImage and NAND.
"""
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))

def env(name, default=None):
    v = os.environ.get(name, default)
    if v is None:
        sys.exit(f"set {name}")
    return v

def main():
    qemu = env("QEMU", "qemu-system-arm")
    kernel = env("KERNEL", os.path.normpath(os.path.join(HERE, "..", "..", "build", "sideload-zImage")))
    work = env("WORK", os.path.join(HERE, "build", "recovery-demo"))
    timeout = int(env("TIMEOUT", "120"))
    os.makedirs(work, exist_ok=True)

    nand = os.path.join(work, "zero-nand.img")
    with open(nand, "wb") as f:
        f.truncate(0x4200000)
    log_path = os.path.join(work, "recovery.log")
    if os.path.exists(log_path):
        os.unlink(log_path)

    if not os.path.exists(kernel):
        sys.exit(f"{kernel}: no sideload zImage (run dist/build first)")
    qemu_args = [
        qemu, "-M", "chumby-classic", "-kernel", kernel,
        "-drive", f"if=mtd,file={nand},format=raw,snapshot=on",
        "-snapshot", "-display", "none", "-no-reboot", "-monitor", "none",
        "-serial", f"file:{log_path}",
        "-append", "console=ttymxc0,115200 rdinit=/init loglevel=7",
        "-icount", "shift=auto,sleep=off",
    ]
    p = subprocess.Popen(qemu_args, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    checks = [
        (b"init: no sideload marker; this kernel was flashed", "no marker -> flashed"),
        (b"init: loading the NAND recovery modules", "NAND modules loaded"),
        (b"5.9.16-mx21-sideload", "sideload kernel version"),
        (b"init: /proc/mtd: mtd0:", "/proc/mtd has mtd0"),
        (b"init: /dev/mtdblock0", "/dev/mtdblock0 exists"),
        (b"init: module mxc_nand loaded", "mxc_nand module is live"),
        (b"init: starting shell", "recovery shell started"),
    ]
    ok = False
    deadline = time.time() + timeout
    try:
        while time.time() < deadline:
            time.sleep(0.5)
            if not os.path.exists(log_path):
                continue
            with open(log_path, "rb") as f:
                text = f.read()
            if all(marker in text for marker, _ in checks):
                ok = True
                break
            if p.poll() is not None:
                break
    finally:
        p.terminate()
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill()
    with open(log_path, "rb") as f:
        text = f.read()
    for marker, what in checks:
        if marker not in text:
            print(f"ERROR: recovery check failed: {what}", file=sys.stderr)
            ok = False
    print(f"sideload recovery demo: {'PASS' if ok else 'FAIL'}")
    print(f"log: {log_path}")
    return 0 if ok else 1

if __name__ == "__main__":
    sys.exit(main())
