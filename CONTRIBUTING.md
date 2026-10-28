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
cargo chumby            # the session and the home screen, for the Chumby
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
button and speaker are fakes you drive from outside. It needs Linux, root,
FUSE and qemu-user-static; on a Mac, use a Linux VM. CI boots every pull
request's tree with it (`dev/emulate-smoke`).

```sh
sudo apt install qemu-user-static dosfstools    # Debian, Ubuntu
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
  a binary beside the session. `/psp` is kept from run to run in
  `target/emulate/psp`.

`dev/emulate --help` lists the fake devices and what each does. What it
can't catch is what the full-system emulator (`emu/`, #19: the whole
Chumby, stock firmware included) and real devices are for:

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
title = "WEATHER"                    # listed on the home screen; runs exec
exec = "neochumby-weather"
colour = "4a90d9"                    # optional
service = "neochumby-weather-sync"   # optional: runs in the background
companion = "neochumby-weather-companion"   # optional: its server half
neochumbyd = "optional"              # "required" | "optional" | absent
```

`tools/tree` checks the table and writes the tree's `manifest` from it
(`neochumby-manifest`). A `files/` directory beside `Cargo.toml`, optional,
is copied into the app's directory as it is (icons, configuration).

Binaries named by `exec`, `service` and `home` are built for the Chumby
(`armv5te-unknown-linux-musleabi`), and a `companion` for the server. Depend
on the `neochumby` crate for the screen (`fb`), touch, squeeze and
accelerometer (`input`), the manifest and settings (`apps`, `conf`) and
crash reports (`debug`). The screen is 320x240 RGB565, so prerender what
you can, and keep in mind that the CPU is a 266 MHz ARM926 with no FPU.

Things your app can rely on:

- **Starting:** the home screen runs it with `NEOCHUMBY_ROOT` (the tree) and
  `HOME=/tmp`. The root filesystem is read-only, and so is the tree.
- **The screen:** your app owns the screen until it exits. The home screen
  redraws when it comes back.
- **Getting out:** holding the top-left corner for 3 seconds ends your app,
  whatever it is doing, and goes back to `neochumby-home`.

**Companions.** An app with a server half ships it as a second binary,
named by `companion`. neochumbyd runs it for each paired Chumby that opens
the app:
- **Opening:** the Chumby side sends `Msg::Open { app: "<name>" }` over a
  `neochumby_proto::Session`.
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
