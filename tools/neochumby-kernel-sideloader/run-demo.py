#!/usr/bin/env python3
"""Run the stock 2.6.16 -> sideload Linux demo on qemu-chumby's machine,
through its chumby-machine runner.

The stock kernel still boots from NAND (it has no other boot path).  The
sideload zImage and initramfs are loaded from outside NAND by QEMU
-device loader at 0xc2000000; the stock kernel does not overwrite that
reserved area before rcS.  The demo works for 5.9.16 and for the dist's
7.x images; neither loads NAND in the sideloaded path.

Both the original NAND and the scratch image are hashed before and after;
the drive uses snapshot=on.
"""
import hashlib
import os
import re
import shutil
import subprocess
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

def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()

# The sideloader must never open an MTD device.  The stock 2.6.16 stage reads
# NAND to boot; the new OS has no MTD driver at all.  This static check catches
# a regression in the executable before QEMU is started.
# The sideload label is allowed; these are the MTD device paths and write
# tools the sideloader must never contain.
MTD_STRINGS = (b"/dev/mtd", b"mtdblock", b"flash_erase", b"flashcp", b"nandwrite")

def assert_no_mtd_strings(path):
    with open(path, "rb") as f:
        data = f.read()
    found = [s.decode() for s in MTD_STRINGS if s in data]
    if found:
        sys.exit(f"{path}: contains MTD device paths or write tools: {', '.join(found)}")
    print(f"{path}: no MTD device paths or write tools", flush=True)

def main():
    qemu = os.environ.get("QEMU")
    stock = env("STOCK_ZIMAGE")
    nand_src = env("NAND_SRC")
    sideloader = env("SIDELOADER", os.path.join(HERE, "neochumby-kernel-sideloader"))
    payload = env("PAYLOAD", os.path.join(HERE, "build", "sideload-payload.bin"))
    work = env("WORK", os.path.join(HERE, "build", "demo"))
    os.makedirs(work, exist_ok=True)

    nand_before = sha256(nand_src)
    assert_no_mtd_strings(sideloader)

    # A clean PSP tree: only the sideloader and the rcS hook.  This writes a
    # separate scratch NAND image (host-side image preparation); the
    # original is opened read-only and hashed before/after.
    psp_dir = os.path.join(work, "psp")
    os.makedirs(os.path.join(psp_dir, "rfs1"), exist_ok=True)
    rcS = (
        "#!/bin/sh\n"
        "echo \"sideloader rcS: start\"\n"
        "ifconfig lo 127.0.0.1 up\n"
        f"{os.path.join('/psp', 'neochumby-kernel-sideloader')} --source 0xc2000000\n"
        "echo \"sideloader rcS: app returned $?\"\n"
        "exec /bin/sh\n"
    )
    for token in ("mtd", "nand", "flash_erase", "flashcp", "nandwrite"):
        if token in rcS.lower():
            sys.exit(f"generated rcS contains forbidden token: {token}")
    with open(os.path.join(psp_dir, "rfs1", "rcS"), "w") as f:
        f.write(rcS)
    os.chmod(os.path.join(psp_dir, "rfs1", "rcS"), 0o755)
    with open(sideloader, "rb") as src, open(
        os.path.join(psp_dir, "neochumby-kernel-sideloader"), "wb"
    ) as dst:
        dst.write(src.read())
    os.chmod(os.path.join(psp_dir, "neochumby-kernel-sideloader"), 0o755)
    psp_jffs2 = os.path.join(work, "psp.jffs2")
    with open(psp_jffs2, "wb") as f:
        f.write(cm.jffs2(psp_dir, cm.PARTITIONS["PSP"][1]))
    scratch = os.path.join(work, "nand-scratch.img")
    subprocess.run(
        [sys.executable, os.path.join(HERE, "mk_psp_nand.py"), nand_src, psp_jffs2, scratch],
        check=True,
    )
    scratch_before = sha256(scratch)

    log_path = env("LOG", os.path.join(work, "sideloader-demo.log"))
    m = cm.Machine(tempfile.mkdtemp(prefix="nc-demo-"))
    ok, text = False, ""
    try:
        m.start(kernel=stock, nand=scratch, fast=True, qemu=qemu,
                append="console=ttyS0,38400 root=/dev/mtdblock5 rootfstype=cramfs "
                       "init=/linuxrc chumby_hwversion=3.8",
                extra=["-device", f"loader,file={os.path.abspath(payload)},addr=0xc2000000,force-raw=on",
                       "-no-reboot"])
        try:
            m.wait_log("~ # ", 120)
            ok = True
        except cm.Error:
            pass
        print("sideload: got sideload kernel shell" if ok else "sideload: NO SIDELOAD KERNEL SHELL", file=sys.stderr)
        if ok:
            for cmd in [
                "uname -a",
                "cat /proc/version",
                "cat /proc/cmdline",
                "cat /proc/mtd; echo MTD_RC=$?",
                "ls -l /dev/mtd*; echo MTD_DEV_RC=$?",
                "dmesg | grep -iE 'nand:|mxc_nand|nand device|nand flash' | grep -v 'stays unloaded'; echo NAND_GREP_RC=$?",
                "ls -l /sys/class/mtd; echo SYS_MTD_RC=$?",
                "grep -q 'neochumby.sideloaded=1' /proc/cmdline; echo SIDELOADED_RC=$?",
                "grep -E '^(mtd|mtdblock|nandcore|mxc_nand) ' /proc/modules; echo NAND_MODULE_RC=$?",
                "echo SIDELOADER_SHELL_OK",
            ]:
                _, out = m.exec(cmd, 30)
                text += out
            guest_checks = [
                ("MTD_RC=1", "no /proc/mtd"),
                ("MTD_DEV_RC=1", "no /dev/mtd*"),
                ("NAND_GREP_RC=1", "no NAND in dmesg"),
                ("SYS_MTD_RC=1", "no /sys/class/mtd"),
                ("SIDELOADED_RC=0", "sideload marker in /proc/cmdline"),
                ("NAND_MODULE_RC=1", "NAND modules not loaded"),
                ("SIDELOADER_SHELL_OK", "shell marker"),
            ]
            for marker, what in guest_checks:
                if marker not in text:
                    print(f"ERROR: sideload check failed: {what}", file=sys.stderr)
                    ok = False
            if not re.search(r"^Linux \(none\) [0-9]+\.[0-9]+\.[0-9]+(-rc[0-9]+)?-mx21-sideload "
                             r"#\d+ .* armv5tejl GNU/Linux$", text, re.M):
                print("ERROR: sideload check failed: uname -a is not the sideload kernel", file=sys.stderr)
                ok = False
    finally:
        m.stop()
        with open(log_path, "w") as f:
            f.write(m.log() if os.path.exists(m.at(cm.LOG)) else "")
        shutil.rmtree(m.path, ignore_errors=True)
    sys.stdout.write(open(log_path).read())

    nand_after = sha256(nand_src)
    scratch_after = sha256(scratch)
    print("\nsideloader contains no MTD device paths or write tools")
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
