//! This Chumby's pairing, on its stick, which is its identity: the stick and
//! the key on it are what neochumbyd knows, and tools/stick wrote them when it
//! made the stick. Nothing here writes to the Chumby's own flash.
//!
//!   /mnt/usb/neochumby/state/neochumby.conf   key=value lines: the neochumbyd
//!                         (server, api_port, server_key), this Chumby's name,
//!                         and settings of its own (`start=`). The boot hook
//!                         reads it too, and accepts only the characters
//!                         [`valid_value`] does.
//!   /mnt/usb/neochumby/state/neochumby.key    this Chumby's private key
//!
//! FAT keeps no permissions: only root and neochumby's own programs, outside
//! the jails, can see the stick (SECURITY.md). A rename over a file on FAT can
//! tear, leaving only NAME.new; [`read_state`] takes that too.

use std::io;
use std::path::{Path, PathBuf};

use neochumby_sync::disk::{Disk, RealDisk};

/// Where the stock firmware, and so ncinit, mounts a stick.
pub const STICK: &str = "/mnt/usb";
/// A stick's own tree, made by hand (tools/stick), with its synced sets.
pub const STICK_TREE: &str = "/mnt/usb/neochumby";
/// What is kept on the stick for this Chumby.
pub const STATE: &str = "/mnt/usb/neochumby/state";
pub const CONF: &str = "/mnt/usb/neochumby/state/neochumby.conf";
pub const KEY: &str = "/mnt/usb/neochumby/state/neochumby.key";
/// What `Msg::SyncNow` leaves for neochumby-sync, which looks every second.
pub const SYNC_NOW: &str = "/tmp/neochumby-sync.now";

/// Whether a stick is mounted: nothing is written to its mount point if not.
pub fn stick_mounted() -> bool {
    RealDisk.mounted(Path::new(STICK))
}

/// A small file of the stick's state: as it is, else, if a torn rename left
/// it empty or gone, what was on its way to replacing it.
pub fn read_state(path: &str) -> Option<String> {
    let whole = |p: &str| std::fs::read_to_string(p).ok().filter(|s| !s.trim().is_empty());
    whole(path).or_else(|| whole(&format!("{path}.new")))
}

/// Writes `name` under `dir` whole, and on the stick before it returns, as a
/// synced set's markers are (neochumby_sync::stick::write_whole).
pub fn write_durable(dir: &Path, name: &str, data: &[u8]) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    neochumby_sync::stick::write_whole(&RealDisk, dir, name, data)
}

/// Writes `name` in the stick's state, if a stick is there.
pub fn write_state(name: &str, data: &[u8]) -> io::Result<()> {
    if !stick_mounted() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no stick mounted at {STICK}")));
    }
    write_durable(Path::new(STATE), name, data)
}

/// A directory of `what`'s on the stick (an app's settings), made if a stick
/// is there.
pub fn state_dir(what: &str) -> PathBuf {
    let d = Path::new(STATE).join(what);
    if stick_mounted() {
        let _ = std::fs::create_dir_all(&d);
    }
    d
}

/// What a value may hold: what an address, port, path or hex key needs.
pub fn valid_value(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b".:/_-".contains(&b))
}

/// `key`'s value in a conf file.
pub fn get(path: &str, key: &str) -> Option<String> {
    let s = read_state(path)?;
    s.lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
        .filter(|v| valid_value(v))
}

/// The neochumbyd this stick is paired with.
pub struct Paired {
    pub server: String,
    pub api_port: u16,
    /// Its public key, hex.
    pub server_key: String,
    /// This Chumby's private key, as neochumby-proto keeps it.
    pub secret: String,
}

pub fn paired() -> Option<Paired> {
    Some(Paired {
        server: get(CONF, "server")?,
        api_port: get(CONF, "api_port").and_then(|p| p.parse().ok()).unwrap_or(7340),
        server_key: get(CONF, "server_key")?,
        secret: read_state(KEY)?.trim().to_string(),
    })
}

/// key=value lines from pairs whose values are all [`valid_value`].
pub fn render(pairs: &[(&str, String)]) -> io::Result<String> {
    let mut out = String::new();
    for (k, v) in pairs {
        if !valid_value(v) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad value for {k}")));
        }
        out.push_str(&format!("{k}={v}\n"));
    }
    Ok(out)
}

/// The Chumby's name: the one its stick was made with (`name=`), else as its
/// owner set it in the stock settings (/psp/hostname, handed over), else the
/// kernel's host name unless unset ("(none)"), else "chumby"; lower case, '_'
/// for anything else, at most 32.
pub fn name() -> String {
    get(CONF, "name")
        .map(|n| sanitise_name(&n))
        .into_iter()
        .chain(
            ["/psp/hostname", "/proc/sys/kernel/hostname"]
                .iter()
                .filter_map(|p| std::fs::read_to_string(p).ok())
                .map(|raw| sanitise_name(raw.trim())),
        )
        .find(|n| !n.is_empty() && n != "_none_")
        .unwrap_or_else(|| "chumby".into())
}

fn sanitise_name(raw: &str) -> String {
    raw.chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whole, on a directory that wasn't there; over itself; and a torn
    /// rename, which leaves only NAME.new, is read through.
    #[test]
    fn durable_state() {
        let d = std::env::temp_dir().join(format!("neochumby-conf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        write_durable(&d.join("state"), "neochumby.conf", b"server=a\n").unwrap();
        write_durable(&d.join("state"), "neochumby.conf", b"server=b\n").unwrap();
        let conf = d.join("state/neochumby.conf");
        let conf = conf.to_str().unwrap();
        assert_eq!(get(conf, "server").as_deref(), Some("b"));
        assert!(!std::path::Path::new(&format!("{conf}.new")).exists());
        std::fs::rename(conf, format!("{conf}.new")).unwrap();
        assert_eq!(get(conf, "server").as_deref(), Some("b"));
        std::fs::write(conf, "").unwrap();
        assert_eq!(get(conf, "server").as_deref(), Some("b"), "an empty file is a torn one");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn names() {
        assert_eq!(super::sanitise_name("Kitchen Chumby!"), "kitchen_chumby_");
        assert_eq!(super::sanitise_name("(none)"), "_none_");
        assert_eq!(super::sanitise_name(&"x".repeat(40)).len(), 32);
    }
}
