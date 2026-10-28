//! libgcc's hand-written ARM soft float (GCC 14.2, libgcc/config/arm), in
//! place of the generic routines Rust and zig link otherwise. The Chumby's
//! ARM926 has no FPU, so every float operation, in Rust or C, is a call to
//! one of these; they are about twice as fast as the generic ones.
//!
//! Linking the crate is all it takes: its archive comes ahead of
//! compiler-builtins on the link line, so the calls resolve here. neochumby
//! links it for every app (its default `softfloat` feature).

#![no_std]
