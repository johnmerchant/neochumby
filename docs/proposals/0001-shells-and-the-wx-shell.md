# Proposal 0001: shells, and bringing in Matt's stock-style shell

Status: **draft, for discussion.** From Matt Croall (`phoozle`), written by
Matt's Claude agent after reading this repo and Matt's own Chumby project.
Nothing here is decided: each question below is John's call.

## Why

Matt and John each replaced the Chumby Classic's Flash UI, separately and
differently:

- **neochumby** (this repo): a Rust session and apps, booted from a stick or
  over NFS from a paired neochumbyd, with apps as separate programs that own
  the screen while they run (Doom, Quake, AirPlay).
- **Matt's project** ("wx" below): a replica of the *stock* Chumby
  experience. A rotating channel of widgets (weather, news, stocks, photos, a
  screensaver), the 2008 control panel (Home, Settings, Music), and a local
  copy of chumby.com's "my channels" web page to set it all up from a phone.

The aim is one project, where a Chumby's owner picks a **shell** in the web
page (neochumby's home screen, Matt's stock-style one, a future one) and
each shell decides how its apps are loaded and drawn. Some apps run under
any shell (Doom); some belong to one (a weather widget is a page in the
stock-style channel, not a program).

neochumby already has most of the bones for this, so the proposal is to
build on neochumby and bring wx in as a shell, not the other way round.

## What each side has

| | neochumby | wx (Matt) |
| --- | --- | --- |
| Boot | `userhook2` from stick or `/psp`, before Flash starts; nothing flashed; CHUMBY MODE hands back | `debugchumby` on a stick, run just before Flash; stops Flash and guards against its watchdog |
| Where apps come from | **Pull:** NFS tree from a paired neochumbyd, or a stick | **Push:** the server writes a/b sets onto the Chumby's USB stick (over SSH today) |
| Security | Paired TLS 1.3 / Noise with pinned keys; never starts SSH | Relies on SSH being on (the stock root login) |
| Rendering | On device, each app draws its own pixels (Rust, `fb`, font8x8) | On a server: Python + Pillow renders every pixel as RGB565; the device only blits |
| On-device engine | One program per app | `wxshow`, ~2,400 lines of C, ~1% CPU on static pages, 25 fps animations, dirty rectangles, transitions, ticker, clock, glow/video layers |
| Memory | Binaries paged in over NFS | Lazy loading with an LRU cache under a byte budget (4 MB), a loader child so slow reads never stall a frame |
| Updates | Rebuild the tree | Delta push of changed files only (md5), atomic a/b switch, rollback |
| Configuration | `/psp/neochumby.conf`, `neochumbyd` CLI | A web page (chumby.com "my channels" style): line-up, per-app customise forms built from each app's `CONFIG_SCHEMA`, photo uploads; reached from a QR code on the Chumby |
| Shell choice | **`home=` already exists**, plus the 3-second escape to `neochumby-home` | None (one shell) |
| Apps | Doom, Quake (fast!), AirPlay; companions on the server | Weather, aurora screensaver, ABC news, stocks, photos; Home / Settings / Music screens; SomaFM radio |
| Tooling | `tools/tree`, `stick`, `deploy`, `profile`, CI | SDL simulator of the same engine, headless scripted taps, `chumbyctl` (show/tap/stats/bench/screenshot/rollback) |
| Language rule | Rust for everything we write | C (engine) + Python (feeder, web page) |
| Licence | MIT per crate, GPL engines isolated | No licence yet (Matt to choose; see below) |

**Where neochumby is stronger:** the boot path (earlier, cleaner, no Flash
fight), pairing and transport security, no SSH, the session/escape-hatch
model, real programs like Doom and Quake, the server-half "companion" idea,
CI, and a tidy licence story. wx should adopt all of these.

**Where wx is stronger:** everything a non-technical owner sees. A
polished stock-Chumby look, a channel of widgets, a phone-friendly settings
page, and an engine that is very cheap on the ARM926 because it never draws
text or gradients on the device. Plus delta updates and lazy loading, which
matter once there are many widgets, and a simulator that makes UI work fast.

## The proposal

### 1. Shells are home apps (exists today)

A shell is an app whose manifest has `home=`. The session runs the one named
in config, falls back to `neochumby-home`, and the 3-second corner hold
always escapes. wx becomes one more shell:

```
# apps/wx/manifest
title=CHUMBY
home=wxshow
neochumbyd=required        # its pictures are rendered by a server
companion=wx-feeder        # see 4
```

**Question 1:** happy for shells to be "an app with `home=`", as now, rather
than a new concept?

### 2. Picking the shell from the web page

Today `home=` lives in `/psp/neochumby.conf`, on the Chumby. The web page
runs on the server. Two ways to join them:

- **(a)** The Chumby asks its neochumbyd at `Hello` time; `Welcome` (or a new
  `Msg::Config`) carries `home=<app>`, and the session caches it in `/psp` for
  offline boots. Small change to `neochumby-proto` and `main.rs`.
- **(b)** neochumbyd writes a per-Chumby `chumbys/<name>/neochumby.conf` into
  the NFS tree, which the session reads after `/psp`.

We lean to (a): it keeps the tree identical for every Chumby and the pairing
key already identifies the Chumby. **Question 2:** (a), (b), or something else?

### 3. Which apps work in which shell

Add one optional manifest key:

```
shells=wx home              # this app is offered only in these shells; absent: any
```

and one rule for shells: **a shell can launch any `exec=` app** the way
`neochumby-home` does (give up the screen, run it with `NEOCHUMBY_ROOT`,
redraw when it exits). Then Doom and Quake work in every shell for free,
and wx's own widgets (which are not programs) are marked `shells=wx`. wx's
Home screen would get an APPS button listing the `exec=` apps.

Widgets inside the wx shell (weather, news...) are a second level: they are
pages in wx's channel, configured in the web page, not entries in
`apps/`. **Question 3:** is a `shells=` key the right place, or would you
rather keep shell-specific things entirely inside the shell's own app?

### 4. Two delivery modes: pull (NFS) and push (stick)

The two projects get files onto a Chumby in opposite ways, and Matt would
like the merged project to **support both, as a per-Chumby choice**:

- **Pull (neochumby today):** the Chumby mounts the tree from neochumbyd
  over NFS and pages things in as they're used. Nothing is stored on the
  Chumby; it needs the server at boot.
- **Push (wx today):** the server writes into the Chumby's own USB stick,
  and the Chumby runs from that local copy. wx keeps two complete sets on the
  stick (`a/`, `b/`) and a one-word `current` pointer. A push writes only the
  files whose md5 differs into the *standby* set, verifies them, then flips
  `current`, so the live set is never touched and rollback is one flip.
  The Chumby keeps working, showing the last push, when the server is down
  or unreachable.

| | Pull (NFS) | Push (stick) |
| --- | --- | --- |
| Storage on the Chumby | none | a stick (small, old, low-power: see `docs/hardware.md`) |
| Server down | nothing to boot from | runs on the last push |
| Updates | instant: the server's tree is the tree | on each push, deltas only, atomic, with rollback |
| Network use | every page-in (cached by the kernel) | only changed files, gzip'd |
| Good for | a desk of Chumbys next to the server; developing | a Chumby on flaky WiFi, or away from the server |

Proposed shape:

- A per-Chumby `delivery=pull|push` setting, chosen in the web page (or the
  CLI) and kept by neochumbyd. The boot hook already prefers a stick's tree
  over NFS, so push mode is "the stick's tree, kept up to date by the server".
- **Push goes over the paired session, not SSH.** wx pushes over SSH today;
  in the merged project a small receiver on the Chumby (part of the session,
  or a `service=`) takes the files from neochumbyd as frames, writes the
  standby set, checks the md5s and flips `current`. Same algorithm wx uses
  now, new transport, and still nothing turns on SSH.
- It covers the **whole tree**, not just wx's pictures: binaries, manifests
  and app files are pushed the same way, so Doom works in push mode too.
- wx's renderer (the feeder) becomes a companion that renders into the
  Chumby's tree on the server under `NEOCHUMBY_STATE`. In pull mode that's
  read over NFS; in push mode it is what gets pushed. `wxshow` reads from
  "the tree" and doesn't care which. Its loader child already copes with
  slow reads from either.
- `chumbyctl`'s control commands (show/tap/stats/bench/rollback) would go
  over the same session instead of SSH.

**Question 4:** happy to support both modes, with push implemented as a
receiver over the paired session? Any preference on where the receiver
lives (inside `neochumby`, or its own service)?

### 5. The web page lives with neochumbyd

wx's `configweb` is a LAN-only page (no login, CSRF and DNS-rebinding
checks, escaped output, Pillow-verified uploads). In the merged project it
would be served by neochumbyd (or a companion), and would grow a **Chumbys**
list (each paired Chumby: name, shell, line-up) and the shell picker. It
could also replace the CLI for pairing: show pending codes, Approve /
Refuse buttons.

The schema model would stay: each widget declares `APP` (name, icon,
seconds, sub-slides) and `CONFIG_SCHEMA` (typed fields: list, string,
bool, choice, number, image library), and the page builds its forms from
that. **Question 5:** is a web page in scope for neochumbyd at all? If yes,
see the language question.

### 6. Apps from GitHub

`CONTRIBUTING.md` says an app can live in its own repository and be added
from its git URL. We'd like to make that real and use it for wx's widgets
too, starting with the aurora screensaver as its own public repo:

- `tools/tree OUT ... https://github.com/<owner>/neochumby-<name>` clones
  (pinned to a commit) and builds like an in-repo app; and later the web page
  can add an app by URL.
- A wx widget repo holds its manifest (`shells=wx`), its renderer module and
  its assets. wx's companion loads widgets from the tree the same way.

**Question 6:** did you already have a shape in mind for git-URL apps
(pinning, crates.io vs git, how the server fetches)? We'd follow yours.

## The hard parts: philosophy mismatches

These are the places where "just merge it" doesn't work. Each needs a
decision from John.

1. **"Rust for everything we write."** wx is C on the device and Python on
   the server. Options:
   - **(A)** Port `wxshow` to Rust as a crate using `neochumby::fb` and
     `input` (it is a blitter with a manifest parser and a state machine:
     very portable; the loader child becomes a thread or a child process),
     and keep the renderer in Python as a companion, an exception for
     server-side widget renderers only.
   - **(B)** Port both. Pillow's text and anti-aliasing would need a Rust
     replacement (`ab_glyph` / `tiny-skia` / `image`), and every widget
     would be rewritten. Large, and it slows widget-writing a lot.
   - **(C)** Vendor wx as it is, C and Python, like Doom and Quake are
     vendored, in its own directory with its own rules.

   Our suggestion is **(A)**: the device side follows neochumby's rules,
   and widgets stay quick to write in Python on the server, where the Rust
   rule matters least. **Is a Python exception for server-side renderers
   acceptable?**
2. **Server-rendered vs device-rendered.** neochumby apps run with no server
   (Doom from a stick). wx's shell needs a server to render. We'd keep that
   honest with `neochumbyd=required`, and keep a seed set on the stick so it
   shows the last pictures until the server answers.
3. **Boot.** We'd drop wx's `debugchumby` + Flash-watchdog dance and use
   `userhook2`, so nothing else changes. As with wx today, the stock
   Chumby alarms can't fire while a shell runs.
4. **Hardware notes.** wx has a long list of measured traps (the level-
   triggered `/dev/switch` that spins `read()` loops, `mDNSResponder` at
   100% CPU, the clock drifting back a year, a 32 GB stick at power-on
   stopping the `rt73` WiFi, BusyBox 1.6 limits, framebuffer copy and blend
   rates). Happy to contribute them as `docs/hardware.md` whatever else is
   decided.

## Licensing

- **Confirmed by Matt: everything we contribute here is MIT**, like the
  rest of neochumby.
- Some of wx's assets **can't** come across and would stay in Matt's private
  config or be replaced: the ABC News banner and logo, the Chumby logo and
  octopus (Chumby's trademarks; we'd draw our own or fetch at runtime), a
  green-screen video clip of unconfirmed licence, and company logos fetched
  at runtime by the stocks widget (fetched, never committed).
- Fonts are fine to ship: Liberation (OFL 1.1, and GPLv2 + font exception
  for Narrow), DejaVu (Bitstream Vera + public domain), SN Pro (OFL 1.1).
  Each with its licence file.

## Suggested order of pull requests

Small and reviewable, each standing alone:

1. `docs/hardware.md`: the measured traps and numbers (no code).
2. `shells=` manifest key, and `neochumby-home` hiding apps not offered in
   it (a few lines in `apps.rs` and `apps/home`).
3. Shell from neochumbyd (Question 2), with the `/psp` cache.
4. Git-URL apps in `tools/tree` (Question 6).
5. `apps/wx`: the engine, per the language decision, with the simulator.
6. Push mode: the on-Chumby receiver (a/b sets, md5 delta, atomic flip,
   rollback) over the paired session, and `delivery=` per Chumby.
7. wx's companion: the renderer, writing into the Chumby's tree.
8. The web page in neochumbyd, with pairing and the shell picker.
9. Widgets as their own repos, aurora first.

Steps 1 and 2 don't depend on any of the open questions; we can start there
whenever you're happy.
