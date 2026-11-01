# neochumby

New software for the original Chumby (the Chumby Classic, 2008): Linux 7 and
apps in Rust, booted from a USB stick, with the Chumby's own
firmware left as it is.

Nothing is flashed, and nothing on the Chumby is ever written: a USB stick is
the only way in. The stock boot script runs a hook from a stick, and the hook,
on the stock kernel, loads a module that hands over to Linux 7 in RAM
(`tools/neochumby-kernel-sideloader`). Linux 7 has no NAND driver, and runs
neochumby from the stick, which also holds the Chumby's pairing and settings.
To go back to the stock software, take the stick out, or put a file named
`neochumby-off` on it, and reboot.
Every binary is paged in as it runs, so apps cost the Chumby nothing until
opened.

An independent project, not affiliated with or endorsed by Chumby
Industries, Blue Octy or anyone holding rights in Chumby; "Chumby" is a
trademark of its owner. None of the Chumby's own software is distributed
here: see [docs/legal.md](docs/legal.md).

## The hardware

Freescale i.MX21: an ARM926EJ-S at about 266 MHz, with no FPU and 64 MB of
RAM. The screen is 320x240 RGB565 (`/dev/fb0`), with a resistive touchscreen, a
squeeze switch in the case and an accelerometer. Its firmware is Linux 2.6.16
with glibc 2.8, which neochumby leaves behind at boot: neochumby's own kernel
is Linux 7 (`dist/`).

## What's here

| Path | What | Licence |
| --- | --- | --- |
| `crates/neochumby` | The session (`neochumby`): runs one app at a time in front, starting with the start app, and the apps' services. And the library every app uses: screen, touch, squeeze, accelerometer, a terminal for programs written for one, the apps' manifests, the request pipe, crash reports and a profiler | MIT |
| `crates/neochumby/fonts` | The terminal's fonts: [Spleen](https://github.com/fcambus/spleen) 2.2.0, every size, as PSF (its README has the source) | BSD-2-Clause (`LICENSE.spleen`) |
| `crates/neochumby/themes` | The terminal's own colour schemes: GitHub Dark, the default and the only one built into the session (GitHub's [Primer](https://github.com/primer/primitives) colours, as [terminalcolors.com](https://terminalcolors.com/themes/github/dark/) gives them), and xterm's | MIT |
| `crates/neochumby/third_party/iTerm2-Color-Schemes` | [iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes) (3d8a6a1, a submodule that only `tools/tree` checks out): most of the tree's `colors`, from its `ghostty/` themes | MIT, for the collection; each theme's copyright is its author's, as its README credits them |
| `crates/neochumby/third_party/themes` | Schemes from their own repositories, over iTerm2-Color-Schemes' copies of them: [Catppuccin](https://github.com/catppuccin/ghostty) (b0b03cc: Frappé, Latte, Macchiato, Mocha), [Dracula](https://github.com/dracula/ghostty) (b0e6459), [Rosé Pine](https://github.com/rose-pine/ghostty) (d8b4ec0: and Dawn, Moon) and [Tokyo Night](https://github.com/folke/tokyonight.nvim) (cdc07ac: Day, Moon, Night, Storm), their Ghostty themes as they are; each one's `SOURCE` has its repository, commit and files | MIT (Catppuccin, Dracula, Rosé Pine) and Apache-2.0 (Tokyo Night), each `LICENSE` beside it |
| `crates/neochumby-softfloat` | libgcc's hand-written ARM soft float, which the library links into every app: float maths about twice as fast as the generic routines | GPL-3.0 with the GCC Runtime Library Exception |
| `crates/neochumby-proto` | How a Chumby and neochumbyd talk: TLS 1.3 with pinned keys, over TCP or a serial line | MIT |
| `crates/neochumby-sync` | The rounds that keep a copy of a Chumby's tree on its stick, both ends, and the checks the boot hook relies on. The receiver, `neochumby-sync`, is a binary of `crates/neochumby` | MIT |
| `crates/neochumby-guest` | The wasm runner's host API for apps built as WebAssembly: the screen, touches, the accelerometer, time and the session, as safe functions | MIT |
| `crates/neochumby-wasm` | `neochumby-wasm`, the runner for apps built as WebAssembly: WAMR in the app's jail, a host API that grants only what its manifest lists, and seccomp and Landlock where the kernel has them | MIT |
| `crates/neochumby-wamr` | [WAMR](https://github.com/bytecodealliance/wasm-micro-runtime), the WebAssembly Micro Runtime (WAMR-2.4.5, built by `wamr-sys` from [our fork of wamr-rust-sdk](https://github.com/johnmerchant/wamr-rust-sdk), pinned, with its patches), for the Chumby: its AOT loader and fast interpreter, with a small safe interface, for apps as WebAssembly (proposal 0003) | MIT; WAMR and wamr-sys are Apache-2.0 WITH LLVM-exception |
| `crates/neochumby-ncurses` | [ncurses](https://invisible-island.net/ncurses/) (6.6, a submodule), built static with the terminal's `linux` (and xterm-256color) compiled in, for the apps whose programs use curses (or, in a tree, the same built shared, by `tools/linux-libs`) | MIT, and ncurses' own X11-style licence |
| `crates/neochumbyd` | The server a stick is paired with: its apps synced to the stick, and the apps' server halves, native or WebAssembly under [Wasmtime](https://wasmtime.dev) (the `wasm-companions` feature, which release builds have) | MIT; Wasmtime is Apache-2.0 WITH LLVM-exception |
| `apps/home` | neochumby-home, the app list, in text mode: the installed apps, the time | MIT |
| `apps/airplay` | AirPlay: a receiver per Chumby on neochumbyd, played on the Chumby's speaker, with a now-playing screen in text mode; and `neochumby-airplay-proto`, its messages, for frontends of other apps | MIT |
| `apps/catalog` | GET APPS: the catalog on the Chumby, in text mode, from its neochumbyd, which installs an app only once its owner has approved it on the Chumby and on the server | MIT |
| `apps/logs` | LOGS: the Chumby's logs (the session's, the kernel's, /tmp and /var/log), and with a neochumbyd the host's, followed and scrolled by touch | MIT |
| `apps/doom` | Doom: [doomgeneric](https://github.com/ozkl/doomgeneric) (e257a095, a submodule) with a Rust platform layer | GPL-2.0 |
| `apps/fetch` | neochumby-fetch: [fastfetch](https://github.com/fastfetch-cli/fastfetch) (2.70.0, a submodule) in text mode, beside a Chumby | MIT |
| `apps/htop` | neochumby-htop: [htop](https://github.com/htop-dev/htop) (3.5.3, a submodule) in text mode, in Spleen 6x12, the touchscreen its mouse (F10, Quit, is past the 53rd column: the corner hold leaves it) | GPL-2.0-or-later |
| `apps/top` | neochumby-top: [procps-ng](https://gitlab.com/procps-ng/procps)' top (4.0.7, a submodule) in text mode, until tapped | GPL-2.0-or-later |
| `apps/cmatrix` | MATRIX: [cmatrix](https://github.com/abishekvashok/cmatrix) (v2.0, a submodule) in text mode, until tapped | GPL-3.0-or-later |
| `apps/clockwasm` | WASM CLOCK: a big digital clock built as WebAssembly, run by the wasm runner, and the same face built natively, to measure one against the other | MIT |
| `apps/clock` | CLOCK: [tty-clock](https://github.com/xorg62/tty-clock) (8868f3e, a submodule), until tapped | BSD-3-Clause |
| `apps/bonsai` | BONSAI: [cbonsai](https://gitlab.com/jallbrit/cbonsai) (v1.4.2, a submodule) growing a tree, a new one every 20 seconds, until tapped | GPL-3.0-only |
| `apps/pipes` | PIPES: [pipes-rs](https://github.com/dnorhoj/pipes-rs) (v0.1.1, a submodule), pipes.sh in Rust, until tapped | GPL-3.0-only |
| `apps/aquarium` | AQUARIUM: [asciiquarium-rs](https://github.com/cablehead/asciiquarium-rs) (beef5b7, a submodule), asciiquarium in Rust, until tapped | GPL-2.0-or-later |
| `apps/genact` | GENACT: [genact](https://github.com/svenstaro/genact) (1.6.0, from crates.io), looking busy, until tapped | MIT |
| `apps/ttfx` | TTFX: [ttfx](https://github.com/omacom/ttfx) (v0.5.1, a submodule), [TerminalTextEffects](https://github.com/ChrisBuilds/terminaltexteffects) by ChrisBuilds in Rust: "neochumby" and the time and date, through one effect after another, in Spleen 6x12, until tapped; Linux 5.9 and later | MIT (TerminalTextEffects' copyright kept in its LICENSE and NOTICE) |
| `apps/transitions` | TRANSITIONS: nine animations between apps (slide, iris, dissolve, pixelate, blinds, melt, glitch, CRT, tiles), for `transition=transitions` | MIT |
| `apps/quake` | neochumby-quake: id's WinQuake software renderer ([id-Software/Quake](https://github.com/id-Software/Quake), a submodule), made fast on the ARM926 (below) | GPL-2.0-or-later |
| `boot/userhook2` | The boot hook | MIT |
| `tools/` | The cross-compiling wrappers and `deploy` | MIT |
| `dist/` | The image: a Buildroot build of Linux 7 (`sideload-zImage`), and its initramfs (`sideload-rootfs.cpio.gz`) | MIT |
| `tools/neochumby-kernel-sideloader` | From the stock 2.6.16 to Linux 7: the module, and its build | MIT |
| `dev/` | Device-side test helpers: framebuffer capture to PNG, touch injection, mount test | MIT |
| `dev/wasmbench` | Proposal 0003's benchmark of WebAssembly runtimes on a Chumby: its workloads in Rust, and EEMBC's [CoreMark](https://github.com/eembc/coremark) (1f483d5, a submodule) | MIT; CoreMark Apache-2.0, with EEMBC's terms for its name |
| `emu/` | The emulator: [qemu-chumby](https://github.com/johnmerchant/qemu-chumby) at a pinned commit (a submodule), QEMU's `chumby-classic` machine with its runner and image tool, and the script that builds it | GPL-2.0-or-later, like QEMU (its image tool: MIT) |

Everything we write is MIT, except in `apps/doom`, `apps/quake`,
`apps/htop`, `apps/top`, `apps/cmatrix`, `apps/bonsai`, `apps/pipes` and
`apps/aquarium`. They link GPL programs (the games' engines, htop, procps'
top, cmatrix, cbonsai, pipes-rs, asciiquarium-rs), so each of those crates, our Rust in it included, is
GPL and lives in its own directory, with the program's upstream licence
in its submodule (`apps/quake/src/span.rs` is a port of
the engine's span drawer, so it could be nothing else). `apps/fetch` links
fastfetch, which is MIT as we are (its `LICENSE` is in its submodule), and
`crates/neochumby-ncurses` ncurses, whose licence is MIT's in other words.
Nothing links an app: each runs as a separate
program. The
soft-float routines' runtime exception puts no conditions on the programs
they're linked into.

The colour schemes are data, not code: `tools/tree` converts them into
one file, the tree's `colors`, with their licences beside it in
`colors.LICENSE`, and neither is committed. Nord, Solarized and Gruvbox
are iTerm2-Color-Schemes' copies: Nord's and Solarized's own repositories
have no Ghostty themes (only iTerm2's, Solarized's in a colour space of
their own), and Gruvbox's terminal colours (gruvbox-contrib) carry no
licence.

## Apps

Everything you see is an app, the app list included. An app is one crate,
`neochumby-<name>`, which installs as a directory `apps/<name>/` in the
tree, holding its binaries and a `manifest`. The manifest comes from the
crate's `Cargo.toml`:

```toml
[package.metadata.neochumby]
title = "QUAKE"                      # listed in app lists
exec = "neochumby-quake"             # binaries, by name; exec runs in front
wasm = "neochumby-x"                 # or, in place of exec, a binary built as WebAssembly
color = "8c5a28"                    # optional; `colour` works too
service = "neochumby-airplay"        # runs in the background, restarted if it exits
companion = "neochumby-airplay-companion"   # its server half, run by neochumbyd
server = "neochumby-web"             # runs once on the server, for all Chumbys
listen = [8080]                      # the TCP ports the server program may bind
neochumbyd = "required"              # or "optional"; absent: never needed
start = true                         # it may be the start app
interrupt = false                    # no service may replace it in front
button = true                        # it gets the top button while in front
access = ["fb", "touch", "speaker", "network"]   # what it reaches beyond its jail
player = true                        # a player: MUSIC buttons may open it (needs exec and speaker)
font = "spleen-8x16"                 # text mode: its terminal's font
tap = "q"                            # text mode: what a tap types until it asks for the mouse
frontend = "airplay"                 # it shows that app, in its place, once the owner approves
transition = "neochumby-transitions" # animates switching from one app to the next
```

An app can use any of these keys:
- **`neochumbyd`:** Doom and Quake don't need a neochumbyd, so they run from
  a stick on a Chumby nobody paired. AirPlay is `required`: on an unpaired
  Chumby it's listed greyed out and its service doesn't start.
- **`companion`:** an app with a server half ships it as a second binary.
  neochumbyd runs it for each Chumby that asks, and relays frames between
  the two halves over the paired connection. neochumbyd knows no app by
  name. An app with a companion sets `neochumbyd` too, and its Chumby half
  opens it with `neochumby::companion!()`, which fails the build if the
  app's `Cargo.toml` declares no companion. A companion that is a `.wasm`
  file (`companion=weather.wasm` in the tree's manifest) runs under
  Wasmtime, inside the same sandbox: it gets its frames, its own state
  directory, the Chumby's name, and HTTPS GETs to the hosts its owner
  approved (`network=host1,host2`) at public addresses, and nothing else.
  Only a neochumbyd built with the `wasm-companions` feature runs one
  (release builds, `cargo neochumbyd` and `tools/deploy` have it).
- **`server`:** a server half that runs once on the server for all its
  Chumbys, rather than once per Chumby as a companion does, such as a
  household web page. neochumbyd starts it when it starts, starts it again
  if it exits, and stops it when neochumbyd stops. It may bind the TCP
  ports `listen` names.

  | Key | Runs | How many |
  | --- | --- | --- |
  | `exec` | on the Chumby | the app, in front |
  | `service` | on the Chumby | in the background |
  | `companion` | on the server | one per Chumby |
  | `server` | on the server | one for all of them |

  Companions and server programs run sandboxed: see
  [SECURITY](SECURITY.md).
- **`exec`:** what runs, full-screen, when the app is opened. The session
  (`neochumby`) runs one app at a time in front. An app opens another by asking the session over its request pipe
  (`NEOCHUMBY_SESSION`) and exiting. When an app exits, the session goes
  back to the start app. [CONTRIBUTING](CONTRIBUTING.md#the-app-contract)
  has the details.
- **`start`:** the start app is the one a Chumby opens first and comes back
  to. It's named by `start=<app>` in this Chumby's settings from its
  neochumbyd (`neochumbyd set NAME start=<app>`, kept in
  the stick's `neochumby/state/neochumby.settings`), else in its `neochumby.conf` beside it, or else it's
  `home` (neochumby-home). Only an app with `start = true` may be the start
  app; the session refuses any other, logs why, and uses `home`. `neochumbyd list`
  names the start apps in the tree, and suggests one to a Chumby whose
  `start` isn't set; `neochumbyd set NAME start=<app>` warns about a name
  that isn't one. **Holding the top-left
  corner of the screen for 3 seconds** always opens `neochumby-home`,
  ending whatever was in front. That's the way out of an app gone wrong.
- **`service`:** runs in the background for as long as neochumby does, and
  is started again if it exits. It's told which app is in front. Any
  service may open an app over the start app, and the start app's service
  over any app, unless the one in front says **`interrupt = false`**, as
  Doom and Quake do.
- **`button`:** the session keeps the top button. A press returns to the start
  app, unless the app in front says `button = true`: then it has
  the button. Doom and Quake fire with it. With the start app in front, a
  press is a `button` line on its pipe.
- **`access`:** each app runs as a user of its own, jailed in a directory
  holding only its own files, a `/tmp` of its own and what `access` lists:
  `fb`, `touch`, `speaker`, `accel`, `system` (`/proc`, `/sys`, `/etc`)
  `network`, `terminal`, `logs` (the Chumby's logs, read-only) and `storage`
  (a small directory of its own, kept on the stick across boots). The session
  does for it what only root may: the mute and volume, the stream player
  (`play`, `/usr/bin/btplay` where there is one: the image has none yet), and, for the start app, the backlight (`brightness`). It
  never sees the Chumby's key. `root`, listed alone or with `fb`, runs it
  unjailed, as root: `home` does (in text mode), and
  neochumby-classic (with `fb`).
  [SECURITY](SECURITY.md) says what that protects, and what it doesn't.
- **Text mode, or `fb`:** an app's `exec` runs in text mode unless its
  `access` lists `fb`. In text mode it has no framebuffer: its stdin and
  stdout are a terminal (`TERM=linux`), which the session shows on the
  screen, and a touch arrives on its stdin as a mouse click
  ([CONTRIBUTING](CONTRIBUTING.md#text-mode) has the encoding). The kernel's console draws it. Games and anything that
  draws its own screen (Doom, Quake, LOGS) list `fb`. **`font`** picks the
  terminal's font, one of Spleen's six sizes (`spleen-5x8`, the default,
  64x30 characters; `spleen-6x12`, `-8x16`, `-12x24`, `-16x32`)
  or a PSF file in the app's directory; **`tap`** what a tap types while
  the program hasn't asked for the mouse (`q` quits top).
- **The owner's font and colours:** `neochumbyd set NAME font=spleen-8x16`
  sets every text-mode app's font, over its own, and `colors=` its colours:
  `github-dark`, the default, built in; any of the tree's `colors`, some
  900 schemes from [iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes)
  and a few themes' own repositories ("What's here"), each named in lower
  case with `-` between words (`dracula`, `catppuccin-mocha`,
  `rose-pine-moon`, `tokyonight-storm`, `nord`, `gruvbox-dark`,
  `solarized-dark-higher-contrast`, `xterm`; `grep -o '^[a-z0-9-]*'
  TREE/colors` lists them); a theme of your own in the tree's `themes/`
  directory, in Ghostty's format (`background = #rrggbb`,
  `foreground = ...`, `cursor-color = ...`, `palette = N=#rrggbb`), which
  most theme sites offer; or 18 hex colours joined by `:` (background,
  foreground, then colours 0 to 15). A name it can't find is logged, and
  GitHub Dark used. Each scheme is its 16 colours, background and foreground;
  the cursor is drawn in reverse video, as the Linux console draws it; a
  theme's other colours (the selection's, the cursor's text) aren't used. Without
  a neochumbyd, the same lines go in the stick's `neochumby/state/neochumby.conf`.
- **`wasm`:** in place of `exec`, a binary that `tools/tree` builds for
  `wasm32-wasip1` and compiles for the Chumby (`tools/wamrc`, when LLVM 18
  is there: `nix develop .#wasm`; else it runs interpreted, slowly). The
  session runs it with `neochumby-wasm`, the wasm runner, in the app's
  jail, in text mode or with `fb` as an `exec` is, and it reaches only
  what its `access` grants, through the runner's host API
  (`neochumby-guest`; [proposal 0003](docs/proposals/0003-wasm-apps.md)).
  WASM CLOCK is one.
- **`player`:** a Chumby's default player is the one a start app's MUSIC
  button opens (`NEOCHUMBY_PLAYER`), and its MUTE button pauses and resumes
  (the start app asks the session, which tells the player's service).
  It's named by `player=<app>` (`neochumbyd set NAME player=<app>`), else it's
  `somafm`, else the first app with `player = true`. `neochumbyd list`
  names the players in the tree.
- **`frontend`:** an app that shows another, through that app's service:
  an AirPlay screen of your own, say. The owner approves it with
  `neochumbyd set NAME frontend_airplay=<app>`; it is then opened wherever
  AirPlay would be, and AirPlay's service talks to it
  ([CONTRIBUTING](CONTRIBUTING.md#writing-an-airplay-frontend)).
- **`transition`:** a binary that animates the switch from one app to the
  next, when this Chumby's settings name its app (`neochumbyd set NAME
  transition=<app>`, else `transition=` in `/psp/neochumby.conf`; unset,
  apps switch with no animation). As one app leaves, the session holds its
  screen in the LCD controller's overlay (`/dev/fb1`) while the next app
  draws underneath, then runs the binary, jailed as its app, with both
  pictures on stdin (each 320x240 RGB565, little-endian), and shows each
  frame it writes to stdout in the overlay, for up to 3 seconds. It needs a
  kernel with the overlay, which Linux 7 doesn't have yet: until then, apps
  switch with no animation.
  [`apps/transitions`](apps/transitions) is one, with nine effects.

## Building

You'll need Rust with the `armv5te-unknown-linux-musleabi` target and
[zig](https://ziglang.org) on `PATH`. zig is the C compiler and linker, and
supplies musl (`tools/zigcc`).

Third-party source (Doom, Quake, fetch's fastfetch, htop, procps,
ncurses) is in git submodules, and fetch builds fastfetch with
[CMake](https://cmake.org) 3.21 or later. ncurses is built with its own
configure and make, which need only a shell, `make`, `awk` and a host C
compiler. `tools/tree`, `tools/stick` and `tools/deploy` check out
the submodules of the apps they build; for `cargo` on its own, check them
out first:

```sh
git submodule update --init   # or clone with --recursive
rustup target add armv5te-unknown-linux-musleabi x86_64-unknown-linux-musl wasm32-wasip1
cargo chumby          # the session and apps, for the Chumby
cargo neochumbyd      # the server and the apps' companions, for x86_64 Linux (with wasm companions)
```

The server half also builds for 64-bit ARM Linux, such as a Raspberry Pi
(`rustup target add aarch64-unknown-linux-musl`, then
`--target aarch64-unknown-linux-musl`), or natively for the machine
building it. `tools/deploy` builds for the server it installs on, asking
it over ssh, and `tools/tree --server-target TRIPLE` takes a Rust target
triple (CPU, vendor, OS and C library), or `host`.

## Running it

A Chumby runs neochumby from a tree of the session and its apps:

```
neochumby                  the session
neochumby-sync             the receiver, which keeps a paired stick's synced sets in step
colors, colors.LICENSE     the colour schemes text-mode apps can be drawn in (colors=), and their licences
themes/                    your own schemes, if any (colors=)
apps/home/                 neochumby-home, manifest
apps/doom/                 neochumby-doom, manifest, doom1.wad
apps/quake/                neochumby-quake, manifest, id1/pak0.pak, id1/autoexec.cfg
apps/airplay/              neochumby-airplay, neochumby-airplay-screen, neochumby-airplay-companion, manifest
apps/logs/                 neochumby-logs, neochumby-logs-companion, manifest
apps/fetch/                neochumby-fetch, manifest, fastfetch.jsonc, chumby.txt
apps/htop/                 neochumby-htop, manifest
apps/top/                  neochumby-top, manifest
apps/cmatrix/              neochumby-cmatrix, manifest (and so on for clock, bonsai, pipes, aquarium, genact)
lib/                       ld-musl-arm.so.1, libncursesw.so.6, libpanelw.so.6
sideload/                  what the hook needs on stock: neochumby-handoff; with --sideload DIR, sideload.ko, zImage, initramfs.cpio.gz
```

`tools/tree OUT [-d DATA] [--sideload DIR] [--pinned] [APP ...]` builds that tree. It always includes
the session and neochumby-home, plus the apps you name, from wherever
they are:

| APP | From |
| --- | --- |
| `doom` | `apps/doom` in this repository |
| `./path`, `/path` | a crate on this machine |
| `git+https://github.com/you/neochumby-x#REV` | a git repository with the crate at its root, at commit REV (unpinned, it warns and prints the commit to pin) |
| `crate:neochumby-x@1.0.0` | crates.io, checked against the index's SHA-256 (unversioned, it warns and takes the newest) |

An app from elsewhere runs on your Chumby, and its companion on your
server, so pin it: a full commit
id after `#` (a branch or tag can move), a version after `@`. With
`--pinned` (`tools/tree OUT --pinned APP ...`, and the same for
`tools/stick` and `tools/deploy`), an app that isn't pinned is refused
rather than warned about.

Each app's binaries are built for the Chumby, and its companion and server
program for the server, with the toolchain above. Its manifest is made from its
`Cargo.toml`, and its `files/` directory, if it has one, is copied as it
is. Game data is added from `DATA`, your own copies laid out the same way.
Every binary is static, but the apps that use curses (htop, top, cmatrix, clock, bonsai)
link musl and ncurses dynamically, from the tree's `lib/`
(`tools/linux-libs` builds them, with zig, and `pkg-config` finds them).
The jail puts those at `/lib` for such an app; the image itself stays
static. `--sideload DIR` adds what the hook needs to leave the stock firmware,
from `DIR/sideload.ko` (`tools/neochumby-kernel-sideloader/build DIR`) and
`DIR/sideload-zImage` and `sideload-rootfs.cpio.gz` (`dist/fetch DIR`, or
`dist/build DIR`).

### A stick

A stick is the only way a Chumby runs neochumby. Any FAT-formatted USB stick
works.

```sh
dist/fetch build/sideload                                  # or dist/build
tools/neochumby-kernel-sideloader/build build/sideload
tools/stick /Volumes/STICK --sideload build/sideload [-d DATA] [APP ...]
```

Plug it into a Chumby and power on: neochumby boots with the stick's apps.
**Nothing on the Chumby changes**: its stock boot runs the stick's `userhook2`,
which stops the stock software's UI and hands over to Linux 7. Without the
stick it boots as it always did. Made like this, a stick is not paired: it runs
the tree it was made with, and needs no server.

On the stick:

```
userhook2                   the boot hook
neochumby-off               if you put one there: the hook does nothing
neochumby/                  the tree it was made with: the session, home, apps
neochumby/sideload/         sideload.ko, zImage, initramfs.cpio.gz, neochumby-handoff
neochumby/state/            with --pair: neochumby.key, neochumby.conf; as it
                            runs: neochumby.settings, what apps keep
neochumby/a/ b/ current ... the synced sets, once a paired Chumby has them
```

### Pairing a stick: neochumbyd

neochumbyd keeps a copy of the app tree on each paired stick, and runs the
apps' server halves. Pairing happens when the stick is made, on the machine
making it; nothing is asked of the Chumby:

```sh
neochumbyd serve --state /var/lib/neochumbyd --shairport $(command -v shairport-sync)   # on the server
tools/stick /Volumes/STICK --sideload build/sideload --pair HOST --name kitchen [APP ...]
```

This makes the Chumby's key on your machine, writing the private key straight
to the stick (`neochumbyd keygen`: no temporary file, and it is never sent
anywhere), registers its public key and `kitchen` with the neochumbyd on HOST
over ssh (`neochumbyd add`), and writes the server's key and address to the
stick. `--server ADDR` is the address a Chumby reaches the server at (by
default HOST), `--state DIR` neochumbyd's state directory there, `--pair local`
the neochumbyd on this machine, and `--replace` puts this stick in the place of
another already registered as `kitchen`. A stick made again keeps its key. Put
the stick in the Chumby and power on: it connects at once, and any other key is
turned away.

**The pair of stick and Chumby is what's paired**, and the stick is the
identity: it moves with the stick. In another Chumby the same stick is the same
Chumby to the server. If a stick is lost, `neochumbyd forget kitchen` revokes it
at once; then make another with `--name kitchen`. `neochumbyd list` shows each
Chumby's name, key and settings. [SECURITY](SECURITY.md) says what a stolen
stick gives.

`tools/deploy HOST [APP ...]` installs the tree and neochumbyd in the server's
state directory (`/var/lib/neochumbyd`), leaving game data alone.

**A sandbox for apps' programs.** neochumbyd runs no companion or server
program without one, and it says at start which sandbox they get and
what that leaves open. Either of two is enough, and both may apply at
once; [SECURITY](SECURITY.md) weighs them:

- **This system's own:** Landlock on Linux (5.13 and later, where the
  kernel has it on), or Apple's sandbox on macOS, for neochumbyd running
  as its own user. Nothing to set.
- **A uid per app.** Started as root with `--app-uids FIRST-LAST`,
  neochumbyd becomes the user that owns its state directory, and runs each
  app's companions and server program under one uid of the app's own from
  that range, with no other groups, in `<state>/apps/<app>` (the app's,
  0750 in neochumbyd's group), and in Landlock too where the kernel has
  it. They then can't read neochumbyd's key, pairings or settings, change
  the tree, or reach another app's files. Pick a range no user or service
  has: systemd's `DynamicUser=` takes 61184-65519, so for example:

  ```sh
  sudo chown neochumbyd: /var/lib/neochumbyd && sudo chmod 2771 /var/lib/neochumbyd
  sudo neochumbyd serve --state /var/lib/neochumbyd --app-uids 61000-61099
  ```

  The state directory must be other-executable (programs pass through it
  to their directories and binaries); neochumbyd makes its own files in it
  unreadable to others. An existing `<state>/plugin-uids` (the flag's
  earlier name) is taken over as `<state>/app-uids`.

With neither, neochumbyd serves Chumbys but runs no companion or server
program, and says why. `--unsandboxed` runs them as neochumbyd's own user
instead, able to read its key and pairings and change the tree; it does
nothing where there is a sandbox, and is refused as root, where they would
run as root.

Under systemd, a unit along these lines keeps a root neochumbyd to the
four capabilities it keeps (not yet run by us on a systemd host):

```ini
[Service]
ExecStart=/var/lib/neochumbyd/neochumbyd serve --state /var/lib/neochumbyd --app-uids 61000-61099
CapabilityBoundingSet=CAP_SETUID CAP_SETGID CAP_CHOWN CAP_KILL
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=/var/lib/neochumbyd
ProtectHome=yes
PrivateTmp=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
Restart=on-failure
```

**The owner's token**, which `neochumbyd token` prints and `neochumbyd token new`
replaces, is what the catalog's commands pass to neochumbyd's admin protocol
(`crates/neochumbyd/src/admin.rs`), which only the command line speaks.

**AirPlay** (the airplay app): while a paired Chumby is connected, its
companion on neochumbyd runs an AirPlay receiver named after it ("Kitchen
Chumby"), and the Chumby plays the audio. Opened, or when a stream
begins, it shows what's playing: the title, artist and album, the cover,
the time and a progress bar, with previous, play/pause, next and the
volume, which go to the phone or Mac playing. It is a player, so a start
app's MUSIC and MUTE buttons open and pause it. The host provides
`shairport-sync` on `PATH`, avahi, and the base of eighty ports as
`NEOCHUMBY_AIRPLAY_PORT` (default 7350): RTSP from the base, UDP from
base + 10. Each Chumby keeps the same ports.

**Logs** (the logs app): the Chumby's own logs first, then, on a paired
Chumby, the host's, from its companion. ◀ and ▶ along the top go from
log to log; dragging scrolls, and at the end it follows. The host's are
whatever its owner lists in `<state>/apps/logs/sources` (or
`sources.NAME` for one Chumby), one a line, a file or a command:

```
syslog=/var/log/syslog
web=|tail -n 200 -f /srv/web/access.log
```

Without that file it serves /var/log/syslog or /var/log/messages, and the
journal where `journalctl` runs.

### The catalog

The catalog (GET APPS on the screen) is other people's apps, built as
WebAssembly and published as crates on crates.io with the keyword
`neochumby-app` ([proposal 0003](docs/proposals/0003-wasm-apps.md#the-catalog)
has what such a crate holds). crates.io is only where they are
published: nothing in the catalog is reviewed or endorsed by the Rust
project or crates.io. `neochumbyd serve` looks for them once a day, and
keeps what it found in `<state>/catalog.json`; Chumbys read only that.
`serve --no-catalog-refresh` turns the daily look off: then only
`neochumbyd catalog refresh` asks crates.io and GitHub.

```sh
neochumbyd catalog                 # what was found, best first
neochumbyd catalog refresh         # ask crates.io and GitHub now
neochumbyd install crate:neochumby-weather@1.2.0 --wamrc "$(tools/wamrc)"
neochumbyd uninstall weather
neochumbyd catalog log             # every install, update and removal
neochumbyd catalog auto-update off # or on, the default
```

`install` names a version, always. It fetches and checks the crate and
its modules, then shows the title, the publisher on crates.io, its
downloads, whether its build is verified, the access it asks for, its
companion's hosts, and whether it will run compiled or interpreted, and
asks y/N. Without a terminal it needs `--yes --access WORDS`, WORDS naming
exactly what it asks, its access words and its companion's hosts
(`--access fb,touch,api.open-meteo.com`). The app goes in the tree as
`apps/<name>/`. GITHUB_TOKEN, if set, raises GitHub's limit for
discovery.

Installing and removing are the owner's: the command line does them
through the admin protocol (`install`, `uninstall`), with the
owner's token from the state directory, as a page given the socket does
with the token its owner types.

**Updates.** Installing a newer version is an update. After each refresh,
an installed app whose listing has a newer version is updated by itself
if that version asks for nothing it didn't have: no new access word and
no new host. One that asks for more waits for the owner, who installs it
as above (or approves it from a Chumby's GET APPS). Every update is
checked as an install is, and logged in `<state>/catalog.log`.
`neochumbyd catalog auto-update off` stops updates by themselves; then
each one waits for the owner.

**"Verified build"** means one of GitHub's artifact attestations for the
app's `.wasm` verified on the server: a Sigstore bundle signed by a
certificate from Sigstore's Fulcio for a GitHub Actions run of a workflow
in the crate's own repository, logged in Sigstore's Rekor, for that very
file. It is a label, never a requirement. Anything else is **unverified**,
and `install`, the log and GET APPS say so, and why.

With a wamrc built by `tools/wamrc` (`--wamrc`, or NEOCHUMBY_WAMRC, on
`install` and `serve`), the app is compiled for the Chumby, sandboxed, and
compiled again when neochumbyd starts with a wamrc for another WAMR.
Without one it runs on the interpreter, which is slow. SECURITY has what
is checked.

**GET APPS** (the catalog app) shows the catalog on a paired Chumby, in
text mode, a page at a time: each app's title, publisher, a line of its
description, downloads, and **verified build** or **UNVERIFIED**, with why. An app's
details say what it asks for, plainly, its version and size, and whether
it would be slow (no wamrc on the server); access a catalog app never gets
is shown as refused. Its companion is neochumbyd itself
(`companion = "neochumbyd:catalog"`), which answers from the cache, and
looks up each app's details (by fetching and checking it, as an install
does) once per version: the best-ranked in the background, others when
first asked. A Chumby can have looked up only apps in the cache, from
crates.io and GitHub only, a few a minute. GET installs nothing on the Chumby's say alone. Its owner
allows the access on the Chumby; neochumbyd checks the app again and
waits, and the Chumby shows a code:

```sh
neochumbyd list                    # lists what Chumbys ask to install or remove
neochumbyd approve ABCD-EF23       # shows it, and lets it go ahead (the admin protocol's approve, with the owner's token)
neochumbyd refuse ABCD-EF23
```

A request lapses after ten minutes, or when the Chumby leaves GET APPS.
Once approved, the Chumby shows the install's progress and where home
lists the new app: its next sync round brings it to the stick, and its next
restart runs it. The installed catalog apps can be updated or
removed. An update that asks for nothing new goes ahead on the Chumby's
say, as serve's own updates do, unless automatic updates are off; one
that asks for more, and a removal, are approved the same way.

### Sync

A paired Chumby keeps a whole copy of its tree on its USB stick, in step with
neochumbyd, and boots from it, so it boots without the server. Unpaired, it runs
the tree the stick was made with. The stick needs room for that tree and two
whole copies of the server's: a round refuses a tree two copies of which won't
fit, saying so, before it touches anything.

Sync works like an A/B update. The stick's `neochumby/` holds two whole
sets, `a/` and `b/`, and `current` names the live one. On a paired Chumby,
the session runs `neochumby-sync` as a service of its own, restarted like the
apps' services but never part of the interruption rules (it opens no app). Once
a minute, or at once when asked (`neochumbyd sync NAME`), it:

1. sends neochumbyd the digest of its live set's manifest. If it matches
   the tree, nothing is written;
2. otherwise sends the standby set's SHA-256 manifest, and neochumbyd sends
   only the files that differ, whole;
3. writes them into the standby set only, each under a temporary name,
   hashed as it streams, and renamed into place. Files the tree no longer
   has are removed. Files are synced in batches, and each batch is listed
   in the set's journal only after it is on the stick;
4. hashes any file kept from a round that was cut short, then writes the
   set's completion marker (`a.complete`, carrying its manifest) last, and
   rewrites `current` to make it live from the next restart or boot. Both are synced
   to the stick before the round says so.

Files arrive only over the paired session, from the pinned neochumbyd key,
never SSH. Writes stay inside the stick's `neochumby/`, and happen only
while a stick is mounted at `/mnt/usb`. No `..`, absolute paths, symbolic
links or names FAT refuses (`con`, `aux.txt`), at most 64 MB a file and 512
MB a set, and only if the stick has room. A round has an hour in all. The
live set is never written. A rename on FAT can tear in a power cut, so the
boot hook doesn't trust `current` alone. It runs the set `current` names
only if that set's marker is whole and every file it lists is there at its
size (`neochumby check-set`, 30 seconds at most for both sets), else the
other set, else the tree the stick was made with. A power cut at any point
leaves a complete set to boot. The hook itself isn't synced: it is what
`tools/stick` put there, and a new one needs the stick made again.

A restart (`neochumbyd restart NAME`) runs the live set: the session finds it
as the hook does, and runs that set's `neochumby` in its own place. A new kernel
or initramfs in the set takes a reboot. While a Chumby is running the set the
next round would write (it has made another live, and not restarted),
`neochumbyd sync` exits 3 and says to restart first. Asked while a round is
running, it waits for the round after: the one running may have scanned an older
tree.

On the Chumby (with SSH enabled by its owner):

```sh
neochumby-sync status     # the sets, which is live
neochumby-sync rollback   # the other set is live from the next restart or boot; syncing holds
neochumby-sync resume     # sync again
```

`neochumbyd manifest` prints the tree a Chumby is sent, with anything a Chumby couldn't take. Files for one Chumby only
(a renderer's output, say) would be laid over the tree where neochumbyd's
`sync.rs` scans it; that isn't built yet.

**Developing on a paired Chumby** needs no shell on it. Its session
keeps a connection to neochumbyd, the developer link, over the same
pinned-key session as everything else:

```sh
neochumbyd log den [-f]          # its log: the session's, its apps' and services' output
neochumbyd sync den              # a sync round now, waiting for it to end
neochumbyd restart den           # end its apps and start its session again from its live set
neochumbyd shot den den.png      # its screen
tools/deploy HOST doom --restart den    # deploy, sync den, then restart it on the new set
```

Once its owner allows it (`neochumbyd set den dev=on`; off by default),
the same link also takes input, so the scenarios that drive the emulator
can drive a real Chumby:

```sh
neochumbyd tap den 160 120 [--hold MS]     # touch the screen there (pixels)
neochumbyd button den press|release|tap    # the top button
```

neochumbyd keeps the last 2000 lines of each Chumby's log in its state
directory (`logs/`), and limits what a Chumby may send. The commands work
through the state directory, so they need no port of their own. A restart ends
the app in front and the services, then runs the live set's `neochumby` in the
same process.

`neochumbyd mcp` serves the same as an [MCP](https://modelcontextprotocol.io)
server on stdin and stdout, so Claude Code (or any MCP client) can read a
Chumby's log, sync and restart it and look at its screen while working on an
app. Its tools are `chumbys` (the list and settings), `log`, `sync`, `restart`,
`screenshot` and `set`. Adding and forgetting a Chumby are left to a person. Run it on the
server, over SSH from wherever the client is:

```sh
claude mcp add neochumby -- ssh HOST neochumbyd mcp
```

The hook times out every step that could block, and, on the stock kernel,
returns to the stock boot if anything fails, stopping the stock software's UI
only once everything is checked. To skip it, put a file named `neochumby-off`
on the stick. neochumby never starts SSH; enable it
yourself from the Chumby's settings (Chumby info → π) when you need it.

## The emulator

All emulation happens through
[qemu-chumby](https://github.com/johnmerchant/qemu-chumby), the
submodule `emu/qemu/qemu-chumby` at a pinned commit: QEMU's
`chumby-classic` machine (patches against a pinned QEMU release, with the
screen, touchscreen, top button, accelerometer, sound, serial console,
flash and USB host working), `chumby-machine`, which runs it headless and
drives it, and `chumby-image`, which makes its NAND image from a flash
backup. Its README documents all three. neochumby's side is
`dev/emulate`, which boots a neochumby tree on the machine as a Chumby
would boot it from a stick, in one of two profiles:

| Profile | Boots | Where |
| --- | --- | --- |
| `dist` | neochumby's Linux 7 (`dist/build`, below) with our own userspace, the tree on a USB stick that its `/init` mounts and runs | Anywhere: CI runs it |
| `stock` | The Chumby's own firmware from your own flash backup, unmodified, with neochumby's tree on a USB stick: the stock boot runs the stick's hook, which hands over to Linux 7. For that handoff alone | Your machine only, never CI ([legal](docs/legal.md)) |

```sh
dev/emulate --profile dist start TREE           # TREE from tools/tree
dev/emulate --profile dist log -f               # the console, with the session's log
dev/emulate --profile dist tap 160 120          # touch at a pixel
dev/emulate --profile dist shot screen.png      # screenshot
dev/emulate --profile dist stop
dev/e2e TREE NEOCHUMBYD OUT                     # the scenarios, on NEOCHUMBY_EMU_PROFILE (dist)
```

`dev/emulate` gets QEMU and the dist image itself, the
first time and whenever what they're built from changes: CI's prebuilt
QEMU and images, verified (`emu/qemu/fetch`, `dist/fetch`), or, when there's none for this checkout (it has local
changes, say) or it can't be verified or run here, built from source
(`emu/qemu/build`, `dist/build`), with a line saying why.
`--build` (or `NEOCHUMBY_BUILD=1`) always builds them from source, and
`--no-fetch` (`NEOCHUMBY_NO_FETCH=1`) builds without trying to fetch;
`dev/emulate prepare` does it without booting anything. Without either,
it says so and the scenarios skip.

For `stock`, make your NAND image and kernel with qemu-chumby's
`chumby-image` (its README, "Images") and give them to `dev/emulate` as
`NEOCHUMBY_EMU_KERNEL`, `NEOCHUMBY_EMU_NAND` and `NEOCHUMBY_EMU_PSP` (your
factory-like settings directory), in the environment, in
`~/.config/neochumby/env.local` (every checkout's), or in an `.env.local`
beside this README that git ignores. `dev/emulate --help` has the rest:
the stick's layout and boot hook, the settings, and what each profile
can't do yet. The emulated Chumby never has a network (`-nic none`).
It reaches a neochumbyd over its serial line instead: qemu-chumby's
`--link` joins its second UART to a Unix socket where neochumbyd listens,
and its session carries the same TLS sessions over that line as over TCP:

```sh
neochumbyd serve --state /tmp/nd --tree TREE --listen-unix /tmp/nd.sock
mkdir made; pub=$(neochumbyd keygen made/neochumby.key)
{ neochumbyd add chumby $pub --address serial --state /tmp/nd; echo name=chumby; } > made/neochumby.conf
dev/emulate start --psp made --server /tmp/nd.sock TREE   # a stick as tools/stick --pair makes one
```

It stays paired from one start to the next. The line carries about 100
KB/s.

## The image

`dist/build` builds the Linux 7 image with Buildroot.
Buildroot needs Linux, so it runs in a
container, which can be Apple's `container`, `docker` or `podman`:
`dist/build` uses the first it finds, in that order, and
`NEOCHUMBY_DIST_RUNTIME` overrides it. The first build downloads and
compiles a toolchain and the kernel, which takes a long while; the work is
kept in a volume, `neochumby-dist`, so a second build rebuilds only what
changed (`<runtime> volume rm neochumby-dist` starts over). The volume
holds the kernel tree because macOS's disk can't: it has files whose names
differ only by case.

```sh
dist/build build/sideload-dist   # -> sideload-zImage, sideload-rootfs.cpio.gz
```

Or take CI's build of it, for exactly this checkout's inputs, from the
`dist-prebuilt` release, verified with `gh attestation verify` (so it
needs `gh`, logged in) before anything is unpacked:

```sh
dist/fetch build/sideload-dist   # the same files; --kernel mainline for that kernel
```

It refuses when the inputs have local changes or have no published build
(dist/README.md, "Prebuilt"); build it then. `dev/emulate` runs one or
the other itself, into `build/sideload-dist`, when it needs the image.

If the downloads fail with `unable to resolve host address`, the
container's nameserver isn't answering (seen with Apple's `container`):
give it one with `NEOCHUMBY_DIST_DNS=1.1.1.1 dist/build ...`.

The initramfs's `/init` mounts proc, sysfs and devtmpfs, logs to the kernel
log, joins the network the stock software was on, and keeps a shell on the
console. The kernel has no NAND driver, so nothing in the image can reach the
flash; `dist/build` fails if a NAND module gets into it.

It also runs a neochumby stick, as the stock boot does: it waits a few seconds
for one, mounts its first partition on `/mnt/usb`, and runs its `userhook2`
beside the console's shell. That is how `dev/emulate`'s dist profile boots a
tree (above).

The handoff from the stock firmware is in
`tools/neochumby-kernel-sideloader`; its
[README](tools/neochumby-kernel-sideloader/README.md) has the build and
hardware steps.

## Game data

None is included, and none may be redistributed with it. Use the shareware
episodes:

- Doom: `doom1.wad` (shareware v1.9) in `apps/doom/`. [Freedoom](https://freedoom.github.io) works too.
- Quake: `id1/pak0.pak` from the shareware `quake106` release, in `apps/quake/`.

## neochumby-quake

Quake's renderer is floating point throughout, and the ARM926 has no FPU.
Here's the work so far, measured with `timedemo demo1` at 320x240 on a
Chumby Classic:

| Step | fps |
| --- | --- |
| WinQuake's portable C paths, with the Chumby platform layer | 6.0 |
| libgcc's hand-written ARM soft float in place of the generic routines | 12.1 |
| Fixed-point span drawers in Rust, one fixed-point plane per surface | 15.6 |

`apps/quake/tools/bench` runs the timedemo on the device. Any app can be
profiled: run it with `NEOCHUMBY_PROF=file` and read the file with
`tools/profile file neochumby-quake`. The profile is read against the
unstripped binary, and soft-float time is charged to its callers.
