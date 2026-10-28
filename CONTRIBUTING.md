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
crates), and `cargo test -p neochumby-proto -p neochumby-sync -p
neochumbyd`.

Code that runs on the Chumby but needs no Chumby goes in a crate that also
builds on the host, so CI can test it there. `neochumby-sync` is one: its
tests run whole sync rounds over a real session on loopback, with a
temporary directory standing in for the stick.

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

## Files for one Chumby

With `delivery=sync`, each Chumby's tree is the shared tree plus its own
files from neochumbyd's `overlay/<name>/` (see the README). A program that
renders for one Chumby writes there:

- **Atomically:** each file goes in under a name starting with `.`, then is
  renamed into place. Names starting with `.` are never sent.
- **Stable names:** the same picture keeps the same name on every run. A
  new name is a new file to send and write to the stick.
- **Only when changed:** leave an identical file alone. Per-run
  timestamps baked into large files make them change every run, so they
  are sent every time.
- **Names a Chumby takes:** A-Z a-z 0-9 `.` `_` `+` `-`, not starting or
  ending with `.`, and unique ignoring case (the stick is FAT).
  `neochumbyd manifest NAME` reports anything left out.

## Pull requests

Keep a pull request to one change, explain why in its description, and make
sure CI is green. If you're changing how a Chumby boots
(`boot/userhook2`), test on a real one or say that you couldn't: a hook
that hangs stops the Chumby's boot. The hook runs under BusyBox 1.6 `sh`,
which has `sed` but no `sha256sum`, `printf`, `od` or `head -c`. Anything
it can't do goes in the session binary as a subcommand, like `neochumby
check-set`.
