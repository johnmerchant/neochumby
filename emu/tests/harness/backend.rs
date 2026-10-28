//! What the tests need of an emulator, and nothing more: boot a Chumby
//! from a kernel and a NAND image, its serial console as a byte stream
//! both ways, the touchscreen, the screen, and stopping it. Each emulator
//! is a [`Backend`]; the scenarios never see which one they run on.
//!
//! - `chumby-emu` ([`super::chumby_emu`]): the console on the process's
//!   stdin and stdout, everything else over its control port.
//! - QEMU with the i.MX21 machine ([`super::qemu`]): the console on a
//!   serial socket, input and `screendump` over QMP.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub type Result<T> = std::result::Result<T, String>;

/// What a Chumby boots from.
pub struct Media {
    /// A zImage, as in a Kernel partition.
    pub kernel: PathBuf,
    /// A whole NAND image (`chumby-emu mknand`, `patchnand`). A backend
    /// may write to it; the harness gives each boot its own copy.
    pub nand: PathBuf,
    /// A USB stick image (FAT), for a backend that models USB storage.
    /// Neither does yet: the tests put the stick in the Cache partition.
    pub stick: Option<PathBuf>,
}

/// A booted Chumby: its console streams and the machine to drive.
pub struct Booted {
    /// Everything the Chumby writes on its serial console.
    pub console_out: Box<dyn Read + Send>,
    /// Typed on its serial console. A line ends with `\n`.
    pub console_in: Box<dyn Write + Send>,
    pub machine: Box<dyn Machine>,
}

pub trait Backend {
    fn name(&self) -> &'static str;
    /// Whether it can run here; the reason not, which skips the tests.
    fn available(&self) -> Result<()> {
        Ok(())
    }
    /// Boots `media`. Logs and screen dumps go in `work`.
    fn boot(&self, media: &Media, work: &Path) -> Result<Booted>;
}

/// A running emulated Chumby. Coordinates are screen pixels (320x240).
pub trait Machine: Send {
    /// Touches the screen and holds.
    fn press(&mut self, x: u32, y: u32) -> Result<()>;
    fn release(&mut self) -> Result<()>;
    /// Touches for `hold`, then releases; returns once released. The hold
    /// is in the Chumby's time where the backend can do that, so that a
    /// slow host doesn't shorten it.
    fn tap(&mut self, x: u32, y: u32, hold: Duration) -> Result<()>;
    /// Writes the screen to `path` as a binary PPM (P6).
    fn screendump(&mut self, path: &Path) -> Result<()>;
    /// Stops the emulator. Called once; dropping a Machine stops it too.
    fn stop(&mut self) -> Result<()>;
}
