# neochumby

New software for the original Chumby (the Chumby Classic, 2008): a home
screen and apps in Rust, booted from a stick or over the network, with the
Chumby's own firmware left as it is.

Nothing is flashed. The stock boot script runs a hook from a stick or from
the Chumby's writable settings partition, and the hook runs neochumby from
the stick or from an NFS export. **CHUMBY MODE** on the home screen hands
the Chumby back to its stock software, and a reboot brings neochumby back.
Every binary is paged in as it runs, so apps cost the Chumby nothing until
opened.

## The hardware

Freescale i.MX21: an ARM926EJ-S at about 266 MHz, with no FPU and 64 MB of
RAM. The screen is 320x240 RGB565 (`/dev/fb0`), with a resistive touchscreen
(`/dev/input/event0`), a squeeze switch in the case (`/dev/switch`) and an
accelerometer (`/dev/accel`). It runs Linux 2.6.16 with glibc 2.8. Our
binaries are static (musl), so they don't depend on its userland.

## What's here

| Path | What | Licence |
| --- | --- | --- |
| `crates/neochumby` | The session (`neochumby`): runs the home screen and the apps' services. And the library every app uses: screen, touch, squeeze, accelerometer, the apps' manifests, crash reports and a profiler | MIT |
| `crates/neochumby-softfloat` | libgcc's hand-written ARM soft float, which the library links into every app: float maths about twice as fast as the generic routines | GPL-3.0 with the GCC Runtime Library Exception |
| `crates/neochumby-proto` | How a Chumby and neochumbyd talk: TLS 1.3 with pinned keys, or Noise as a build option | MIT |
| `crates/neochumbyd` | The server a Chumby pairs with: its apps over NFS, pairing, and the apps' server halves | MIT |
| `apps/home` | neochumby-home, the home screen: the installed apps, pairing, the time, CHUMBY MODE | MIT |
| `apps/airplay` | AirPlay: a receiver per Chumby on neochumbyd, played on the Chumby's speaker | MIT |
| `apps/doom` | Doom: [doomgeneric](https://github.com/ozkl/doomgeneric) (e257a095, vendored) with a Rust platform layer | GPL-2.0 |
| `apps/quake` | neochumby-quake: id's WinQuake software renderer, made fast on the ARM926 (below) | GPL-2.0-or-later |
| `boot/userhook2` | The boot hook | MIT |
| `tools/` | The cross-compiling wrappers and `deploy` | MIT |
| `dev/` | Device-side test helpers: framebuffer capture to PNG, touch injection, mount test | MIT |

Everything we write is MIT, except in `apps/doom` and `apps/quake`. Doom
and Quake link GPL engines, so each of those crates, our Rust in it
included, is GPL and lives in its own directory with its upstream licence
(`apps/quake/src/span.rs` is a port of the engine's span drawer, so it
could be nothing else). Nothing links an app: each runs as a separate
program. The
soft-float routines' runtime exception puts no conditions on the programs
they're linked into.

## Apps

Everything you see is an app, home screen included. An app is one crate,
`neochumby-<name>`, which installs as a directory `apps/<name>/` in the
tree, holding its binaries and a `manifest`. The manifest comes from the
crate's `Cargo.toml`:

```toml
[package.metadata.neochumby]
title = "QUAKE"                      # listed on the home screen; runs exec
exec = "neochumby-quake"             # binaries, by name
colour = "8c5a28"                    # optional
service = "neochumby-airplay"        # runs in the background
companion = "neochumby-airplay-companion"   # its server half, run by neochumbyd
daemon = "neochumby-web"             # runs once per server, by neochumbyd
neochumbyd = "required"              # or "optional"; absent: never needed
home = "neochumby-home"              # this app can be the home screen
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
  app's `Cargo.toml` declares no companion.
- **`daemon`:** a server half that runs once per server rather than once
  per Chumby, such as a household web page. neochumbyd starts it when it
  starts, starts it again if it exits, and stops it when neochumbyd stops.
  It reaches neochumbyd through the admin socket (below).
- **`home`:** the session (`neochumby`) runs the app named by `start=<app>`
  in this Chumby's settings from its neochumbyd (`neochumbyd set NAME
  start=<app>`, kept in `/psp/neochumby.settings`), else in
  `/psp/neochumby.conf`, or else `neochumby-home`. **Holding the
  top-left corner of the screen for 3 seconds** always brings back
  `neochumby-home`, ending whatever was running. That's the way out of a
  home screen or app gone wrong.

## Building

You'll need Rust with the `armv5te-unknown-linux-musleabi` target and
[zig](https://ziglang.org) on `PATH`. zig is the C compiler and linker, and
supplies musl (`tools/zigcc`).

```sh
rustup target add armv5te-unknown-linux-musleabi x86_64-unknown-linux-musl
cargo chumby          # the session and apps, for the Chumby
cargo neochumbyd      # the server and the apps' companions, for x86_64 Linux
```

A Chumby and its neochumbyd talk TLS 1.3. To use Noise instead, build both
with `--features neochumby-proto/noise`. Nothing is negotiated, so the two
ends must be built the same way.

## Running it

A Chumby runs neochumby from a tree of the session and its apps:

```
neochumby                  the session
userhook2                  the boot hook, which pairing installs
apps/home/                 neochumby-home, manifest
apps/doom/                 neochumby-doom, manifest, doom1.wad
apps/quake/                neochumby-quake, manifest, id1/pak0.pak, id1/autoexec.cfg
apps/airplay/              neochumby-airplay, neochumby-airplay-companion, manifest
```

`tools/tree OUT [-d DATA] [APP ...]` builds that tree. It always includes
the session and the home screen, plus the apps you name, from wherever
they are:

| APP | From |
| --- | --- |
| `doom` | `apps/doom` in this repository |
| `./path`, `/path` | a crate on this machine |
| `git+https://github.com/you/neochumby-x#REV` | a git repository with the crate at its root, at commit REV (unpinned, it prints the commit to pin) |
| `crate:neochumby-x@1.0.0` | crates.io (unversioned, the newest) |

Each app's binaries are built for the Chumby, and its companion and daemon
for the server, with the toolchain above. Its manifest is made from its
`Cargo.toml`, and its `files/` directory, if it has one, is copied as it
is. Game data is added from `DATA`, your own copies laid out the same way.
The tree can come from a stick or from a server.

### From a stick

```sh
tools/stick /Volumes/STICK [-d DATA] [APP ...]
```

Any FAT-formatted USB stick works. Plug it into a Chumby and power on:
neochumby boots with the stick's apps. **Nothing on the Chumby changes**:
its stock boot runs the stick's `userhook2`. Without the stick it boots
as it always did.

### From a server: neochumbyd

neochumbyd serves the tree read-only over NFSv3, with NFS and MOUNT on
one TCP port. It also pairs Chumbys and runs their services:

```sh
neochumbyd serve --state /var/lib/neochumbyd --shairport $(command -v shairport-sync)
```

`tools/deploy HOST [APP ...]` installs the tree and neochumbyd in the server's
state directory (`/var/lib/neochumbyd`), leaving game data alone.
BusyBox's `mount` asks the port mapper on port 111 even when given the
ports, so the server should refuse port 111 rather than drop it. Otherwise
the mount hangs.

**Pairing** makes a Chumby boot from neochumbyd with no stick:

1. Put a `neochumby.conf` holding `server=<neochumbyd's address>` on a
   neochumby stick, and boot the Chumby from it.
2. The home screen shows **PAIR**. Tap it: the Chumby shows a 6-digit code.
3. On the server, `neochumbyd pairing` lists the request with the same
   code. If it matches, run `neochumbyd approve <code>`.
4. The Chumby writes its key, the server's key and the boot hook to
   `/psp`. Remove the stick and restart.

The code comes from the connection itself, so it matches on both screens
only if nothing sits between them. After pairing, each end checks the
other's key on every connection.

**The admin socket** lets an app's daemon, such as a household web page,
do what the commands above do: list the Chumbys paired and asking, approve
or refuse a request, forget a Chumby, and set its settings
(`neochumbyd set`). It's `admin.sock` in the state directory, answering
only neochumbyd's own user and root. Approving or forgetting a Chumby
through it needs the **owner's token**, which `neochumbyd token` prints:
give it to the web page when it asks. `neochumbyd token new` replaces it.
The protocol is described in `crates/neochumbyd/src/admin.rs`.

**AirPlay** (the airplay app): while a paired Chumby is connected, its
companion on neochumbyd runs an AirPlay receiver named after it ("Kitchen
Chumby"), and the Chumby plays the audio. The host provides
`shairport-sync` on `PATH`, avahi, and the base of eighty ports as
`NEOCHUMBY_AIRPLAY_PORT` (default 7350): RTSP from the base, UDP from
base + 10. Each Chumby keeps the same ports.

When a stick and a server are both available, the stick wins. The hook
times out every step that could block, and returns to the stock boot if
anything fails. To skip it, create `/psp/neochumby-off`, or put a file
named `neochumby-off` on a stick. neochumby never starts SSH; enable it
yourself from the Chumby's settings (Chumby info → π) when you need it.

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
