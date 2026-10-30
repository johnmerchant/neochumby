# neochumby-macros

**Experimental and unstable.** Any release may break anything, a 0.0.x
release included. This crate is built for
[neochumby](https://github.com/johnmerchant/neochumby), a new software
stack for the original Chumby Classic, and is published to start an SDK
for it, not as a general-purpose library.

neochumby's procedural macros, re-exported by the `neochumby` crate: use
them from there. `companion!()` names the compiling app's companion (its
server half), checked against the app's Cargo.toml as it compiles.

## Licence

MIT: see `LICENSE`.
