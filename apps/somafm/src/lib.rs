//! neochumby-somafm: SomaFM's stations on the Chumby, with the 2008
//! Chumby's Pandora-style Music screen (from neochumby-classic), as a music
//! app of its own (`music = true`).
//!
//! - **The screen** (`neochumby-somafm`, the exec) shows pictures drawn at
//!   build time by render/render.py into files/: a background and the
//!   pieces that change, light and dark (screen.rs). It saves what is
//!   tapped (app.rs) to the app's storage (store.rs), and DONE exits.
//! - **The player** (`neochumby-somafm-service`) runs for the whole
//!   session. It reads the storage and plays the chosen station through
//!   the session, giving the speaker up to games and taking it back after
//!   (player.rs); MUTE on the start app pauses and resumes it.
//! - **The stations** are compiled in (stations.rs, from stations.txt):
//!   SomaFM's plain-HTTP MP3 streams. The tiles are our own drawings, not
//!   SomaFM's artwork.

pub mod app;
pub mod player;
pub mod screen;
pub mod stations;
pub mod store;

#[cfg(target_arch = "arm")]
pub mod device;
