"""neochumbyd_admin: the web page's one way into neochumbyd's admin functions.

The Chumbys page lists the paired Chumbys and the ones asking to pair, approves or
refuses a request, forgets a Chumby, and sets a Chumby's shell (home=) and delivery
(delivery=nfs|sync). Only this module talks to neochumbyd, so swapping how it does is
a change here and nowhere else.

How it talks to neochumbyd:
  socket   NEOCHUMBY_ADMIN, the admin socket neochumbyd gives its daemon= app (neochumbyd
           #9): a local Unix socket that only neochumbyd's own user may use. Requests are
           one line of words; the answer is data lines, then "ok", "ok WORD" or
           "error MESSAGE".
  cli      without it (the page started by hand, or a neochumbyd without #9): the
           neochumbyd command (NEOCHUMBYD, default `neochumbyd` on PATH) with --state
           NEOCHUMBYD_STATE: `pairing`, `approve CODE`, `refuse CODE`, `forget NAME` and
           `set NAME KEY=VALUE` (neochumbyd #5), for a page run by a user in neochumbyd's
           group.

Signing in: approving a pairing lets a device onto the server, so the Chumbys page needs a
login.
  socket   the server owner's token, which the human types in (`neochumbyd token` prints
           it on the server; the page never reads it from disk). neochumbyd checks it
           (`check TOKEN`) and requires it again on `approve`, so the page keeps it in
           memory for the session, and nowhere else.
  cli      a one-time token in a file only the server's admin can read
           (NEOCHUMBY_ADMIN_TOKEN_FILE, default admin-token in the app's state directory,
           mode 0600): a login with it starts a session, and the file gets a new token.

Shells are the apps in the tree whose manifest has home= (NEOCHUMBY_TREE, which neochumbyd
sets, default <NEOCHUMBYD_STATE>/tree).
"""
import hmac
import os
import re
import secrets
import socket
import subprocess
import threading
from collections import namedtuple

NAME = re.compile(r"[a-z0-9_-]{1,32}")              # neochumby_proto::valid_name
CODE = re.compile(r"[0-9]{6}")
OWNER_TOKEN = re.compile(r"[0-9a-f]{32}")              # neochumbyd's owner-token
DELIVERY = ("nfs", "sync")
# The Chumby setting that names its shell, and the manifest key that makes an app one.
# Both are "home" today; John's proposal 0002 (#8) may rename them (start= or shell=),
# and then only these two lines change.
SHELL_SETTING = "home"
SHELL_MANIFEST_KEY = "home"

Chumby = namedtuple("Chumby", "name key settings")   # settings: {key: value}, e.g. home, delivery
Pending = namedtuple("Pending", "name code addr key")


class AdminError(Exception):
    """What went wrong, in words fit for the page."""


def state_dir():
    """The app's own state directory (neochumbyd's NEOCHUMBY_STATE)."""
    return os.environ.get("NEOCHUMBY_STATE") or "/var/lib/neochumbyd/apps/wx-feed"


def neochumbyd_state():
    """neochumbyd's state directory: NEOCHUMBYD_STATE, else two up from the app's own
    (<state>/apps/<app>)."""
    if os.environ.get("NEOCHUMBYD_STATE"):
        return os.environ["NEOCHUMBYD_STATE"]
    return os.path.dirname(os.path.dirname(os.path.abspath(state_dir())))


# ---------------------------------------------------------------- the admin token
def token_path():
    return os.environ.get("NEOCHUMBY_ADMIN_TOKEN_FILE") or os.path.join(state_dir(), "admin-token")


def new_token():
    """Write a fresh token (0600, replacing any old one) and return where it is."""
    path = token_path()
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    tmp = f"{path}.{os.getpid()}.tmp"
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as fh:
        fh.write(secrets.token_urlsafe(18) + "\n")
    os.replace(tmp, path)
    return path


def ensure_token():
    """Make the token file if there isn't one. Returns its path."""
    path = token_path()
    if not os.path.isfile(path):
        new_token()
    return path


def check_token(given):
    """True if `given` is the current token, which is then used up (a new one is written)."""
    try:
        with open(token_path()) as fh:
            want = fh.read().strip()
    except OSError:
        return False
    if not want or not isinstance(given, str) or not hmac.compare_digest(given.strip().encode(), want.encode()):
        return False
    new_token()
    return True


# ---------------------------------------------------------------- shells, from the tree
def tree_dir():
    return os.environ.get("NEOCHUMBY_TREE") or os.path.join(neochumbyd_state(), "tree")


def manifest(app):
    try:
        with open(os.path.join(tree_dir(), "apps", app, "manifest"), encoding="utf-8", errors="replace") as fh:
            return dict(ln.strip().split("=", 1) for ln in fh if "=" in ln)
    except OSError:
        return {}


def shells():
    """[(app, title)] for every app in the tree that can be a home screen."""
    try:
        apps = sorted(os.listdir(os.path.join(tree_dir(), "apps")))
    except OSError:
        return []
    out = []
    for app in apps:
        if NAME.fullmatch(app):
            m = manifest(app)
            if m.get(SHELL_MANIFEST_KEY):
                out.append((app, m.get("title") or app))
    return out


# ---------------------------------------------------------------- backends
class CliBackend:
    """neochumbyd's own command, as an admin would run it."""

    def __init__(self):
        self.exe = os.environ.get("NEOCHUMBYD") or "neochumbyd"
        self.state = neochumbyd_state()

    def run(self, *args):
        try:
            r = subprocess.run([self.exe, *args, "--state", self.state], capture_output=True, text=True, timeout=20)
        except (OSError, subprocess.TimeoutExpired) as e:
            raise AdminError(f"couldn't run neochumbyd: {e}") from None
        if r.returncode:
            msg = (r.stderr or r.stdout).strip().splitlines()
            raise AdminError(msg[-1][:200] if msg else f"neochumbyd failed ({r.returncode})")
        return r.stdout

    def listing(self):
        """`neochumbyd pairing`: 'paired NAME KEY [k=v ...]' and 'asking NAME code CODE from ADDR KEY'."""
        chumbys, pending = [], []
        for ln in self.run("pairing").splitlines():
            t = ln.split()
            if len(t) >= 3 and t[0] == "paired" and NAME.fullmatch(t[1]):
                settings = dict(kv.split("=", 1) for kv in t[3:] if "=" in kv)
                chumbys.append(Chumby(t[1], t[2], settings))
            elif len(t) >= 7 and t[0] == "asking" and t[2] == "code" and CODE.fullmatch(t[3]) and t[4] == "from":
                pending.append(Pending(t[1], t[3], t[5], t[6]))
        return chumbys, pending

    def approve(self, code, token=None):
        return self.run("approve", code).strip()

    def refuse(self, code):
        self.run("refuse", code)

    def forget(self, name):
        self.run("forget", name)

    def set(self, name, key, value):
        self.run("set", name, f"{key}={value}")


class SocketBackend:
    """neochumbyd's admin socket (NEOCHUMBY_ADMIN): one connection, kept open and used by one
    request at a time, made again if it breaks. Every word sent has been checked against
    its pattern first, so no request can carry a space or a newline of its own."""

    def __init__(self, path):
        self.path = path
        self.lock = threading.Lock()
        self.f = None

    def ask(self, *words):
        line = " ".join(words)
        if any(not w or any(c.isspace() for c in w) for w in words) or len(line) > 1000:
            raise AdminError("bad request")
        with self.lock:
            for attempt in (0, 1):
                try:
                    if self.f is None:
                        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                        s.settimeout(20)
                        s.connect(self.path)
                        self.f = s.makefile("rw", encoding="utf-8", newline="\n")
                    self.f.write(line + "\n")
                    self.f.flush()
                    out = []
                    while True:
                        ln = self.f.readline()
                        if not ln:
                            raise OSError("neochumbyd closed the admin socket")
                        ln = ln.rstrip("\n")
                        if ln == "ok" or ln.startswith("ok "):
                            return out, ln[3:]
                        if ln.startswith("error"):
                            raise AdminError(ln[6:][:200] or "neochumbyd refused")
                        out.append(ln)
                except OSError as e:
                    self.f = None
                    if attempt:
                        raise AdminError(f"can't reach neochumbyd's admin socket: {e}") from None

    def listing(self):
        chumbys, pending = [], []
        for ln in self.ask("paired")[0]:
            t = ln.split()
            if len(t) >= 3 and t[0] == "paired" and NAME.fullmatch(t[1]):
                chumbys.append(Chumby(t[1], t[2], dict(kv.split("=", 1) for kv in t[3:] if "=" in kv)))
        for ln in self.ask("pending")[0]:
            t = ln.split()                                  # pending CODE NAME ADDRESS KEYHEX
            if len(t) >= 5 and t[0] == "pending" and CODE.fullmatch(t[1]):
                pending.append(Pending(t[2], t[1], t[3], t[4]))
        return chumbys, pending

    def check(self, token):
        try:
            self.ask("check", token)
            return True
        except AdminError:
            return False

    def approve(self, code, token):
        return self.ask("approve", code, token)[1]

    def refuse(self, code):
        self.ask("refuse", code)

    def forget(self, name):
        self.ask("forget", name)

    def set(self, name, key, value):
        self.ask("set", name, f"{key}={value}")


_socket = None


def backend():
    """The admin socket if neochumbyd gave us one, else its command."""
    global _socket
    path = os.environ.get("NEOCHUMBY_ADMIN")
    if not path:
        return CliBackend()
    if _socket is None or _socket.path != path:
        _socket = SocketBackend(path)
    return _socket


def uses_socket():
    return bool(os.environ.get("NEOCHUMBY_ADMIN"))


# ---------------------------------------------------------------- what the page calls
def listing():
    """(paired Chumbys, requests to pair)."""
    return backend().listing()


def login(token):
    """True if `token` signs in: the owner token, as neochumbyd checks it (socket), or the
    one-time token, then spent (cli)."""
    token = (token or "").strip()
    if uses_socket():
        return bool(OWNER_TOKEN.fullmatch(token)) and backend().check(token)
    return check_token(token)


def login_help():
    """What the login page tells the human about the token."""
    if uses_socket():
        return "the server owner's token: on the server, `neochumbyd token` prints it", False
    return f"the one-time admin token in {token_path()} on the server, readable only by its admin", True


def approve(code, token=None):
    """Pairs the Chumby asking with `code`; `token` is the session's owner token (socket)."""
    if not CODE.fullmatch(code or ""):
        raise AdminError("a pairing code is six digits")
    if uses_socket() and not OWNER_TOKEN.fullmatch(token or ""):
        raise AdminError("sign in again")
    return backend().approve(code, token)


def refuse(code):
    if not CODE.fullmatch(code or ""):
        raise AdminError("a pairing code is six digits")
    backend().refuse(code)


def forget(name):
    if not NAME.fullmatch(name or ""):
        raise AdminError("not a Chumby's name")
    backend().forget(name)


def set_shell(name, app):
    """A Chumby's home screen; "" goes back to the default (neochumby-home)."""
    if not NAME.fullmatch(name or ""):
        raise AdminError("not a Chumby's name")
    if app and app not in [a for a, _ in shells()]:
        raise AdminError("not a shell in this server's tree")
    backend().set(name, SHELL_SETTING, app)


def set_delivery(name, how):
    if not NAME.fullmatch(name or ""):
        raise AdminError("not a Chumby's name")
    if how and how not in DELIVERY:
        raise AdminError("delivery is nfs or sync")
    backend().set(name, "delivery", how)
