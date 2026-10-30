# neochumby-sync

**Experimental and unstable.** Any release may break anything, a 0.0.x
release included. This crate is built for
[neochumby](https://github.com/johnmerchant/neochumby), a new software
stack for the original Chumby Classic, and is published to start an SDK
for it, not as a general-purpose library.

`delivery=sync`: a Chumby keeps a whole copy of its tree on its USB
stick, in step with its neochumbyd, and boots from it. Both sides'
rounds, over neochumby-proto's session; they build and test on the host.

## Licence

MIT: see `LICENSE`.
