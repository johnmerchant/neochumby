# neochumby-softfloat

**Experimental and unstable.** Any release may break anything, a 0.0.x
release included. This crate is built for
[neochumby](https://github.com/johnmerchant/neochumby), a new software
stack for the original Chumby Classic, and is published to start an SDK
for it, not as a general-purpose library.

libgcc's hand-written ARM soft-float routines (GCC 14.2,
`libgcc/config/arm`), for the Chumby Classic's FPU-less ARM926: about
twice as fast as the generic ones Rust links otherwise. Linking the crate
is all it takes. On any target but 32-bit ARM it is empty.

## Building

Building it for ARM takes a C compiler for the target (it assembles
libgcc's `.S` files with the `cc` crate).

## Licence

GPL-3.0-or-later WITH GCC-exception-3.1: libgcc's licence, with the GCC
Runtime Library Exception, which puts no obligations on the programs the
routines are linked into. The texts are in `libgcc/COPYING3` and
`libgcc/COPYING.RUNTIME`.
