# neochumby-proto

**Experimental and unstable.** Any release may break anything, a 0.0.x
release included. This crate is built for
[neochumby](https://github.com/johnmerchant/neochumby), a new software
stack for the original Chumby Classic, and is published to start an SDK
for it, not as a general-purpose library.

How a Chumby and its neochumbyd talk: TLS 1.3 with raw public keys, over
TCP or over a serial line shared by several sessions (`mux`), pairing by a code both sides show, and the messages a session
carries (an app's frames to and from its companion, and the `Sync*`
messages that keep a stick in step). Builds for the Chumby and for servers.

## Licence

MIT: see `LICENSE`.
