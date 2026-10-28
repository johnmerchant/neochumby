//! This Chumby's pairing, kept in /psp (its writable settings flash):
//!
//!   /psp/neochumby.conf   key=value lines: the neochumbyd (server,
//!                         api_port, server_key) and where its apps are
//!                         (nfs_port, mount_port, path). The boot hook
//!                         reads it too, and accepts only the characters
//!                         [`valid_value`] does.
//!   /psp/neochumby.key    this Chumby's private key (0600)

use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub const CONF: &str = "/psp/neochumby.conf";
pub const KEY: &str = "/psp/neochumby.key";

/// What a value may hold: what an address, port, path or hex key needs.
pub fn valid_value(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b".:/_-".contains(&b))
}

/// `key`'s value in a conf file.
pub fn get(path: &str, key: &str) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    s.lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
        .filter(|v| valid_value(v))
}

/// The neochumbyd this Chumby is paired with.
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
        secret: std::fs::read_to_string(KEY).ok()?.trim().to_string(),
    })
}

/// Writes `path` whole or not at all: a sibling file, renamed over it.
pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    let tmp = path.with_extension("new");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
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

/// The Chumby's name, as its owner set it in the stock settings
/// (/psp/hostname), else the kernel's host name unless unset ("(none)"),
/// else "chumby"; in the form a pairing takes: lower case, '_' for anything
/// else, at most 32.
pub fn name() -> String {
    ["/psp/hostname", "/proc/sys/kernel/hostname"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|raw| sanitise_name(raw.trim()))
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
    #[test]
    fn names() {
        assert_eq!(super::sanitise_name("Kitchen Chumby!"), "kitchen_chumby_");
        assert_eq!(super::sanitise_name("(none)"), "_none_");
        assert_eq!(super::sanitise_name(&"x".repeat(40)).len(), 32);
    }
}
