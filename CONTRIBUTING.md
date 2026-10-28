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
cargo chumby            # the session and neochumby-home, for the Chumby
cargo neochumbyd        # the server
tools/tree /tmp/tree doom quake airplay   # a whole tree, with apps
```

CI runs the same checks on every pull request. To run them yourself:
`cargo fmt --all --check`, `cargo clippy -- -D warnings` (on the host
crates and, with `--target armv5te-unknown-linux-musleabi`, on the Chumby's
crates), and `cargo test -p neochumby-proto -p neochumbyd`.

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
```

`tools/tree` checks the table and writes the tree's `manifest` from it
(`neochumby-manifest`). A `files/` directory beside `Cargo.toml`, optional,
is copied into the app's directory as it is (icons, configuration).

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
