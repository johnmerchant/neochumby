//! neochumby-wx: the 2008 Chumby's look, as a neochumby app; run it as the
//! start app with `start=wx` in /psp/neochumby.conf. A feeder on a server renders every pixel (the
//! weather, news, stocks, the aurora screensaver, the Home, Music and
//! Settings screens) and this shows them: see engine.rs for what it does
//! and manifest.rs for what it reads. It is the Rust port of wxshow.c, and
//! reads the same pictures and manifest.
//!
//! ```text
//! neochumby-wx [DIR]                  on the Chumby
//! neochumby-wx render DIR PREFIX ...  anywhere: frames to files (host.rs)
//! ```
//!
//! The pictures are in DIR, else $NEOCHUMBY_WX_DIR, else [`DEFAULT_DIR`].
//! Home's CHUMBY exits cleanly, and the session hands the Chumby back to its
//! own software.

mod cache;
#[cfg(target_arch = "arm")]
mod device;
mod draw;
mod engine;
mod host;
mod launch;
mod loader;
mod logo;
mod manifest;
mod plat;
mod scan;

/// Where the pictures are when neither the command line nor the
/// environment says.
pub const DEFAULT_DIR: &str = "/tmp/wx";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("render") {
        std::process::exit(host::render(&args[2..]));
    }
    let dir = args
        .get(1)
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("NEOCHUMBY_WX_DIR").map(Into::into))
        .unwrap_or_else(|| DEFAULT_DIR.into());
    #[cfg(target_arch = "arm")]
    std::process::exit(device::run(dir));
    #[cfg(not(target_arch = "arm"))]
    {
        eprintln!(
            "neochumby-wx: {} needs a Chumby; on this machine, use: neochumby-wx render DIR PREFIX",
            dir.display()
        );
        std::process::exit(2);
    }
}
