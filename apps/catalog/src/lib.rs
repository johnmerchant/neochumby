//! GET APPS, the catalog on the Chumby: other people's WebAssembly apps,
//! found by the paired neochumbyd and installed only with its owner's
//! approval, on the Chumby and then on the server
//! (docs/proposals/0003-wasm-apps.md).
//!
//! A text-mode app: it draws in the terminal the session gives it, and
//! touches come as mouse reports ([`input`]). It talks only to neochumbyd,
//! whose own catalog is its companion (neochumby_proto::catalog), never to
//! crates.io or GitHub. [`ui`] has the screens; [`screen`] draws them.

pub mod input;
pub mod screen;
pub mod ui;

#[cfg(target_arch = "arm")]
pub mod device;
