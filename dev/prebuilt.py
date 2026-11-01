"""What dev/emulate boots, made ready: QEMU with the machine
(emu/qemu/out/obj/qemu-system-arm) and the dist image (build/sideload-dist),
each fetched prebuilt and verified (emu/qemu/fetch, dist/fetch), or built from
source when it can't be (emu/qemu/build, dist/build).

Beside each output it records, in .neochumby-inputs, what it was made from
and how (fetched or built), and the size and time of each file it made.
The output is current while that record names the inputs as they are now:
for QEMU, the qemu-chumby commit this repository pins (or the submodule's
own checkout, if that differs or has local changes); for an image, its
inputs commit (dist/fetch --inputs) and kernel, and a digest of any local
changes to its inputs (dist/fetch --pathspecs). A current output is used as
it is, with no network; a missing or stale one is fetched, else built.

An output without that record, or whose files have changed since, was made
some other way (dist/build by hand, CI's download): it is used as it is,
since what it was made from isn't known, and never overwritten unless
building is asked for.

The order, for an output that is missing or stale:
  1. fetch, unless asked not to (--no-fetch, --build), or its inputs have
     local changes (no prebuilt matches them);
  2. if fetch refuses or fails, one line saying why, then the build;
  3. if the build can't run here (it exits 2: no container runtime, or
     QEMU's build dependencies missing), Unavailable: dev/emulate skips,
     naming both;
  4. if the build fails, Failed.
Only what fetch verified or the build made is ever recorded or used.
"""

import contextlib
import fcntl
import hashlib
import os
import re
import subprocess
import sys

MARK = ".neochumby-inputs"
DIST_KERNELS = ("stable", "mainline")
DEFAULT_DIST_KERNEL = "stable"

# How each fetch says why it refused, as one short reason. The first match
# wins: the specific before the general.
REASONS = [
    (r"local changes", "its inputs have local changes"),
    (r"shallow clone", "this is a shallow clone"),
    (r"needs gh", "no gh to verify the prebuilt with"),
    (r"gh isn't logged in", "gh isn't logged in, which verifying the prebuilt needs"),
    (r"doesn't run here", "the prebuilt binary doesn't run here"),
    (r"isn't attested|SHA-256 is|MANIFEST doesn't|refused", "the prebuilt didn't verify"),
    (r"has no digest|no prebuilt", "no prebuilt for these inputs (or no network)"),
]


class Unavailable(Exception):
    """Neither fetched nor built, and not for want of trying: skipped."""


class Failed(Exception):
    """The build ran and failed."""


def say(msg):
    print(f"dev/emulate: {msg}", file=sys.stderr, flush=True)


def git(repo, *args, check=True):
    r = subprocess.run(["git", "-C", repo, *args], capture_output=True)
    if check and r.returncode != 0:
        return None
    return r.stdout


def reason(output, script, status):
    """One line: why SCRIPT (a fetch) refused, from what it said."""
    for pattern, why in REASONS:
        if re.search(pattern, output):
            return why
    for line in output.splitlines():
        if line.startswith("fetch: "):
            return line[len("fetch: "):].rstrip(":;")
    return f"{script} failed (exit {status})"


class Output:
    """One thing fetched or built: `files` (the first is what's booted or
    run) in `where`, a directory."""

    name = None
    where = None
    files = ()
    fetch = None  # argv
    build = None  # argv
    build_env = {}

    def __init__(self, repo):
        self.repo = repo

    def rel(self, path):
        return os.path.relpath(path, self.repo)

    def path(self, f=None):
        return os.path.join(self.where, f or self.files[0])

    def inputs(self):
        """(key, why not to fetch or None). key is None when the inputs
        can't be known."""
        raise NotImplementedError

    # The record.

    def stats(self):
        out = []
        for f in self.files:
            try:
                st = os.stat(self.path(f))
            except FileNotFoundError:
                continue
            out.append(f"{f} {st.st_size} {st.st_mtime_ns}")
        return out

    def recorded(self):
        """(key, how) as recorded, or None if there's no record or the files
        aren't the ones it describes."""
        try:
            with open(os.path.join(self.where, MARK)) as f:
                lines = f.read().splitlines()
        except (FileNotFoundError, NotADirectoryError):
            return None
        fields = dict(line.split(": ", 1) for line in lines if ": " in line and not line.startswith("#"))
        files = [line[len("file "):] for line in lines if line.startswith("file ")]
        if fields.get("what") != self.name or files != self.stats():
            return None
        return fields.get("inputs"), fields.get("from")

    def record(self, key, how):
        lines = ["# What dev/emulate made here, from which inputs (dev/prebuilt.py): it",
                 "# fetches or builds again when they change.",
                 f"what: {self.name}", f"inputs: {key}", f"from: {how}"]
        lines += [f"file {s}" for s in self.stats()]
        mark = os.path.join(self.where, MARK)
        with open(mark + ".new", "w") as f:
            f.write("\n".join(lines) + "\n")
        os.rename(mark + ".new", mark)

    @contextlib.contextmanager
    def locked(self):
        """One dev/emulate at a time fetching or building this."""
        lock = self.where.rstrip("/") + ".lock"
        os.makedirs(os.path.dirname(lock), exist_ok=True)
        with open(lock, "w") as f:
            try:
                fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                say(f"waiting for another dev/emulate making {self.name} ({self.rel(self.where)})")
                fcntl.flock(f, fcntl.LOCK_EX)
            yield

    # The decision.

    def ensure(self, mode="auto", act=True):
        """The path of the file to boot or run, current for the inputs, after
        fetching or building it if need be. MODE: auto (fetch, else build),
        no-fetch (build) or build (never a fetched one). With act False,
        neither: what's there, if it can be used."""
        with self.locked():
            return self._ensure(mode, act)

    def _ensure(self, mode, act):
        key, why_not = self.inputs()
        have = os.path.exists(self.path())
        rec = self.recorded() if have else None
        if have and rec is None and mode != "build":
            say(f"using {self.rel(self.where)} as it is: not made by dev/emulate, so what it was "
                f"made from isn't known (--build to build it from source)")
            return self.path()
        if rec and key is not None and rec[0] == key and not (mode == "build" and rec[1] != "build"):
            return self.path()
        if have and key is None and mode != "build":
            say(f"using {self.rel(self.where)} as it is: its inputs can't be known ({why_not})")
            return self.path()
        state = "stale" if have else "missing"
        if not act:
            raise Unavailable(f"{self.rel(self.where)} is {state} (made from other inputs)" if have
                              else f"no {self.rel(self.path())}")

        if mode == "build":
            why = "--build"
        elif mode == "no-fetch":
            why = "--no-fetch"
        elif why_not:
            why = why_not
        else:
            say(f"{self.name}: {'out of date' if have else 'missing'}: fetching it ({self.rel(self.fetch[0])})")
            r = subprocess.run(self.fetch, capture_output=True, text=True, cwd=self.repo)
            if r.returncode == 0 and os.path.exists(self.path()):
                sys.stderr.write(r.stdout + r.stderr)
                self.record(key, "fetch")
                return self.path()
            why = reason(r.stdout + r.stderr, self.rel(self.fetch[0]), r.returncode)

        build = " ".join([f"{k}={v}" for k, v in self.build_env.items()] + [self.rel(self.build[0])]
                         + [self.rel(a) for a in self.build[1:]])
        say(f"{self.name}: {why}; building from source ({build})")
        r = subprocess.run(self.build, cwd=self.repo, env=dict(os.environ, **self.build_env),
                           stdout=sys.stderr)
        if r.returncode == 2:
            there = f"; {self.rel(self.where)} is there, but made from other inputs" if have else ""
            raise Unavailable(f"{self.name}: not fetched ({why}), and it can't be built here "
                              f"({build}: see above){there}")
        if r.returncode != 0 or not os.path.exists(self.path()):
            raise Failed(f"{self.name}: {build} failed (exit {r.returncode}); not fetched: {why}")
        if key is not None:
            self.record(key, "build")
        return self.path()


class Qemu(Output):
    files = ("qemu-system-arm",)

    def __init__(self, repo):
        super().__init__(repo)
        self.name = "QEMU"
        here = os.path.join(repo, "emu", "qemu")
        self.where = os.path.join(here, "out", "obj")
        self.fetch = [os.path.join(here, "fetch")]
        self.build = [os.path.join(here, "build")]

    def inputs(self):
        pin = git(self.repo, "rev-parse", "HEAD:emu/qemu/qemu-chumby")
        if not pin:
            return None, "no qemu-chumby commit pinned here"
        pin = pin.decode().strip()
        sub = os.path.join(self.repo, "emu", "qemu", "qemu-chumby")
        if not os.path.exists(os.path.join(sub, ".git")):
            return pin, None  # emu/qemu/build checks out the pinned commit
        head = (git(sub, "rev-parse", "HEAD") or b"").decode().strip()
        diff = git(sub, "diff", "HEAD", "--binary") or b""
        if head == pin and not diff:
            return pin, None
        key = head + (f"+{hashlib.sha256(diff).hexdigest()[:16]}" if diff else "")
        if head != pin:
            return key, f"emu/qemu/qemu-chumby is checked out at {head[:12]}, not the pinned {pin[:12]}"
        return key, "emu/qemu/qemu-chumby has local changes"


class Image(Output):
    """dist's, for a kernel, from the dist-prebuilt release."""

    def __init__(self, repo, kernel=None):
        super().__init__(repo)
        fetch = os.path.join(repo, "dist", "fetch")
        kernel = kernel or DEFAULT_DIST_KERNEL
        if kernel not in DIST_KERNELS:
            sys.exit(f"dev/emulate: NEOCHUMBY_DIST_KERNEL is {' or '.join(DIST_KERNELS)}, not {kernel}")
        self.name = f"the dist image ({kernel})"
        self.where = dist_dir(repo, kernel)
        self.files = ("sideload-zImage", "sideload-rootfs.cpio.gz")
        self.args = ["--kernel", kernel]
        self.fetch = [fetch, *self.args, self.where]
        self.build = [os.path.join(repo, "dist", "build"), self.where]
        if kernel != DEFAULT_DIST_KERNEL:
            self.build_env = {"NEOCHUMBY_DIST_KERNEL": kernel}
        self.dist_fetch = fetch

    def inputs(self):
        r = subprocess.run([self.dist_fetch, "--inputs", *self.args], capture_output=True, text=True)
        if r.returncode != 0:
            return None, reason(r.stderr, "dist/fetch --inputs", r.returncode)
        commit = r.stdout.strip()
        specs = subprocess.run([self.dist_fetch, "--pathspecs", *self.args], capture_output=True,
                               text=True).stdout.split("\n")
        specs = [s for s in specs if s]
        status = git(self.repo, "status", "--porcelain", "--untracked-files=all",
                     "--ignore-submodules=dirty", "--", *specs)
        if not status:
            return commit, None
        # The digest of the local changes: the diff, and each new file.
        h = hashlib.sha256(git(self.repo, "diff", "HEAD", "--binary", "--ignore-submodules=dirty",
                               "--", *specs) or b"")
        new = git(self.repo, "ls-files", "-z", "--others", "--exclude-standard", "--", *specs) or b""
        for name in sorted(n for n in new.split(b"\0") if n):
            h.update(b"\0" + name + b"\0")
            path = os.path.join(self.repo, os.fsdecode(name))
            if os.path.islink(path):
                h.update(os.fsencode(os.readlink(path)))
            else:
                with open(path, "rb") as f:
                    h.update(f.read())
        return f"{commit}+{h.hexdigest()[:16]}", "its inputs have local changes"


def dist_dir(repo, kernel):
    """Where dist's image for KERNEL goes: build/sideload-dist for the
    default, build/sideload-dist-KERNEL for another (as CI lays them out)."""
    name = "sideload-dist" if kernel == DEFAULT_DIST_KERNEL else f"sideload-dist-{kernel}"
    return os.path.join(repo, "build", name)
