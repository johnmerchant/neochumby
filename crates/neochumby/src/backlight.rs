//! The backlight, always in one range: 0 (off) to 65535 (full). The kernel
//! has it as a backlight class device (pwm-backlight on the Chumby), whose
//! levels run from 0 to its own max_brightness: this scales between the two.
//! Only the session sets it for apps (`brightness <n>` from the start app);
//! an unjailed start app may set it itself.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Where the kernel's backlight devices are.
const CLASS: &str = "/sys/class/backlight";
/// Full brightness.
const FULL: u32 = crate::session::MAX_BRIGHTNESS;

/// The backlight's file, and the level it takes for full brightness.
pub struct Backlight {
    path: PathBuf,
    max: u32,
}

impl Backlight {
    /// The first backlight class device with a max_brightness, if any.
    pub fn find() -> Option<Backlight> {
        let mut devices: Vec<PathBuf> = fs::read_dir(CLASS).ok()?.flatten().map(|e| e.path()).collect();
        devices.sort();
        devices.into_iter().find_map(|d| {
            let max = fs::read_to_string(d.join("max_brightness")).ok()?.trim().parse().ok().filter(|m| *m > 0)?;
            Some(Backlight { path: d.join("brightness"), max })
        })
    }

    /// The file the level is written to, for the log.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The level now, 0 to 65535.
    pub fn level(&self) -> Option<u32> {
        let v: u32 = fs::read_to_string(&self.path).ok()?.trim().parse().ok()?;
        Some(scale(v.min(self.max), self.max, FULL))
    }

    /// Sets the level, 0 to 65535: one write to /sys.
    pub fn set(&self, level: u32) -> io::Result<()> {
        fs::write(&self.path, format!("{}\n", scale(level.min(FULL), FULL, self.max)))
    }
}

/// `v` of a range 0 to `from`, as the nearest of 0 to `to`. Anything above
/// 0 stays above it, so a dim level never turns the backlight off.
fn scale(v: u32, from: u32, to: u32) -> u32 {
    let s = ((u64::from(v) * u64::from(to) + u64::from(from) / 2) / u64::from(from)) as u32;
    if v > 0 { s.max(1) } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_between_the_ranges() {
        // pwm-backlight's 0-100.
        assert_eq!(scale(0, FULL, 100), 0);
        assert_eq!(scale(FULL, FULL, 100), 100);
        assert_eq!(scale(9000, FULL, 100), 14);
        assert_eq!(scale(1, FULL, 100), 1);
        assert_eq!(scale(14, 100, FULL), 9175);
        assert_eq!(scale(100, 100, FULL), FULL);
    }
}
