# Contributing to neochumby

Thanks for helping bring the original Chumby back. A few things make
changes easy to take.

## Ground rules

- **Rust** for everything we write. Third-party C (Doom's doomgeneric,
  Quake's engine, fetch's fastfetch, htop, procps, ncurses) is a git
  submodule, pinned to an
  upstream commit and left unchanged, and linked from a Rust crate. Where
  an app needs it to do otherwise, its `build.rs` builds that file with a
  function renamed, so that the app's own takes its place, and says why
  (`apps/quake/build.rs`). A change that can't be made that way goes in a
  fork, as a commit marked with a comment naming the app
  (`// neochumby-quake: ...`), and the submodule is pinned to that commit:
  no patch files applied at build time. `tools/tree` (and so `tools/stick`
  and `tools/deploy`) checks out the submodules of the apps it builds, so a
  clone without `--recurse-submodules` works there; `cargo` alone needs
  `git submodule update --init` first.
- **Patch files** (the kernels' and BusyBox's, under `dist/patches/`,
  `dist/kernel/` and `classic/patches/`) are plain patches, applied with
  `patch -p1`, with a DEP-3-style header before the diff: `Description:`
  (the summary, then the explanation indented one space, `.` for an empty
  line), `Author:`, `Upstream-Status:` (`Inappropriate [AI generated
  code]` for ours, `Backport [commit]` with the original author for
  upstream's), `Signed-off-by:`, `Claude-Session:`, then `---`; not
  `git format-patch` output.
- **Comments say what the code does and why the rule is what it is**, not
  how it was debugged. One paragraph is usually enough.
- **Licences stay per crate.** Our crates are MIT. A crate that links GPL
  code says so in its `license` field and ships the upstream licence. Never
  commit game data, firmware images or anything from a Chumby's settings.
- **None of the Chumby's own software, anywhere.** No firmware, images or
  stock artwork in the repository, releases, CI or attachments, and the
  emulator never impersonates a real device: see
  [docs/legal.md](docs/legal.md).
- **Nothing starts remote access by itself.** SSH on a Chumby is turned on
  by its owner, from its settings.

## Unsafe code

The workspace denies `unsafe` code. Every raw system call lives in
`crates/neochumby-sys`, behind safe functions, each `unsafe` block with a
`// SAFETY:` comment saying why it holds; anything else needing one adds a
function there, and uses std instead wherever std does the job safely. The
games (Doom, Quake), fetch, htop and top are allowed it as FFI to their C. We use libc
rather than rustix: rustix calls the kernel directly and needs Linux 3.2,
the Chumby runs 2.6.16, and its `ioctl` and `mmap` are `unsafe` too (see
neochumby-sys's documentation).

## Building and checking

From a clean clone, on macOS or Linux, nothing is built but our own code:
QEMU (qemu-chumby's) and the Chumby's Linux images (dist and classic) are
CI's, fetched and verified.

```sh
nix develop             # the tools (flake.nix); or install what dev/setup lists
dev/setup               # checks the tools, checks out the submodules, fetches QEMU and the images
cargo chumby            # the session and neochumby-home, for the Chumby
cargo neochumbyd        # the server
tools/tree /tmp/tree doom quake airplay fetch htop top logs   # a whole tree, with apps
dev/emulate start /tmp/tree   # the tree on an emulated Chumby (Testing without a Chumby, below)
# the end-to-end scenarios as CI runs them: boot on dist; boot and smoke on classic
cargo build --release -p neochumbyd
E2E_SCENARIOS=boot dev/e2e /tmp/tree target/release/neochumbyd /tmp/e2e
NEOCHUMBY_EMU_PROFILE=classic E2E_SCENARIOS='boot smoke' dev/e2e /tmp/tree target/release/neochumbyd /tmp/e2e-classic
```

Without Nix, the tools are: Rust with the
`armv5te-unknown-linux-musleabi` and `x86_64-unknown-linux-musl` targets
(`rustup target add ...`), zig 0.17 on PATH (the C compiler, linker and
musl: https://ziglang.org), CMake 3.21 or later (for fetch), Python 3.9 or
later, and gh, logged in (`gh auth login`: dist/fetch and classic/fetch
verify CI's images' attestations with it). `dev/setup --check` says which
are missing and how to get them on this system. `git submodule update
--init` checks out the third-party C (Doom, Quake, fastfetch, htop, procps,
ncurses, and dev/wasmbench's CoreMark) and qemu-chumby, but not the 2.6.16 kernel (`update = none`), which
only `classic/build` needs.

The server half (neochumbyd and companions) is built for x86_64 Linux by
default. `tools/tree --server-target aarch64-unknown-linux-musl` (64-bit
ARM Linux) or `--server-target host` (this machine, natively; a Mac, say)
builds companions for another, and `tools/deploy` picks the one its server
runs. A companion must build for all three: keep anything
platform-specific behind `cfg`, as neochumbyd does.

CI runs the same checks on every pull request (and builds the dist image
for 5.9.16 and stable; mainline weekly, or when a change is to it alone:
dist/README.md, "Mainline, weekly"). To run them yourself:
`cargo fmt --all --check`, `cargo clippy -- -D warnings` (on the host
crates and, with `--target armv5te-unknown-linux-musleabi`, on the Chumby's
crates), and `cargo test -p neochumby-proto -p neochumby-sync -p
neochumbyd`. CI also checks what the build takes in: `cargo deny check
bans licenses sources` (as `deny.toml` sets them) and `gitleaks git` over
every commit, on every run, and RustSec's advisories (`cargo deny check
advisories`) daily and whenever `Cargo.lock` or `deny.toml` changes. A new
dependency must pass `cargo deny`: a licence not yet allowed, or a source
other than crates.io, is added to `deny.toml` in the same pull request,
saying why.

neochumbyd's tests include one end to end on loopback
(`crates/neochumbyd/tests/server.rs`): a Chumby pairs, and a server
program approves it through the admin socket, sandboxed where the system
can, and checks that it can't read neochumbyd's key or the owner's token.
`neochumby-sys`'s tests check the sandbox itself; on Linux they need
Landlock, and in CI they fail without it.

One test needs root, as it changes uids: apps' programs under uids of
their own (`isolate::tests::as_root`, `--app-uids`). CI runs it with sudo.
On a Linux machine, run it as CI does, or as root in a container (rootless
podman will do), where it leaves nothing behind. Run as CI does, it leaves
a directory in `/tmp`, owned by made-up uids, to remove as root:

```sh
bin=$(cargo test -p neochumbyd --no-run --message-format=json |
  jq -r 'select(.profile.test and .target.kind == ["bin"]) | .executable')
sudo "$bin" --ignored --exact isolate::tests::as_root
```

Code that runs on the Chumby but needs no Chumby goes in a crate that also
builds on the host, so CI can test it there. `neochumby-sync` is one: its
tests run whole sync rounds over a real session on loopback, with a
temporary directory standing in for the stick. Its crash test
(`crash.rs`) cuts the power after every operation sync makes on the stick,
over a model of FAT on the Chumby's kernel, boots, and syncs again. Any
new operation on the stick goes through its `Disk` interface, so the
crash test sees it.

### On a Chumby

A paired Chumby can be worked on without SSH. Deploy a tree and restart
its session on it (`tools/deploy HOST [APP ...] --restart NAME`), read
what it and its apps wrote (`neochumbyd log NAME -f`) and capture its
screen (`neochumbyd shot NAME OUT.png`), all on the server. Everything
an app writes to stdout or stderr ends up in that log, so print what
you'd want to read there. The protocol is in `neochumby-proto`'s `dev`
module.

### With Claude Code (MCP)

`neochumbyd mcp` serves the same as an [MCP](https://modelcontextprotocol.io)
server on stdin and stdout, so Claude Code (or any MCP client) can read a
Chumby's log, restart it and look at its screen while you work on an app
together. Add it once, from your checkout, running it on the server over
SSH:

```sh
claude mcp add neochumby -- ssh -o BatchMode=yes HOST neochumbyd mcp
```

That adds it for you, in this project only (Claude Code's `local` scope),
and nothing is committed; `claude mcp list` or `/mcp` in Claude Code shows
it connected, with its tools and what each does. The SSH user needs what
the `neochumbyd` command does on that server, usually membership of its
group.

A loop that works well: deploy (`tools/deploy HOST APP --restart NAME`),
then ask Claude to read the log and look at the screen and say what's
wrong. Pairing stays with a person (`neochumbyd pairing`, `approve`), and
so does turning on remote input (`dev=on`): don't let a tool do that for
you.

To drive it as a scenario drives the emulator, turn on input for it
first: `neochumbyd set NAME dev=on` (and `dev=` to turn it off again). It
is off by default, and both neochumbyd and the Chumby refuse input
without it: pairing a Chumby lets its server see its log and its screen,
not use it, until its owner says so. Then:

- `neochumbyd tap NAME X Y [--hold MS]` touches the screen at (X, Y), in
  pixels (0-319, 0-239), for MS milliseconds (100 by default, at most
  10000), then lets go. It's written to the touchscreen's input device,
  through the Chumby's own calibration, so every reader sees it as a
  finger: the app in front, and the escape hatch (`tap NAME 10 10 --hold
  3500` ends the app in front).
- `neochumbyd button NAME press|release|tap` puts the top button down, up,
  or down and up again 300 ms later. It goes where a real press goes: a
  `button` line to the start app, or back to the start app with
  `NEOCHUMBY_REASON=button`. **Except to an app with `button = true`**:
  such an app reads `/dev/switch` itself, so a press from neochumbyd can't
  reach it, and the command says so. A scenario that needs the button in a
  game (FIRE in Doom) can't be run on a Chumby this way.

Each command returns once the Chumby has done it (a tap, once it has let
go), or fails with why not, or after 15 seconds if the Chumby isn't
connected. One at a time per Chumby. A press is let go, and a touch
lifted, if the link drops.

## Testing without a Chumby: dev/emulate

All emulation happens through qemu-chumby (the submodule
`emu/qemu/qemu-chumby`): QEMU's `chumby-classic` machine, the whole
Chumby, run and driven by its `chumby-machine` runner. `dev/emulate` is
neochumby's side of it: it puts a tree on a USB stick image, laid out as
`tools/stick` lays one out, boots one of three profiles with the stick in
the machine's USB port, and drives the result with the commands the
scenarios use. It needs Python 3.9+ and QEMU with the machine. No root,
no VM: it runs natively on Linux and macOS.

QEMU and the dist and classic images come by themselves. When `start`
(or `dev/e2e`, once, before its scenarios) needs one that's missing, or
was made from other inputs than the checkout's now, it fetches CI's
prebuilt build and verifies it: QEMU by the qemu-chumby commit pinned here
(`emu/qemu/fetch`), each image by its inputs commit and kernel
(`dist/fetch`, `classic/fetch`). If there's none, or it can't be verified
or doesn't run here (the inputs have local changes, `gh` isn't logged in,
CI hasn't published it, no network), it says why in one line and builds
from source instead (`emu/qemu/build`, which needs qemu-chumby's README's
build dependencies; `dist/build` and `classic/build`, which need a
container runtime). What it made, and from what, is recorded beside it, so
the next `start` uses it as it is. One you made yourself, or give
(`--kernel`, `$QEMU`, `--qemu`), is used as it is. `--build`
(`NEOCHUMBY_BUILD=1`) builds from source, never fetching; `--no-fetch`
(`NEOCHUMBY_NO_FETCH=1`) builds without trying to fetch; `dev/emulate
prepare` does just this part.

| Profile | What boots | Needs |
| --- | --- | --- |
| `dist` (default) | neochumby's Linux 5.9.16 with our own userspace: its `/init` mounts the stick and runs its hook | `build/sideload-dist`, fetched or built by itself (or `NEOCHUMBY_EMU_DIST_KERNEL`); `NEOCHUMBY_DIST_KERNEL=stable` or `mainline` for those kernel tracks (`build/sideload-dist-stable`, `-mainline`; CI builds mainline weekly, so that may be its newest build, not this checkout's) |
| `stock` | The Chumby's own Linux 2.6.16 firmware, unmodified: the stock boot mounts the stick on `/mnt/usb` and runs its hook | Your own flash backup: `NEOCHUMBY_EMU_KERNEL`, `_NAND` (from qemu-chumby's `chumby-image`) and `_PSP` (a factory-like settings directory). Local only, never in CI or an attachment (`docs/legal.md`) |
| `classic` | Linux 2.6.16 built from Chumby's GPL source, with an open-source busybox initramfs that runs the stick's hook as the stock boot does | Its image (`NEOCHUMBY_EMU_CLASSIC_KERNEL`, else `build/classic`, fetched or built by itself) |

`--profile NAME` or `NEOCHUMBY_EMU_PROFILE` picks one. Your own settings,
such as where your firmware is, can go in `~/.config/neochumby/env.local`,
shared by every checkout, or in `.env.local` at the top of a checkout
(git ignores it), which wins over it. Without a profile's images or QEMU, `start` and
the scenarios exit 77, skipped (the stock profile's images are yours and
never fetched; for the others, when neither fetching nor building works
here).

```sh
tools/tree /tmp/tree doom
dev/emulate start /tmp/tree          # the dist profile, the tree on a stick
dev/emulate log -f                   # the console, with the session's log
dev/emulate shot home.png
dev/emulate tap 160 102              # the second row of the home screen
dev/emulate tap 5 5 --hold 3500      # the corner hold
dev/emulate button press             # the top button
dev/emulate exec /mnt/usb/neochumby/neochumby sensors   # on the console (dist, classic)
dev/emulate stop
```

- **The stick** is a FAT image in the emulator's directory, made afresh
  at each `start`. Its `userhook2` is `dev/emulate-userhook2`, which puts
  the settings and `--bind` directories in place, copies the session's log
  to the console and runs the tree's own `userhook2`, unchanged; the
  session's end is that hook's `neochumby exited: N`. `dev/emulate mkstick
  IMG TREE` makes one to keep, and `start --stick IMG` boots it as it is,
  written as the Chumby writes it.
- **Settings:** `start --psp DIR` adds DIR's files to `/psp`: on stock, in
  the settings partition of a copy of your NAND image (yours is never
  written); on dist and classic, through the stick.
- **Pictures and the like:** `start --bind DIR:/tmp/classic` puts a host
  directory on the stick and links it there. `dev/emulate compare RAW ...`
  says which 320x240 RGB565 picture the screen is most like.
- **Its directory** is `neochumby-emulate-UID-PROFILE` in the temporary
  directory, or `--state DIR`: the stick, the NAND copy, the console's log
  and chumby-machine's sockets.
- **No network.** The emulated Chumby never has one (QEMU runs with `-nic
  none`). `start --server SOCK` joins its second UART (ttyS1 on 2.6.16,
  ttymxc1 on dist) to SOCK, a neochumbyd's `--listen-unix` socket, and
  writes `server=serial` in the stick's neochumby.conf: the session then
  reaches neochumbyd over that line, with the same TLS sessions as over TCP
  (neochumby-proto's `mux`). Pairing and `delivery=sync` work there; NFS
  doesn't. The pairing it keeps goes back in /psp at the next start
  (`dev/emulate kept` copies it out), and `--psp` starts afresh.

`python3 dev/test_emulate.py` (CI runs it) checks the images dev/emulate
makes and its whole control path against a stub QEMU, without firmware.

### Scenarios

`dev/e2e TREE NEOCHUMBYD OUT` runs them all on the profile and writes a
summary to attach to a pull request; `E2E_SCENARIOS` picks some. What
passes where, so far:

| Scenario | stock | dist | Why not |
| --- | --- | --- | --- |
| `boot` | passes | passes (CI) | |
| `smoke` | passes | fails | on 5.9.16 the kernel's framebuffer console blinks its cursor over the session's screen (`vt.global_cursor_default=0` stops it), so the screen seldom settles; and the session reads touches from `/dev/input/event0`, which there is the top button (the touchscreen is `event1`), so no tap reaches it; there is no `/dev/switch` either |
| `stock` | passes | skipped | the stock firmware's own boot |
| `classic` | not run yet | not run yet | it needs a renderer's pictures; on dist it needs what smoke does, and `/dev/switch` |
| `link` | passes | passes (5.9.16) | not in CI: it runs a neochumbyd here, at the other end of the serial line, and a round over the line takes minutes; `pairing` and `powercut` likewise (both pass on classic) |

The session on dist still uses the stock kernel's `/dev/switch` and
`/dev/accel`, where 5.9.16 offers evdev and IIO: the scenarios that need
devices pass there once the session (or dist) bridges the two. The
backlight and the speaker's mute it already finds as each kernel has them:
`/proc/sys/sense1` on 2.6.16, else the backlight class and the sound
card's `Speaker Switch`.

**boot** (`dev/emulate-boot TREE OUT`): the boot runs the stick's hook,
the hook runs the session from the stick, and home opens and draws (with
its CHUMBY MODE bar on the stock 2.6 kernel; on dist, where home has
none, until its screen settles).

**smoke** (`dev/emulate-smoke TREE OUT`, a tree with doom): home draws, a
tap on DOOM opens Doom (without game data it says so and exits, and home
comes back), the corner hold restarts home, the top button reaches the
session when it holds `/dev/switch`, `neochumby sensors` on the console
sees a touch (dist, classic), and CHUMBY MODE ends the session cleanly.

**stock** (`dev/emulate-stock TREE OUT`, a tree with `./dev/probe`, at
most four apps; the stock profile only): the stock boot runs the hook,
PROBE draws its pattern and sees a tap where it was made, the corner hold
ends it and home stays, and after CHUMBY MODE the stock software carries
on to its own screens (light and mostly pale blue; nothing of its artwork
is compared or kept).

**neochumby-classic** (`dev/emulate-classic TREE PICTURES OUT`; run by
`dev/e2e` when `NEOCHUMBY_CLASSIC_PICTURES` names the pictures). classic is
the start app (`start=classic` in `/psp/neochumby.conf`), with pictures
from neochumby-classic's offline smoke render, made with
`NEOCHUMBY_TREE=TREE` so that its APPS screen lists the tree's apps:
made-up data and placeholder artwork. The script checks that:
- classic draws a slideshow page;
- the top button toggles Home and back without restarting classic;
- Home's APPS, then the DOOM tile, opens Doom through the session's pipe,
  and classic comes back when Doom exits;
- classic's service starts once, hears that Doom is in front, and keeps
  running;
- CHUMBY on Home ends the session with exit 0.

Where it taps comes from the pictures' own tap zones.

**link** (`dev/emulate-link TREE NEOCHUMBYD OUT`): a neochumbyd here, at
the other end of the emulated Chumby's serial line. PAIR pairs over the
line, with the code both ends show; at the next start neochumbyd has the
Chumby's log and its screen over the developer link, and with
`delivery=sync` a first round fills set a of its stick, which the boot
after runs.

**Pairing** (`dev/emulate-pairing TREE NEOCHUMBYD OUT`). The
Chumby taps PAIR against a neochumbyd on the PC, and the code it logs must
be the one `neochumbyd pairing` lists. A wrong code and a six-digit one are
turned down, a refusal leaves `/psp` alone, and an approval (typed in
capitals, without dashes) pins neochumbyd's key in the conf, after which
the next boot's log reaches `neochumbyd log`. A new key under the same name
pairs only with `--replace`, and once forgotten, the Chumby is turned away.
It runs over the serial line, as link does.

**A power cut during a sync round** (`dev/emulate-powercut TREE NEOCHUMBYD
OUT`; over the serial line, as pairing). The Chumby pairs with a
neochumbyd, syncs its tree to its stick (`delivery=sync`), then boots
through the hook while a second round writes the other set, and
`dev/emulate powercut OUT.img` cuts the power: the machine stopped where it
is, the stick image copied as it stands, QEMU killed. Booting that copy,
the log must show the hook running a complete set, `check-set` agreeing,
and the next round making the new set live, which one more boot runs. With
`--device` the person beside the Chumby is asked to pull the plug. (Every
cut point of a round, on the host, is neochumby-sync's own crash test.)

### A real Chumby: --device NAME

The same scenarios run on a real Chumby, paired with a neochumbyd and set
`dev=on` (`neochumbyd set NAME dev=on`, which allows remote input), over
its developer link: `NEOCHUMBY_EMU_DEVICE=NAME dev/emulate-smoke /tmp/tree
/tmp/smoke`, with `NEOCHUMBY_EMU_HOST=HOST` when neochumbyd runs elsewhere
(over ssh; its state in `NEOCHUMBYD_DIR`). `start` installs the tree there
(`tools/deploy --tree`) and restarts the Chumby; `log` is its log from then
on; `shot`, `tap` and `button` are neochumbyd's (`neochumbyd tap` and
`button`; an older neochumbyd without them skips those steps). A remote
press goes where a real one would, so it can't reach an app that reads the
switch itself (`button = true`, Doom's FIRE): a scenario needing that skips
on a device. Steps that need hands, like pulling the power, are
`dev/emulate prompt` lines, shown only with `--device`.

So that one scenario holds on every backend, it asserts on lines the
session and apps log on purpose, compares only our own screens (with what
changes masked), scales its waits by `dev/emulate scale`, and exits 77,
skipped, where a backend can't do a step.

What the emulator can't catch is what real devices are for:

- **Real input.** Taps are clean: no echo after a release, no brief releases
  during a hold, and the button never bounces.
- **Real USB and flash.** The stick is a file: a slow stick, one pulled
  out, and what a stick does with a write when the power goes aren't
  reproduced.
- **Speed.** QEMU's ARM926 isn't a 266 MHz one, so frame rates and CPU
  load mean little.
- **The network.** The emulated Chumby has none.

## Writing an app

An app is a crate named `neochumby-<name>`. It lives in this repository
under `apps/<name>/`, or in its own repository with the crate at the root,
where it is added to a tree from its git URL or from crates.io
(`tools/tree OUT git+https://github.com/you/neochumby-weather#REV`).
cargo-deny sees only this repository's `Cargo.lock`, so an app
`tools/tree` fetches from git or crates.io, and what it depends on, isn't
checked for licences or advisories: pin it (a full commit id, a version),
and read what you pin. Its `Cargo.toml` says what it is:

```toml
[package]
name = "neochumby-weather"
# ...

[dependencies]
neochumby = { git = "https://github.com/johnmerchant/neochumby" }

[package.metadata.neochumby]
title = "WEATHER"                    # listed in app lists
exec = "neochumby-weather"           # runs in front when the app is opened
color = "4a90d9"                    # optional; `colour` works too
service = "neochumby-weather-sync"   # optional: runs in the background
companion = "neochumby-weather-companion"   # optional: its server half
server = "neochumby-weather-web"     # optional: once on the server
listen = [8080]                      # optional: the ports `server` may bind
neochumbyd = "optional"              # "required" | "optional" | absent
start = true                         # optional: it may be the start app
interrupt = false                    # optional: no service replaces it (games)
button = true                        # optional: it gets the top button (games)
access = ["system"]                  # optional: what it reaches beyond its jail
player = true                        # optional: a player (below)
font = "spleen-8x16"                 # optional, text mode: its terminal's font
tap = "q"                            # optional, text mode: what a tap types
frontend = "airplay"                 # optional: it shows another app (below)
linux = "5.9"                        # optional: the oldest Linux it runs on (tools/tree --linux)
```

`tools/tree` checks the table and writes the tree's `manifest` from it
(`neochumby-manifest`). An app with a `companion` must also say
`neochumbyd = "required"` or `"optional"`: it is never inferred. A
`files/` directory beside `Cargo.toml`, optional, is copied into the app's
directory as it is (icons, configuration). An app in its own repository
may keep third-party source in submodules: `tools/tree` checks them out
for a `git+` app (in `~/.cache/neochumby/app-sources`), and a crate
published to crates.io carries their files in its package.

Binaries named by `exec` and `service` are built for the Chumby
(`armv5te-unknown-linux-musleabi`), and a `companion` and a `server` for
the server. An app may be only a server program. Depend
on the `neochumby` crate for the screen (`fb`), touch, squeeze and
accelerometer (`input`), the manifest and settings (`apps`, `conf`),
crash reports (`debug`) and the request pipe (`session`). A program
written for a terminal needs none of that: it runs in text mode (below),
as `apps/htop` and `apps/top` do. A C program that uses curses depends on
`neochumby-ncurses` and builds against `DEP_NCURSESW_INCLUDE`
(`apps/htop/build.rs`). The screen
is 320x240 RGB565, so prerender what you can, and keep in mind that the CPU
is a 266 MHz ARM926 with no FPU.

### Text mode

An app's `exec` runs in **text mode** unless its `access` lists `fb`.
Text mode is the default, and what a program written for a terminal wants:

- **Its stdin and stdout are a terminal** of its own, a pseudo-terminal
  with `TERM=linux`, as many characters as its font fits on the screen
  (64x30 in Spleen 5x8). Its stderr is the session's log, as every app's
  is. It has no framebuffer and no touchscreen device of its own.
- **The session shows it.** On the stock 2.6.16 kernel, which has no
  console on the screen, the session draws it (`neochumby::term`: the
  Linux console's escape sequences, and xterm's 256 colours, alternate
  screen and line drawing besides). On the dist kernels (Linux 5.9 and
  7.2), the session passes its output to the kernel's console, tty1, for
  the kernel to draw: the font is loaded there (BusyBox's `loadfont`) and
  the colours go in its palette (`ESC ] P`). Either way the app never holds
  the screen, so it behaves the same on both. Fonts taller or wider than
  32 pixels (Spleen 32x64), which the console can't take, are drawn by the
  session there too. The console has only 16 colours, so a scheme's
  background and foreground take colours 0 and 7 there.
- **Touches arrive on its stdin** as xterm's mouse reports, the left
  button, once the program asks for them (curses' `mousemask` does) with
  DECSET 9, 1000, 1002 or 1003, in the encoding it asked for. With 1006,
  SGR's: `ESC [ < 0 ; COL ; ROW M` as the finger goes down, `ESC [ < 32 ;
  COL ; ROW M` as it moves to another character (1002 and 1003 only), and
  `ESC [ < 0 ; COL ; ROW m` as it lifts, COL and ROW counted from 1.
  Without 1006, X10's: `ESC [ M` then three bytes, 32 plus the button (0
  down, 32 moved, 3 up), 32 plus COL and 32 plus ROW. curses' `linux`
  entry asks for X10's; a program of your own should ask for SGR's. Before
  the program asks for the mouse, a tap types the manifest's **`tap`**
  instead (`"q"` quits top; `"\n"` ends a read in line mode, as fetch
  waits), or nothing without one. A USB keyboard on a dist image types
  into it as well.
- **Its font** is the manifest's **`font`**: `spleen-5x8` (the default),
  `spleen-6x12`, `spleen-8x16`, `spleen-12x24`, `spleen-16x32` or
  `spleen-32x64` (Spleen 2.2.0, `crates/neochumby/fonts`, BSD-2-Clause), or
  a PSF file (`.psf`, `.psfu`) in the app's directory, from `files/`. The
  owner's `font=` setting overrides it for every app.
- **Its colours** are the owner's (`colors=` in the settings): GitHub Dark
  unless they choose otherwise (README, "Apps").

It ends as any app does, and the terminal closes with it and with
whatever it started. An app with `fb` can still show a program on a
terminal of its own with `neochumby::term::run` (it then needs `touch` and
`terminal` too).

### The app contract

The session (`neochumby`) runs one app at a time in front, full-screen.
Every app is opened the same way, whoever asked for it.

**When your app is opened**, the session runs its `exec`:
- jailed, as below, in the app's directory in the tree, in a process group
  of its own;
- with `NEOCHUMBY_ROOT` (the tree) and `HOME=/tmp`. The tree is read-only;
  `/tmp` is your app's own;
- with `NEOCHUMBY_SESSION=<fd>`, its request pipe to the session (below);
- with `NEOCHUMBY_COMPANION=<fd>` if it has a companion (below);
- with `NEOCHUMBY_REASON` when the session has a reason to give: `button`
  when the top button opened it. Ignore values you don't know;
- with `NEOCHUMBY_NAME`, the Chumby's name as its neochumbyd knows it
  (`neochumby::session::name()`);
- with `NEOCHUMBY_STICK_FREE_KIB`, the free space on the stick in KiB as
  your app started, when there's a stick (`session::stick_free_kib()`);
- with `NEOCHUMBY_STORAGE`, your app's lasting directory, when its
  `access` lists `storage` (below);
- with `NEOCHUMBY_PLAYER` naming this Chumby's default player, when it
  has one (below);
- with `NEOCHUMBY_FRONTEND` or `NEOCHUMBY_CLIENTS` for a frontend the
  owner approved, and for the app it shows (Frontends, below);
- in text mode, with a terminal as its stdin and stdout (above); with `fb`,
  owning the screen until it exits. What's on the screen when it starts is
  whatever was there: draw all of it;
- with `touch`, sharing the touchscreen: every reader gets every touch.
  The session watches the top-left corner for the escape hatch, whatever
  the mode;
- **the top button** only with `button = true`. Only one process at a time
  can have `/dev/switch` open, and the session keeps it. An app with
  `button = true` is handed the open file as `NEOCHUMBY_BUTTON=<fd>`, and
  `neochumby::input::Squeeze::open` picks it up. A read returns the level
  now, so act on a 0 → 1 change.

**To leave**, your app exits. Then the session opens the start app again,
and ends anything your app left running in its process group (SIGTERM,
then SIGKILL 200 ms later), so nothing you started keeps the screen, the
speaker or the button.
- **To open another app**, write `open <app>` (the app's name, its
  directory under `apps/`) to the request pipe, then exit. The session runs
  that app next.
- **For CHUMBY MODE**, write `chumby-mode`, then exit. The session stops
  the services and hands the Chumby back to its own software.
- Lines end in `\n`. Each end ignores lines it doesn't know, and the
  session ignores an `open` for an app that isn't here or can't run. The
  last request before you exit counts.
- After a request, exit within 5 seconds. Otherwise the session ends your
  app: SIGTERM to its process group, then SIGKILL 2 seconds later.
- **The escape hatch:** holding the top-left corner for 3 seconds ends
  your app the same way, whatever it is doing, and opens `neochumby-home`.
  So handle SIGTERM by exiting promptly, and expect anything you started to
  be ended with you.
- **The top button:** unless your app says `button = true`, a press ends
  it the same way and opens the start app with `NEOCHUMBY_REASON=button`,
  as the stock Chumby does. If your app is the start app, it gets a
  `button` line on its pipe instead (below). There's no hold gesture: games hold the button
  to fire, and the corner hold is the way out of anything.
- **A service** may end your app the same way, to open another: any
  service over the start app, only the start app's over other apps (an
  alarm, say). Declare `interrupt = false` if your app mustn't be ended
  like that: games do.

In Rust:

```rust
use neochumby::session::Pipe;

if let Some(mut pipe) = Pipe::from_env() {
    let _ = pipe.open("doom"); // or pipe.chumby_mode()
}
std::process::exit(0);
```

`Pipe::from_env()` is None when the app wasn't started by the session (run
by hand).

**The jail.** The session runs as root; your app doesn't. Its `exec` and
its `service` run as a user of their own (one uid per app, from 20000), in
a private mount namespace, chrooted into a small tmpfs that holds:
- your app's directory from the tree, at the same path, read-only;
- `/tmp`, your app's own (4 MB, `nosuid,nodev,noexec`), which is `HOME`;
- `/dev/null`, `zero`, `random` and `urandom`;
- what its `access` lists, and nothing else:

| `access` | what your app gets |
|---|---|
| `fb` | `/dev/fb0`, for its `exec` only, never its `service`, in place of a terminal: it draws the screen itself |
| `touch` | the touchscreen and its calibration (`/psp/ts_settings`, read-only), likewise. A text-mode app gets touches on its stdin without it |
| `speaker` | `/dev/dsp`, and the speaker's handover (`neochumby::audio`). The mute and the volume are the session's: `audio` asks for them over the request pipe |
| `accel` | the accelerometer |
| `system` | `/proc`, `/sys` and `/etc` |
| `network` | `/etc/resolv.conf` and `/etc/hosts`; and internet sockets (below) |
| `terminal` | `/dev/ptmx` and `/dev/pts`, for a pseudo-terminal (`neochumby::term`) |
| `storage` | `/psp/apps/<name>/`, its own, lasting across boots, shared by its `exec` and `service` (below) |
| `logs` | the Chumby's logs, read-only, each at its own path: the session's (`/tmp/neochumby.log`), `/tmp`'s other `*.log` and `/var/log`'s files, as they are when it starts (plain files only). The kernel's ring needs no access: `neochumby_sys::klog` |
| `root` | no jail: it runs as root, as the session does. Listed alone, or with `fb` for an app that draws the screen itself: without `fb`, a root app runs in text mode too |

It may have 64 processes and 256 open files, and dumps no core. Its users
can't signal or trace each other, or root, and can't read the Chumby's
key or write to `/psp`, except its own `storage`.

**Storage.** With `storage`, your app has `/psp/apps/<name>/` in the
Chumby's settings flash, named by `NEOCHUMBY_STORAGE` (the same path in
the jail and out). Its `exec` and its `service` share it: they run as the
same uid. Keep settings there, such as a station or a volume; anything
else belongs in `/tmp`.
- The session makes it as either starts, mode 0700, and gives it and
  everything in it to your app's uid. Uids follow the apps' order in the
  tree, so they can change between boots.
- It may hold **64 KiB**: each file, directory or link counts its size
  rounded up to 512 bytes, and at least 512. `/psp` is 2 MB in all.
- The kernel has no quotas, so the session measures every 2 seconds. An
  app over its limit is ended, `exec` or `service`, as for the network.
  One already over as the session started may stay that size, to tidy up,
  but not grow. Write small files, and replace them whole (write a new
  one, then rename it over the old).

**Playing a stream.** The stock player (`btplay` and its daemon) can't
run in a jail. An app with `speaker` may ask the session to run it, and
carry on:
- `play <url>` plays an `http://` stream (no TLS) of at most 200 bytes.
  It may hold only the characters a URL may hold unescaped
  (`neochumby::session::valid_url`). The session runs `/usr/bin/btplay
  --start-daemon <url>` as root, with the URL as one argument and no
  shell. A line that isn't such a URL is ignored, like any unknown line.
- `play off` stops your app's stream (`btplay --passthru=stop`).

The player plays one stream, so a newer `play` from any app replaces
yours. The session also stops your stream:
- when the `exec` or `service` that asked ends;
- when another process takes the speaker (the handover in
  `neochumby::audio`: the newest player wins). So take the speaker with
  `audio::Unmuted` first, then `play`, and `play off` when asked to yield.
  A stream played without the speaker plays muted, and the session leaves
  it;
- at CHUMBY MODE.

**The backlight.** Only root may set it. The start app and its service
may write `brightness <n>` (0, off, to 65535, full), and carry on. The
session writes `/proc/sys/sense1/brightness` (on a newer kernel without
it, the first `/sys/class/backlight` device, scaled to its
`max_brightness`), and puts back the level the Chumby booted with at
CHUMBY MODE. From any other app it's refused and logged.

In Rust: `Pipe::play(Some(url))`, `Pipe::play(None)` and
`Pipe::brightness(n)`.

**Your binaries.** Each `exec` and `service` must be a static ARM ELF.
Before it starts one, the session refuses a file with a `PT_LOAD` below
`0x1000` (page zero), a `PT_INTERP`, or a non-ARM ELF, and it checks every
executable file in your app's directory the same way. Page zero is where
the ARM `mmap` check doesn't reach the ELF loader. Put helper programs in
the app's directory, not in `/tmp`: `/tmp` and the speaker handover are
`noexec`.

**The network.** The stock kernel can't take the network away from a
process. Instead, once a second, the session looks for internet sockets in
`/proc/net`, which lists each socket with its owner's uid. If it finds one
owned by an app whose `access` doesn't list `network`, it ends the app,
`exec` or `service`, as if it had crashed. Unix sockets aren't counted.
Ask for `network` if your app opens a socket at all, even briefly: an
unlisted one is found sooner or later.

List only what your app uses. `neochumby-manifest` (run by `tools/tree`)
refuses words it doesn't know. `root` is for `home`, which pairs the
Chumby and so writes its key. Under `dev/emulate`, which can't mount or
chroot, every app runs unjailed, as root.

**Start apps.** The start app is the one a Chumby opens at boot and comes
back to. The owner picks it (`start=<app>`), and it defaults to
`neochumby-home`. Declaring `start = true` lets your app be picked, and
promises:
- it **opens other apps** through the request pipe;
- it offers **CHUMBY MODE**, and the owner's settings, or at least points
  to where they are;
- it handles **the top button**, for example by showing its own main
  screen. A press while it's in front arrives as a `button` line on its
  pipe (read it with `Pipe::receive` and `Notice::parse`, without waiting).
  If the line can't be written (the pipe is closed or full), the session
  opens the start app again with `NEOCHUMBY_REASON=button`, as it does
  after a press over any other app;
- it **doesn't say `button = true`**, so a press always lands somewhere
  useful. The session refuses a start app that does;
- it **keeps running**. If it exits without asking for anything five times
  within a minute, the session makes `neochumby-home` the start app
  instead.

Without `start = true`, the session refuses the app as the start app and
uses `neochumby-home`, so a Chumby can't be left stuck in a game.

**Players.** A player is an app that keeps playing (internet radio,
music, podcasts) and that the start app can pause and resume. A Chumby
has a default player, which a start app's MUSIC button opens and its MUTE
button pauses. The owner picks it
(`player=<app>`, set like `start`); otherwise it's `somafm` if that's in the
tree, else the first player by name, else there's none. Declaring
`player = true` lets your app be picked, and promises:
- it has an `exec`, which plays or shows what's playing, and lists
  `speaker` (or `root`) in its `access` (`neochumby-manifest` refuses
  `player` without them);
- its **service** plays on after the `exec` exits, and acts on two lines
  from the session: `pause` (stop playing, ready to resume) and `resume`
  (play again). It ignores lines it doesn't know, as every service does.

The session resolves the default as it starts, as it does the start app: a
name that isn't a player here is refused, and the reason logged. It
sets `NEOCHUMBY_PLAYER=<app>` for everything it runs, and leaves it out when
there's no player, so a start app can grey out its MUSIC button. A
changed `player=` applies at the next session start, as `start=` does.

A start app, and its service, may also write these to the request pipe,
and carry on (they aren't followed by an exit):
- `player pause`, `player resume`: the session tells the default player's
  service `pause` or `resume`;
- `player toggle`: `pause` if the session last told it `resume` (or nothing
  yet: it starts as playing), else `resume`.

From any other app or service, `player` is refused and logged. A line
that can't be passed on (no player, no service running, or its pipe
full) is dropped and logged.

In Rust:

```rust
use neochumby::session::{self, Pipe, Player};

let mut pipe = Pipe::from_env();
// MUSIC: open the default player, if there is one, then exit.
if let (Some(app), Some(p)) = (session::player(), pipe.as_mut()) {
    let _ = p.open(&app);
}
// MUTE, from a start app: pause or resume it, and carry on.
if let Some(p) = pipe.as_mut() {
    let _ = p.player(Player::Toggle);
}
// In the player's service: Notice::Pause and Notice::Resume.
```

### Services

What has to keep working while another app is in front (alarms, a night
schedule, music) belongs in the app's `service`. The session runs each
app's service, where what it needs of a neochumbyd is here, for as long as
neochumby runs:
- with the same environment as an app (`NEOCHUMBY_ROOT`, `HOME=/tmp`,
  `NEOCHUMBY_SESSION`), in a process group of its own, in the app's jail
  but without the screen or the touchscreen;
- **restarted when it exits**, after 1 second, then 2, 4 and so on up to a
  minute. A run of a minute or more starts the wait again from 1 second;
- **told what's in front:** `front <app>` on its pipe as it starts, and
  whenever another app comes to the front. Reading these is optional. A
  line is never cut; while the pipe is full, only the latest notice waits;
- **stopped at CHUMBY MODE:** SIGTERM to its process group, then SIGKILL 5
  seconds later.

A service may write `open <app>` or `chumby-mode` to its pipe, like an
app, but it doesn't exit after. That ends the app in front, so the session
takes it only:
- over the start app, from **any service** (an alarm, say); over any other
  app, only from the **start app's service**, so services don't fight over
  the screen. `chumby-mode` only from the start app's service;
- when the app in front doesn't say `interrupt = false`;
- not after the escape hatch, until an app is opened by hand (an `open`
  from the app in front), so a service can't trap the Chumby in an app gone
  wrong;
- not once the app in front has asked for something itself: what it asked
  for wins.

Which services run is decided as the session starts: one that needs a
neochumbyd starts on a paired Chumby only, and pairing asks for a restart.

Anything else is ignored and logged, and so is an `open` for the app
already in front.

**The speaker.** When a service exits, the session mutes the speaker and
puts back the master volume the Chumby booted with (`/psp/volume`),
through `neochumby::audio`, so a service that dies while playing can't
leave it on. If another live process holds the speaker (the handover's
claim, which `audio::Unmuted` and `audio::Speaker` make), it's left alone.
As the session ends, it does the same, waiting up to 3 seconds for the
volume.

In Rust, a service reads its notices with `Pipe::receive`, which doesn't
wait, and `Notice::parse`.

**Companions.** An app with a server half ships it as a second binary,
named by `companion`. neochumbyd runs it for each paired Chumby that opens
the app:
- **Opening:** the Chumby side calls
  `neochumby::companion::open(neochumby::companion!())`, which returns a
  `Link` to its companion. `companion!()` reads the app's `Cargo.toml` as
  it compiles: an app without a `companion`, or without `neochumbyd`, does
  not compile.
- **Through the session:** apps never hold the Chumby's key. The session
  opens the connection to neochumbyd for your app, and relays its frames
  over a socket it hands the app (`NEOCHUMBY_COMPANION`). It opens the
  companion of the app it started, named by the app's directory, so an app
  reaches only its own. Run by hand, as root, an app connects itself.
- **Messages:** define a type for each direction in the app's library
  crate, and a `neochumby_proto::channel::Channel` that pairs them (serde
  derives; postcard on the wire). The Chumby side sends and receives them
  with `open(...)?.typed::<YourChannel>()`, the companion with
  `neochumby_proto::channel::stdio::<YourChannel>()`; each end can send
  only its own direction's type, so halves that disagree don't compile.
  `apps/logs` does this.
- **Frames:** for raw bytes instead, `Link::send` and `Link::recv` (`None`
  once the companion has gone). On the companion's side they arrive on
  stdin and leave on stdout, a u32 big-endian length then the bytes
  (`neochumby_proto::frame`); a typed message is one such frame.
- **Environment:** the companion gets `NEOCHUMBY_CHUMBY` (the Chumby's name)
  and `NEOCHUMBY_STATE` (a directory of its own, also its HOME, with its
  TMPDIR in it). It should exit when its stdin closes.
- **The sandbox:** it may write only its own directory, and read the tree
  and the system ([SECURITY](SECURITY.md) has the details). Its stderr goes
  to neochumbyd's log. It gets only a short list of neochumbyd's
  environment: what it needs from the host comes in a `NEOCHUMBY_`
  variable, such as AirPlay's `NEOCHUMBY_AIRPLAY_PORT`.
- **Its own uid:** with `--app-uids`, a companion runs as a uid of its
  app's own (the same as its app's server program), with no other
  groups. Keep everything it writes in `NEOCHUMBY_STATE`: it can't write
  neochumbyd's state, the tree or another app's directory.

`apps/airplay` is a worked example of both halves, and `apps/logs` one
whose companion is optional.

**Server programs.** A server half that runs once on the server for all
its Chumbys, not once per Chumby (a household web page), is named by
`server`:
- **Starting:** neochumbyd starts it when it starts, sandboxed as a
  companion is. If it exits, it is started again after 1 s, then 2, 4 and
  so on up to a minute.
- **Stopping:** when neochumbyd stops, it gets SIGTERM, and SIGKILL five
  seconds later.
- **Environment:** `NEOCHUMBY_STATE` (the app's own directory, shared with
  its companions; its working directory and HOME, with its TMPDIR in it)
  and `NEOCHUMBY_TREE` (the app tree, to read the apps' manifests). Its
  stdout and stderr go to neochumbyd's log.
- **Ports:** it may bind only the TCP ports `listen` names, on kernels
  whose Landlock can say (Linux 6.7 on).
- **The admin socket,** if its owner allows it (`neochumbyd allow <app>
  pairing`): descriptor 3, with `NEOCHUMBY_ADMIN=3`. Mark it close-on-exec
  as the program starts (in Python, `os.set_inheritable(3, False)`). It's
  lines of text: send `paired`, `pending`, `approve CODE TOKEN`,
  `refuse CODE TOKEN`, `forget NAME TOKEN`, `set NAME KEY=VALUE TOKEN` or
  `check TOKEN`; the answer is lines of data, then `ok` or
  `error MESSAGE`. `crates/neochumbyd/src/admin.rs` has the details.
- **Every change needs the owner.** Changes take the owner's token
  (`neochumbyd token`). A web page asks the person for it, and should
  check it (`check TOKEN`) before it lets them change anything.
**Frontends.** An app's exec, in front, can talk to its own service:
the service listens with `neochumby::service::listen("<app>")`, and the
exec connects with `neochumby::service::connect("<app>")`; each gets a
`UnixStream`, and what goes over it is the app's. The socket is the
session's: it binds each service's as it starts, before any app runs,
and hands it to the service (`NEOCHUMBY_LISTENER`), so no app can take
another's name first. Another app can do the
same as that app's **frontend**: it shows the app in its place. Its
manifest says which (`frontend = "airplay"`), with an `exec`, in text
mode or with `fb`. The owner approves it in the Chumby's settings,
`neochumbyd set NAME frontend_<app>=<frontend>` (`-` in the app's name
written `_`; or the same line in `/psp/neochumby.conf`), from the
session's next start. Then:
- the frontend is **opened in the app's place**, however the app is
  opened: from an app list, by a MUSIC button for a player, or by the
  app's own service;
- **its uid is let in** by the app's service (`NEOCHUMBY_CLIENTS`), and it
  is told which app it's for (`NEOCHUMBY_FRONTEND`). `connect` takes only
  a socket the session made. Unapproved, the service turns it away.
  Where jails are off (the emulator), every app is one user and nothing
  tells them apart;
- the app keeps everything else: its service, its companion, its
  `access`, and the player's `pause` and `resume`.

An app has at most one approved frontend. One that isn't in the tree, or
whose manifest names another app, is refused and logged, and the app
opens as itself.

### Writing an AirPlay frontend

AirPlay is three programs: the receiver, its companion on neochumbyd
(shairport-sync, with metadata and remote control); its service on the
Chumby, which keeps the only connection to the receiver, plays the audio
and is the player; and a frontend, in front. Its own is the now-playing
screen in text mode (`apps/airplay/src/frontend.rs`). Yours, in a
repository of its own, can take its place:

- **Depend on `neochumby-airplay-proto`** (`apps/airplay/proto`), as
  well as `neochumby`, from this repository's URL: it stays
  `publish = false`, off crates.io, until the first frontend outside
  this repository needs it there. Its
  `frontend` module has the messages, and its `VERSION` their protocol's
  version: messages are only ever added, and one a frontend doesn't know
  is skipped.
- **Connect** with `neochumby::service::connect(neochumby_airplay_proto::APP)`.
  The service says `Hello`, then everything it knows: whether it has its
  receiver, the receiver's name, the track, the cover (RGB565, at most
  128x128) and the state (playing or paused, the time gone and the
  length, the volume), and each change after. `NowPlaying` keeps them, and
  runs the time on while playing. Send `ToService::Control` for play/pause,
  next, previous and the volume. When the connection closes (the service
  restarts with its receiver), connect again.
- **The manifest:** `frontend = "airplay"` and an `exec`; `fb` and
  `touch` in `access` to draw the screen yourself, or neither for text
  mode. Not `player`, `service` or `companion`: those stay AirPlay's, and
  AirPlay's service still opens AirPlay, and so your frontend, when a
  stream begins.
- **The owner approves it:** `neochumbyd set NAME frontend_airplay=<yours>`.

```rust
use std::time::Instant;

use neochumby_airplay_proto::frontend::{NowPlaying, ToFrontend, ToService};
use neochumby_airplay_proto::{APP, Control, Reader, send};

fn main() -> std::io::Result<()> {
    let mut sock = neochumby::service::connect(APP)?;
    // What's playing, as it changes.
    let (mut reader, mut np) = (Reader::default(), NowPlaying::default());
    while let Some(m) = reader.next::<ToFrontend>(&mut sock)? {
        np.apply(m, Instant::now());
        println!("{} - {} ({:?})", np.track.artist, np.track.title, np.state.play);
    }
    Ok(())
}

/// A tap on play/pause.
fn play_pause(sock: &mut std::os::unix::net::UnixStream) -> std::io::Result<()> {
    send(sock, &ToService::Control(Control::PlayPause))
}
```

To wait on touches and the service together, as AirPlay's own does,
`neochumby_sys::fd::readable` takes both. `dev/emulate-airplay` tests a
frontend on the emulator, where the receiver is a stand-in
(`dev/airplay-standin`).

## Files in the tree

With `delivery=sync`, a Chumby keeps a copy of the tree on its FAT stick.
Names must be ones it can take: A-Z a-z 0-9 `.` `_` `+` `-`, not starting
or ending with `.`, unique ignoring case, and not a DOS device name (`con`,
`aux.txt`). `neochumbyd manifest` lists anything left out. Only files that
change are sent, so keep names stable and leave identical files alone.

## Releases

`gh release create vYYYY.MM.N --generate-notes` on main (`N` counts the
month's releases from 0: `v2026.10.0`) cuts a dated release; CI's
`release` job then copies that commit's prebuilt, attested images and their
sources onto it from `dist-prebuilt`, and links qemu-chumby's build (a
manual CI run on main with `release` set does the same, creating the
release). It builds nothing, so cut one once main's CI has finished.
The mainline kernel track's image isn't in a release: CI builds it only
weekly (dist/README.md, "Mainline, weekly"), and the newest is on
`dist-prebuilt`.

## Publishing the SDK crates

Six crates are published on crates.io, as an SDK for apps outside this
repository: experimental, and any release may break them. In dependency
order:

1. `neochumby-sys`, `neochumby-proto`, `neochumby-macros`,
   `neochumby-softfloat`
2. `neochumby-sync`
3. `neochumby`

The rest stay `publish = false`. `neochumby-ncurses` 0.0.1 is on
crates.io, but it isn't published any more.

- **A crate is released by hand, with a tag.** Raise its `version` in its
  Cargo.toml, and in `[workspace.dependencies]`, where the others take it
  by path and version, in a pull request. Once that's merged, tag the
  commit on main and push the tag:

      git tag neochumby-sync-v0.0.2 origin/main
      git push origin neochumby-sync-v0.0.2

  CI's "Publish to crates.io" job then publishes that one crate. It
  checks that the tag names a published crate, at the version its
  Cargo.toml says, on a commit on main; nothing else runs for the tag. A
  crate whose dependency changed needs that dependency released first.
  At 0.0.x every release is incompatible with the one before, so raise
  the requirement in its dependents too, and release them after it.
  These versions are not the bundle's dated releases (vYYYY.MM.N).
- **Re-runs.** Re-running the job for a version already on crates.io
  passes and publishes nothing.
- **What's packaged.** Each crate's `include` lists what it carries: what
  it builds from, its README and its licence texts. CI's "Package the SDK
  crates" job packages them all on every run and builds each from its
  package, on the host and for the Chumby (`cargo publish --dry-run`), so
  `cargo package --list -p <crate>` and that job are where to look after
  changing one.
- **Later:** at 0.1 and above, `cargo semver-checks` against the published
  release will choose between a patch and a minor release.

## Pull requests

Keep a pull request to one change, explain why in its description, and make
sure CI is green. If you're changing how a Chumby boots
(`boot/userhook2`), test on a real one or say that you couldn't: a hook
that hangs stops the Chumby's boot. The hook runs under BusyBox 1.6 `sh`,
which has `sed` but no `sha256sum`, `printf`, `od` or `head -c`. Anything
it can't do goes in the session binary as a subcommand, like `neochumby
check-set`. Raise its `neochumby-hook-version` line with every change: the
session replaces the hook installed in `/psp` only with a higher version.

### Test on a Chumby, or an emulated one, and show it

CI builds and unit-tests everything, but it can't tell whether a change
works on a Chumby. So a pull request that changes what runs on the Chumby
(`crates/neochumby`, `crates/neochumby-sys`, `apps/`, `boot/`) is
approved only once its description, or a comment, shows it running. There
are three ways, from quickest to most faithful:

1. **The emulator on the dist profile** (qemu-chumby's machine booting
   neochumby's Linux 5.9.16, with your tree on a USB stick): `dev/e2e TREE
   NEOCHUMBYD OUT` runs the scenarios, as CI does, or drive it by hand
   with `dev/emulate start`, `tap`, `button`, `shot` and `log` (see
   [Testing without a Chumby](#testing-without-a-chumby-devemulate)). It
   runs on Linux and macOS without root, and needs no firmware; not every
   scenario passes there yet.
2. **The emulator on the stock profile**: the Chumby's stock 2.6.16
   kernel and firmware, from **your own device's** flash backup
   (qemu-chumby's README, "Images", says how to build the images; they
   never leave your machine), booting your tree from a USB stick as a
   Chumby does. Use it for anything that depends on the stock kernel, the
   boot or the handover to the stock software:
   `NEOCHUMBY_EMU_PROFILE=stock dev/e2e TREE NEOCHUMBYD OUT` runs the
   scenarios on it, with `dev/emulate-stock`, which only it can: neochumby
   inside the stock boot, handing back to the stock software. Put where
   your images are in `~/.config/neochumby/env.local`, which every
   checkout and worktree shares, or in `.env.local` at a checkout's root,
   which wins over it (git ignores it): `NEOCHUMBY_EMU_KERNEL=`, `_NAND=`,
   `_PSP=` lines, and for a real Chumby with `--device`, where its
   neochumbyd is (`NEOCHUMBY_EMU_HOST=`, `NEOCHUMBYD_DIR=`). `dev/emulate`
   and `dev/e2e` find them. CI can't, having no firmware (and it never
   will), so this suite is yours to run locally, and its results are the
   evidence.
3. **A real Chumby**, if you have one: deploy, then `neochumbyd log`,
   `neochumbyd shot` (see [On a Chumby](#on-a-chumby)). Needed for what no
   emulator models: timing, the touchscreen's noise, a USB stick's own
   behaviour, power cuts, the network.

**The evidence to include:**
- which of the three, and the commit you tested;
- the commands you ran, and what they reported: `dev/e2e`'s
  `OUT/summary.txt` (each scenario ok, skipped or FAILED), or a short log
  excerpt showing the behaviour;
- screenshots of **neochumby's** screens or your app's, as PNG, showing
  the change.

**Never attach:**
- the stock software's screens or artwork (the control panel, the Chumby
  logo or octopus, widgets), firmware, images, or anything from a
  device's settings ([docs/legal.md](docs/legal.md));
- anything that identifies you, your network or your device: addresses,
  host and network names, Wi-Fi names, a device's name or serial. Cut
  them from logs before pasting.

If you couldn't test something (no Chumby, or a path no emulator covers
yet), say which and why. A reviewer may run it on a Chumby for you.

### Scenarios run everywhere

The automated scenarios (`dev/emulate-boot`, `dev/emulate-smoke`,
`dev/emulate-pairing`, `dev/emulate-powercut`, `dev/emulate-classic`,
`dev/emulate-stock`, and any you add to `dev/e2e`) use only
`dev/emulate`'s verbs, `start`, `log`/`wait log`, `shot`, `tap`, `button`
and `stop`, so the same scenario runs on each of the emulator's profiles
(`--profile stock|dist|classic`) and on a real Chumby (`--device NAME`,
through `neochumbyd`; remote input needs that Chumby set `dev=on`). Write
them so they pass on all of them:

1. **Assert on log lines** the session and apps print on purpose
   (`opening home`, `escape: ending …`), not on timing or on the order of
   unrelated lines. If a scenario needs to see something happen, print a
   line for it.
2. **Compare screenshots by regions of our own screens**, masking what
   changes (the clock, time-dependent pictures). Never compare against, or
   commit, the stock software's screens.
3. **Wait with timeouts the backend scales** (`dev/emulate scale`): the
   emulator boots a whole Chumby first, and a real Chumby is slow.
4. **What a backend can't do is skipped there, not failed**, with the
   reason: remote input can't reach an app that holds the top button
   itself (`button = true`), and the emulator has no network for a
   neochumbyd.
5. **Physical steps** (pull the power, plug in a stick) are prompts the
   runner shows and waits on, checked from the log afterwards; they run
   only with `--device` and someone there.
