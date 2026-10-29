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
import re
import select
import shutil
import socket
import subprocess
import sys
import tempfile
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
    # A unix socket's path is limited to about 100 bytes, so not under work.
    sock_dir = tempfile.mkdtemp(prefix="nc-")
    sock_path = os.path.join(sock_dir, "serial.sock")
    if os.path.exists(log_path):
        os.unlink(log_path)

    if not os.path.exists(kernel):
        sys.exit(f"{kernel}: no sideload zImage (run dist/build first)")
    qemu_args = [
        qemu, "-M", "chumby-classic", "-kernel", kernel,
        "-drive", f"if=mtd,file={nand},format=raw,snapshot=on",
        "-snapshot", "-display", "none", "-no-reboot", "-monitor", "none",
        "-serial", f"unix:{sock_path},server=on,wait=off",
        "-append", "console=ttymxc0,115200 rdinit=/init loglevel=7",
        "-icount", "shift=auto,sleep=off",
    ]
    checks = [
        (b"init: no sideload marker; this kernel was flashed", "no marker -> flashed"),
        (b"init: loading the NAND recovery modules", "NAND modules loaded"),
        (b"init: /proc/mtd: mtd0:", "/proc/mtd has mtd0"),
        (b"init: /dev/mtdblock0", "/dev/mtdblock0 exists"),
        (b"init: module mxc_nand loaded", "mxc_nand module is live"),
        (b"init: starting shell", "recovery shell started"),
    ]
    p = subprocess.Popen(qemu_args, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    for _ in range(200):
        if os.path.exists(sock_path):
            break
        time.sleep(0.05)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(sock_path)
    s.setblocking(False)
    buf = bytearray()
    log = open(log_path, "wb")

    def pump(seconds):
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([s], [], [], 0.1)
            if not r:
                continue
            try:
                d = s.recv(65536)
            except BlockingIOError:
                continue
            if not d:
                break
            buf.extend(d)
            log.write(d)
            log.flush()

    def wait_for(needle, seconds):
        end = time.time() + seconds
        while time.time() < end and p.poll() is None:
            pump(0.2)
            if needle in bytes(buf):
                return True
        return needle in bytes(buf)

    ok = wait_for(b"~ # ", timeout)
    if not ok:
        print("ERROR: recovery check failed: no busybox prompt", file=sys.stderr)
    else:
        # The prompt means busybox is up; ask the kernel what it is.
        s.sendall(b"uname -a\r")
        pump(2)
    p.terminate()
    try:
        p.wait(timeout=5)
    except subprocess.TimeoutExpired:
        p.kill()
    log.close()
    shutil.rmtree(sock_dir, ignore_errors=True)
    text = bytes(buf)
    for marker, what in checks:
        if marker not in text:
            print(f"ERROR: recovery check failed: {what}", file=sys.stderr)
            ok = False
    if not re.search(rb"^Linux \(none\) 5\.9\.16-mx21-sideload #\d+ .* armv5tejl GNU/Linux\r?$",
                     text, re.M):
        print("ERROR: recovery check failed: uname -a is not the sideload 5.9.16 kernel",
              file=sys.stderr)
        ok = False
    print(f"sideload recovery demo: {'PASS' if ok else 'FAIL'}")
    print(f"log: {log_path}")
    return 0 if ok else 1

if __name__ == "__main__":
    sys.exit(main())
