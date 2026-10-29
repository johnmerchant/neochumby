#!/usr/bin/env python3
"""Run the stock 2.6.16 -> 5.9.16 chumpwn demo in QEMU and capture evidence.

The stock kernel still boots from NAND (it has no other boot path).  The
5.9.16 zImage and initramfs are loaded from outside NAND by QEMU
-device loader at 0xc2000000; the stock kernel does not overwrite that
reserved area before rcS.  The 5.9.16 kernel is built with CONFIG_MTD=n.

Both the original NAND and the scratch image are hashed before and after;
QEMU uses -snapshot and the drive uses snapshot=on.
"""
import hashlib
import os
import select
import socket
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
def env(name, default=None):
    v = os.environ.get(name, default)
    if v is None:
        sys.exit(f"set {name}")
    return v

def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()

# chumpwn must never open an MTD device.  The stock 2.6.16 stage reads NAND
# to boot; the new OS has no MTD driver at all.  This static check catches a
# regression in the executable before QEMU is started.
MTD_STRINGS = (b"/dev/mtd", b"mtdblock", b"flash_erase", b"flashcp", b"nandwrite", b"mtd", b"nand")

def assert_no_mtd_strings(path):
    with open(path, "rb") as f:
        data = f.read()
    found = [s.decode() for s in MTD_STRINGS if s in data]
    if found:
        sys.exit(f"{path}: contains MTD write-path strings: {', '.join(found)}")
    print(f"{path}: no MTD/NAND device strings", flush=True)

def main():
    qemu = env("QEMU", "qemu-system-arm")
    stock = env("STOCK_ZIMAGE")
    nand_src = env("NAND_SRC")
    chumby_emu = env("CHUMBY_EMU", "chumby-emu")
    chumpwn = env("CHUMPWN", os.path.join(HERE, "chumpwn"))
    payload = env("PAYLOAD", os.path.join(HERE, "build", "payload.bin"))
    work = env("WORK", os.path.join(HERE, "build", "demo"))
    os.makedirs(work, exist_ok=True)

    nand_before = sha256(nand_src)
    assert_no_mtd_strings(chumpwn)

    # A clean PSP tree: only chumpwn and the rcS hook.  This writes a
    # separate scratch NAND image (host-side image preparation); the
    # original is opened read-only and hashed before/after.
    psp_dir = os.path.join(work, "psp")
    os.makedirs(os.path.join(psp_dir, "rfs1"), exist_ok=True)
    rcS = (
        "#!/bin/sh\n"
        "echo \"chumpwn rcS: start\"\n"
        "ifconfig lo 127.0.0.1 up\n"
        f"{os.path.join('/psp', 'chumpwn')} --source 0xc2000000\n"
        "echo \"chumpwn rcS: app returned $?\"\n"
        "exec /bin/sh\n"
    )
    for token in ("mtd", "nand", "flash_erase", "flashcp", "nandwrite"):
        if token in rcS.lower():
            sys.exit(f"generated rcS contains forbidden token: {token}")
    with open(os.path.join(psp_dir, "rfs1", "rcS"), "w") as f:
        f.write(rcS)
    os.chmod(os.path.join(psp_dir, "rfs1", "rcS"), 0o755)
    with open(chumpwn, "rb") as src, open(os.path.join(psp_dir, "chumpwn"), "wb") as dst:
        dst.write(src.read())
    os.chmod(os.path.join(psp_dir, "chumpwn"), 0o755)
    psp_jffs2 = os.path.join(work, "psp.jffs2")
    subprocess.run([chumby_emu, "mkpsp", psp_dir, psp_jffs2], check=True)
    scratch = os.path.join(work, "nand-scratch.img")
    subprocess.run(
        [sys.executable, os.path.join(HERE, "mk_psp_nand.py"), nand_src, psp_jffs2, scratch],
        check=True,
    )
    scratch_before = sha256(scratch)

    sock_path = os.path.join(work, "serial.sock")
    try:
        os.unlink(sock_path)
    except FileNotFoundError:
        pass
    log_path = env("LOG", os.path.join(work, "chumpwn-demo.log"))
    log = open(log_path, "wb")
    qemu_args = [
        qemu, "-M", "chumby-classic", "-kernel", stock,
        "-drive", f"if=mtd,file={scratch},format=raw,snapshot=on",
        "-device", f"loader,file={payload},addr=0xc2000000,force-raw=on",
        "-snapshot", "-display", "none", "-no-reboot", "-monitor", "none",
        "-serial", f"unix:{sock_path},server=on,wait=off",
        "-append",
        "console=ttyS0,38400 root=/dev/mtdblock5 rootfstype=cramfs "
        "init=/linuxrc chumby_hwversion=3.8",
        "-icount", "shift=auto,sleep=off",
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
            sys.stdout.buffer.write(d)
            sys.stdout.buffer.flush()

    def wait_for(needle, timeout):
        end = time.time() + timeout
        while time.time() < end:
            pump(0.2)
            if needle in bytes(buf):
                return True
        return False

    def send(line):
        for i in range(0, len(line), 16):
            s.sendall(line[i:i + 16].encode())
            time.sleep(0.02)
        time.sleep(0.05)
        s.sendall(b"\r")
        pump(0.6)

    ok = wait_for(b"chumpwn# ", 120)
    print("\nchumpwn: got 5.9.16 shell" if ok else "\nchumpwn: NO 5.9.16 SHELL", file=sys.stderr)
    if ok:
        time.sleep(0.5)
        for cmd in [
            "uname -a",
            "cat /proc/version",
            "cat /proc/cmdline",
            "cat /proc/mtd; echo MTD_RC=$?",
            "ls -l /dev/mtd*; echo MTD_DEV_RC=$?",
            "dmesg | grep -i nand; echo NAND_GREP_RC=$?",
            "ls -l /sys/class/mtd; echo SYS_MTD_RC=$?",
            "echo CHUMPWN_SHELL_OK",
        ]:
            send(cmd)
        pump(2)
        text = bytes(buf)
        guest_checks = [
            (b"MTD_RC=1", "no /proc/mtd"),
            (b"MTD_DEV_RC=1", "no /dev/mtd*"),
            (b"NAND_GREP_RC=1", "no NAND in dmesg"),
            (b"SYS_MTD_RC=1", "no /sys/class/mtd"),
            (b"CHUMPWN_SHELL_OK", "shell marker"),
        ]
        for marker, what in guest_checks:
            if marker not in text:
                print(f"ERROR: 5.9.16 check failed: {what}", file=sys.stderr)
                ok = False
    p.terminate()
    time.sleep(0.5)
    try:
        p.wait(timeout=2)
    except subprocess.TimeoutExpired:
        p.kill()
    log.close()

    nand_after = sha256(nand_src)
    scratch_after = sha256(scratch)
    print("\nchumpwn contains no MTD/NAND device strings (it opens only /dev/mem or user files)")
    print(f"original NAND before: {nand_before}")
    print(f"original NAND after:  {nand_after}")
    print(f"scratch NAND before:  {scratch_before}")
    print(f"scratch NAND after:   {scratch_after}")
    print(f"log: {log_path}")
    if nand_before != nand_after:
        print("ERROR: original NAND changed", file=sys.stderr)
        return 1
    if scratch_before != scratch_after:
        print("ERROR: scratch NAND changed during the run", file=sys.stderr)
        return 1
    return 0 if ok else 2

if __name__ == "__main__":
    sys.exit(main())
