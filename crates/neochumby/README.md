# neochumby

**Experimental and unstable.** Any release may break anything, a 0.0.x
release included. This crate is built for
[neochumby](https://github.com/johnmerchant/neochumby), a new software
stack for the original Chumby Classic, and is published to start an SDK
for it, not as a general-purpose library.

The Chumby Classic's session and device library: its screen, inputs and
speaker, for the launcher, the apps and the services. Every neochumby app
written in Rust depends on it. It builds only for the Chumby
(`armv5te-unknown-linux-musleabi`): Linux 7, static musl, ARM926.

## Building

Building for the Chumby (`armv5te-unknown-linux-musleabi`) takes a C
compiler and linker for static musl on the ARM926: neochumby uses zig
(`zig cc`, through the repository's `tools/zigcc` and
`.cargo/config.toml`). See the repository's CONTRIBUTING.md.

## Licence

MIT: see `LICENSE`.
