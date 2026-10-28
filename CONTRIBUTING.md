# Contributing to neochumby

Thanks for helping bring the original Chumby back. A few things make
changes easy to take.

## Ground rules

- **Rust** for everything we write. Third-party engines in C (Doom, Quake)
  are vendored with as few changes as possible, each marked with a comment
  naming the app (`// neochumby-quake: ...`), and linked from a Rust crate.
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
games (Doom, Quake) are allowed it as FFI to their C engines. We use libc
rather than rustix: rustix calls the kernel directly and needs Linux 3.2,
the Chumby runs 2.6.16, and its `ioctl` and `mmap` are `unsafe` too (see
neochumby-sys's documentation).

## Building and checking

```sh
rustup target add armv5te-unknown-linux-musleabi x86_64-unknown-linux-musl
# zig 0.17 on PATH (the C compiler, linker and musl): https://ziglang.org
cargo chumby            # the session and neochumby-home, for the Chumby
cargo neochumbyd        # the server
tools/tree /tmp/tree doom quake airplay   # a whole tree, with apps
```

CI runs the same checks on every pull request. To run them yourself:
`cargo fmt --all --check`, `cargo clippy -- -D warnings` (on the host
crates and, with `--target armv5te-unknown-linux-musleabi`, on the Chumby's
crates), and `cargo test -p neochumby-proto -p neochumbyd`.

## Testing without a Chumby: dev/emulate

`dev/emulate` runs a tree on a Linux PC as if it were a Chumby: the same
static ARM binaries, unchanged, under qemu-arm user mode, chrooted in a fake
root with the Chumby's paths. The screen is a file, and the touchscreen, top
button and speaker are fakes you drive from outside. CI boots every pull
request's tree with it (`dev/emulate-smoke`).

It needs Linux, root, Python 3.9+, FUSE, util-linux's `mount` with loop
devices, qemu-user's binfmt handler (static), and dosfstools for stick
images. On a Mac, run it in a Linux VM.

```sh
sudo apt install qemu-user-binfmt dosfstools    # Debian, Ubuntu (older: qemu-user-static)
tools/tree /tmp/tree doom
sudo dev/emulate start /tmp/tree     # the tree at /mnt/neochumby, as over NFS
dev/emulate shot home.png
dev/emulate tap 160 102              # the second row of the home screen
dev/emulate tap 5 5 --hold 3500      # the corner hold
dev/emulate button press             # the top button
dev/emulate log -f                   # what the session and its apps print
dev/emulate stop
```

- **A stick:** `sudo dev/emulate mkstick /tmp/stick.img /tmp/tree` makes a
  FAT image laid out as `tools/stick` does, and `start --stick
  /tmp/stick.img` mounts it at `/mnt/usb` and boots from it. Given a tree
  as well, the session runs from the tree and the stick starts as it is.
- **A neochumbyd:** the emulator shares the PC's network, so a neochumbyd
  running on the PC can pair with it (`start --server 127.0.0.1 ...`).
- **Inside:** `sudo dev/emulate exec /mnt/neochumby/neochumby sensors` runs
  a binary beside the session.
- **Its directory** is `/var/tmp/neochumby-emulate-UID`, root's, with the
  screen, the log and `/psp` (kept from run to run). `--state DIR` picks
  another; sudo drops `NEOCHUMBY_EMU`, so pass it with `sudo
  --preserve-env=NEOCHUMBY_EMU`.
- **Pictures and the like:** `start --bind DIR:/tmp/classic` puts a host
  directory in the fake root, read-only. `dev/emulate compare RAW ...` says
  which 320x240 RGB565 picture the screen is most like.

The session runs as root, as on the Chumby, but without the capabilities
that would let it out of the fake root or into the PC's settings: no
mounts, chroot, modules, raw devices or network setup, and `/proc/sys` is
read-only.

### Scenarios

**neochumby-classic** (`dev/emulate-classic TREE PICTURES OUT`; CI runs it
once classic is a start app, #26).
classic is the start app (`start=classic` in `/psp/neochumby.conf`), with
pictures from neochumby-classic's offline smoke render (`python3
tests/smoke.py --out PICTURES` in that repository): made-up data and
placeholder artwork. The script checks that:
- classic draws a slideshow page;
- the top button toggles Home and back without restarting classic;
- a tap zone's `app doom` opens Doom through the session's pipe, and
  classic comes back when Doom exits;
- classic's service starts once, hears that Doom is in front, and keeps
  running;
- CHUMBY on Home ends the session with exit 0.

No renderer lists apps yet, so the script adds one `app doom` tap zone to
its copy of Settings.

**Sync** (manual, not in CI). It needs a paired Chumby and neochumbyd's
side of a round, so it waits until `delivery=sync` (#10) and the session
PRs are both in `main`. Then, as root on one Linux PC (not yet run, so
expect to adjust it):

```sh
tools/tree /tmp/tree doom && cargo neochumbyd
B=target/x86_64-unknown-linux-musl/release/neochumbyd
$B serve --state /tmp/nd --tree /tmp/tree --api 127.0.0.1:7340 --nfs 127.0.0.1:2049 &
dev/emulate mkstick /tmp/stick.img            # an empty FAT stick
dev/emulate start --stick /tmp/stick.img --server 127.0.0.1 /tmp/tree
dev/emulate tap 80 220                        # PAIR, on neochumby-home
$B pairing --state /tmp/nd                    # the same code as the screen
$B approve CODE --state /tmp/nd
$B set chumby delivery=sync --state /tmp/nd   # the emulator's name: chumby
dev/emulate stop
dev/emulate start --stick /tmp/stick.img /tmp/tree   # paired now: settings fetched
dev/emulate wait log 'neochumby-sync' 120      # the receiver's first round
```

Then check, from the PC, with the stick image mounted read-only:
- `neochumby/a/` (or `b/`) holds the tree;
- its `.complete` marker is whole, and `current` names it;
- `dev/emulate exec /mnt/usb/neochumby/neochumby check-set
  /mnt/usb/neochumby/a` agrees.

For the next boot, the boot hook won't run here: it needs BusyBox. Start
the set `current` names in its place, with `--run`:

```sh
dev/emulate start --stick /tmp/stick.img --run /mnt/usb/neochumby/a/neochumby
```

Then change a file in `/tmp/tree` and wait for a round to write the other
set and flip `current`. A power cut mid-round can be approximated by
`dev/emulate stop` (SIGKILL) during a large file. That shows the journal
and resume logic, not FAT's behaviour.

`dev/emulate --help` lists the fake devices and what each does. What it
can't catch is what full-system emulation (the whole Chumby, its stock
firmware included, still to come) and real devices are for:

- **The 2.6.16 kernel.** System calls go to the PC's kernel. Futex flags
  that kernel lacks, `nanosleep` failing with `EINVAL` on restart, and newer
  calls and flags all work here and fail there.
- **Real FAT and USB.** The stick is the PC's vfat driver on a file in
  memory, not the old driver on a slow stick. Its directory fsync, its
  speed, a stick pulled out and a power cut aren't reproduced.
- **Real input.** Taps are clean: no echo after a release, no brief releases
  during a hold, and the button never bounces.
- **Speed.** qemu on a PC isn't a 266 MHz ARM926, so frame rates and CPU
  load mean nothing here. Only the speaker is paced, to the rate it's set to.
- **The rest of the Chumby.** The boot hook doesn't run: the session is
  started directly. There's no BusyBox, `btplay` or `chumby_set_volume`, no
  accelerometer, and no overlay or backlight to see.

## Writing an app

An app is a crate named `neochumby-<name>`. It lives in this repository
under `apps/<name>/`, or in its own repository with the crate at the root,
where it is added to a tree from its git URL or from crates.io
(`tools/tree OUT git+https://github.com/you/neochumby-weather#REV`). Its
`Cargo.toml` says what it is:

```toml
[package]
name = "neochumby-weather"
# ...

[dependencies]
neochumby = { git = "https://github.com/johnmerchant/neochumby" }

[package.metadata.neochumby]
title = "WEATHER"                    # listed in app lists
exec = "neochumby-weather"           # runs in front when the app is opened
colour = "4a90d9"                    # optional
service = "neochumby-weather-sync"   # optional: runs in the background
companion = "neochumby-weather-companion"   # optional: its server half
neochumbyd = "optional"              # "required" | "optional" | absent
start = true                         # optional: it may be the start app
interrupt = false                    # optional: no service replaces it (games)
```

`tools/tree` checks the table and writes the tree's `manifest` from it
(`neochumby-manifest`). An app with a `companion` must also say
`neochumbyd = "required"` or `"optional"`: it is never inferred. A
`files/` directory beside `Cargo.toml`, optional, is copied into the app's
directory as it is (icons, configuration).

Binaries named by `exec` and `service` are built for the Chumby
(`armv5te-unknown-linux-musleabi`), and a `companion` for the server. Depend
on the `neochumby` crate for the screen (`fb`), touch, squeeze and
accelerometer (`input`), the manifest and settings (`apps`, `conf`) and
crash reports (`debug`), and the request pipe (`session`). The screen is
320x240 RGB565, so prerender what you can, and keep in mind that the CPU is
a 266 MHz ARM926 with no FPU.

### The app contract

The session (`neochumby`) runs one app at a time in front, full-screen.
Every app is opened the same way, whoever asked for it.

**When your app is opened**, the session runs its `exec`:
- in the app's directory in the tree, in a process group of its own;
- with `NEOCHUMBY_ROOT` (the tree) and `HOME=/tmp`. The root filesystem is
  read-only, and so is the tree;
- with `NEOCHUMBY_SESSION=<fd>`, its request pipe to the session (below);
- with `NEOCHUMBY_REASON` when the session has a reason to give. Ignore
  values you don't know;
- owning the screen until it exits. What's on the screen when it starts is
  whatever was there: draw all of it;
- sharing the touchscreen: every reader gets every touch. The session
  watches the top-left corner for the escape hatch.

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

**Start apps.** The start app is the one a Chumby opens at boot and comes
back to. The owner picks it (`start=<app>`), and it defaults to
`neochumby-home`. Declaring `start = true` lets your app be picked, and
promises:
- it **opens other apps** through the request pipe;
- it offers **CHUMBY MODE**, and the owner's settings, or at least points
  to where they are;
- it **keeps running**. If it exits without asking for anything five times
  within a minute, the session makes `neochumby-home` the start app
  instead.

Without `start = true`, the session refuses the app as the start app and
uses `neochumby-home`, so a Chumby can't be left stuck in a game.

### Services

What has to keep working while another app is in front (alarms, a night
schedule, music) belongs in the app's `service`. The session runs each
app's service, where what it needs of a neochumbyd is here, for as long as
neochumby runs:
- with the same environment as an app (`NEOCHUMBY_ROOT`, `HOME=/tmp`,
  `NEOCHUMBY_SESSION`), in a process group of its own;
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
  `neochumby::companion::open(neochumby::companion!(), &paired, &identity)`,
  which connects to the paired neochumbyd and returns the
  `neochumby_proto::Session` to its companion. `companion!()` reads the
  app's `Cargo.toml` as it compiles: an app without a `companion`, or
  without `neochumbyd`, does not compile.
- **Frames:** after that, frames go both ways as `Msg::Data`. On the
  companion's side they arrive on stdin and leave on stdout, a u32
  big-endian length then the bytes (`neochumby_proto::frame`).
- **Environment:** the companion gets `NEOCHUMBY_CHUMBY` (the Chumby's name)
  and `NEOCHUMBY_STATE` (a directory of its own). It should exit when its
  stdin closes.

`apps/airplay` is a worked example of both halves.

## Pull requests

Keep a pull request to one change, explain why in its description, and make
sure CI is green. If you're changing how a Chumby boots
(`boot/userhook2`), test on a real one or say that you couldn't: a hook
that hangs stops the Chumby's boot.
