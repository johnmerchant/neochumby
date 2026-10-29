#!/usr/bin/env python3
"""Pack a userspace directory into a gzipped newc cpio initramfs.

Device nodes are encoded in the archive rather than created on the host,
so this works without root and on macOS.  /dev/console and /dev/null are
added automatically: the kernel needs them to open the initial console.
"""
import gzip
import os
import stat
import sys

def hdr(name, mode, ino, data=b"", rdev=0, nlink=1):
    namez = name.encode() + b"\0"
    fields = [
        ino, mode, 0, 0, nlink, 0, len(data),
        os.major(rdev), os.minor(rdev), os.major(rdev), os.minor(rdev),
        len(namez), 0,
    ]
    h = b"070701" + b"".join(b"%08X" % (f & 0xFFFFFFFF) for f in fields)
    out = h + namez
    out += b"\0" * ((4 - len(out) % 4) % 4)
    out += data
    out += b"\0" * ((4 - len(data) % 4) % 4)
    return out

def walk(root):
    entries = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames.sort()
        filenames.sort()
        rel = os.path.relpath(dirpath, root)
        if rel != ".":
            entries.append((rel, "dir", None))
        for f in filenames:
            p = os.path.join(dirpath, f)
            r = os.path.relpath(p, root)
            if os.path.islink(p):
                entries.append((r, "link", p))
            else:
                entries.append((r, "file", p))
    return entries

def main():
    if len(sys.argv) != 3:
        sys.exit("usage: mkinitramfs.py DIR OUT.cpio.gz")
    root, out = sys.argv[1], sys.argv[2]
    entries = walk(root)
    body = bytearray()
    ino = 1
    for name, kind, path in entries:
        if kind == "dir":
            st = os.stat(os.path.join(root, name))
            mode = stat.S_IFDIR | (stat.S_IMODE(st.st_mode) or 0o755)
            body += hdr(name, mode, ino)
        elif kind == "link":
            target = os.readlink(path).encode()
            body += hdr(name, stat.S_IFLNK | 0o777, ino, target)
        else:
            st = os.stat(path)
            mode = stat.S_IFREG | (stat.S_IMODE(st.st_mode) or 0o644)
            with open(path, "rb") as f:
                body += hdr(name, mode, ino, f.read())
        ino += 1
    # The kernel must be able to open an initial console before /init runs.
    # These are usually already in the directory, but add them if missing.
    for name, mode, rdev in (
        ("dev/console", stat.S_IFCHR | 0o600, os.makedev(5, 1)),
        ("dev/null", stat.S_IFCHR | 0o666, os.makedev(1, 3)),
    ):
        if not any(e[0] == name for e in entries):
            body += hdr(name, mode, ino, rdev=rdev)
            ino += 1
    body += hdr("TRAILER!!!", 0, ino)
    with gzip.GzipFile(out, "wb", mtime=0) as g:
        g.write(body)
    print(f"{out}: {len(body)} bytes cpio, {os.path.getsize(out)} bytes gzip")

if __name__ == "__main__":
    main()
