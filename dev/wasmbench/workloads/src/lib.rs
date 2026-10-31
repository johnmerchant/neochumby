//! The wasm benchmark's workloads, as Rust built twice: natively for the
//! Chumby (static musl, ARM926EJ-S, soft float) and to `wasm32-wasip1`. The
//! kernels are here, safe and without I/O; the binaries in `src/bin` time
//! them and print what they found, one line per part, as the harness reads
//! it (`NAME ... ms=X ...`).
//!
//!   fb        the framebuffer kernel: per-pixel loops and copies
//!   float     the soft-float kernel: n-body (f64) and a matrix (f32)
//!   startup   a module of an app's size whose main does nothing
//!   empty     an empty main, for the runtime's own start-up cost
//!
//! Every output but the timings is the same natively and under every
//! runtime, or something is wrong: tests/checksums.rs holds them.

pub mod fb;
pub mod float;

/// The functions `startup` keeps alive, written by build.rs.
pub mod startup {
    include!(concat!(env!("OUT_DIR"), "/startup.rs"));
}

/// The number in argument `i`, or `default` if there isn't one.
pub fn arg(i: usize, default: i32) -> i32 {
    std::env::args().nth(i).and_then(|a| a.parse().ok()).unwrap_or(default)
}

/// Milliseconds since `t0`, as the C workloads' CLOCK_MONOTONIC did.
pub fn ms(t0: std::time::Instant) -> f64 {
    t0.elapsed().as_secs_f64() * 1e3
}
