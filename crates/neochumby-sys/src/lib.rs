//! neochumby-sys: every raw system call neochumby makes, behind safe
//! functions. It is the only crate of ours with `unsafe` code; the GPL
//! games, which are FFI to C engines, are the only others allowed it.
//!
//! **Experimental and unstable**: any release may break anything. Built
//! for [neochumby](https://github.com/johnmerchant/neochumby), and
//! published to start an SDK for it.
//!
//! The workspace denies `unsafe_code`, and this crate alone allows it. Each
//! `unsafe` block here carries a `// SAFETY:` comment saying why it holds
//! (clippy's `undocumented_unsafe_blocks`, denied workspace-wide), and each
//! public function is safe to call with any arguments. Where std does the
//! job safely it is used instead, such as `Child::kill` for SIGKILL to one
//! process, unless it needs a Rust newer than Buildroot's (1.88), which
//! builds this crate for ncinit: `fs::try_lock` is flock(2) for that.
//!
//! The calls are the ones the Chumby's stock firmware supports: Linux
//! 2.6.16, static musl, an ARM926. Nothing here may use a newer system
//! call, start a thread, or wait on a futex, except [`lockdown`]: its newer
//! calls are asked of the kernel at run time, and do nothing on a kernel
//! without them. Parts that only the Chumby
//! needs are built only for Linux (and the crash reports only for ARM);
//! the rest also serves neochumbyd and app companions on Linux and macOS.
//!
//! One module is not system calls: [`speaker`], the files by which
//! processes hand the speaker over. It is here because this crate builds
//! and tests on the host, and the device library doesn't.
//!
//! # Why not rustix
//!
//! [rustix](https://docs.rs/rustix) offers safe wrappers for much of POSIX,
//! but not for what this crate needs:
//!
//! - Its default backend on Linux, linux_raw (ARMv5 included), makes raw
//!   system calls in place of libc's, and it documents Linux 3.2 as its
//!   minimum. The Chumby runs 2.6.16.
//! - Its `ioctl` and `mmap` are themselves `unsafe fn`, and `sigaction` is
//!   out of its scope (it needs libc's internals). The OSS ioctls, the
//!   framebuffer mapping and the signal handlers would still need `unsafe`
//!   blocks of our own.
//! - A handful of libc calls in one audited crate is a smaller dependency
//!   than a general-purpose system interface.

#![allow(unsafe_code)]

pub mod chan;
pub mod elf;
pub mod fd;
pub mod fs;
pub mod net;
pub mod privilege;
pub mod process;
pub mod pty;
pub mod sandbox;
pub mod serial;
pub mod signal;
pub mod speaker;
pub mod time;
pub mod user;

#[cfg(target_os = "linux")]
pub mod alsa;
#[cfg(all(target_os = "linux", feature = "boot"))]
pub mod boot;
#[cfg(feature = "compat26")]
pub mod compat26;
#[cfg(target_os = "linux")]
pub mod evdev;
#[cfg(target_os = "linux")]
pub mod jail;
#[cfg(target_os = "linux")]
pub mod klog;
#[cfg(target_os = "linux")]
pub mod lockdown;
#[cfg(target_os = "linux")]
pub mod mm;
#[cfg(target_os = "linux")]
pub mod oss;

#[cfg(all(target_os = "linux", target_arch = "arm"))]
pub mod crash;
