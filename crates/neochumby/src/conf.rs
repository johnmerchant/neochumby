//! This Chumby's pairing, kept in /psp (its writable settings flash):
//!
//!   /psp/neochumby.conf   key=value lines: the neochumbyd (server,
//!                         api_port, server_key) and where its apps are
//!                         (nfs_port, mount_port, path). The boot hook
//!                         reads it too, and accepts only the characters
//!                         [`valid_value`] does.
//!   /psp/neochumby.key    this Chumby's private key (0600)
//!   /psp/rfs1/userhook2   the boot hook, which pairing installs and the
//!                         session keeps up to date ([`refresh_hook`])

use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub const CONF: &str = "/psp/neochumby.conf";
pub const KEY: &str = "/psp/neochumby.key";
pub const HOOK: &str = "/psp/rfs1/userhook2";
/// A stick's own tree, made by hand (tools/stick).
pub const STICK_TREE: &str = "/mnt/usb/neochumby";

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

/// Brings the boot hook pairing installed up to date with the tree's copy,
/// so a Chumby paired before the hook changed (to boot synced sets, say)
/// boots the new way. Only a hook that is there is replaced, only from a
/// tree a neochumbyd delivered (over NFS, or synced to the stick), never
/// from a stick made by hand, and only by a higher version
/// ([`hook_version`]): after a rollback to an older set, its older hook
/// leaves the newer one alone. True if it was replaced.
pub fn refresh_hook(root: &Path) -> io::Result<bool> {
    let hook = Path::new(HOOK);
    if root == Path::new(STICK_TREE) || !hook.is_file() {
        return Ok(false);
    }
    install_hook(&crate::verified::read(&root.join("userhook2"))?)
}

/// Replaces the hook pairing installed with `new`, if that is a hook of a
/// higher version. True if it was replaced.
pub fn install_hook(new: &[u8]) -> io::Result<bool> {
    let hook = Path::new(HOOK);
    if !hook.is_file() {
        return Ok(false);
    }
    if new.len() > 64 << 10 || !new.starts_with(b"#!/bin/sh\n") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "the tree's userhook2 is not a hook"));
    }
    let old = std::fs::read(hook).unwrap_or_default();
    if hook_version(new) <= hook_version(&old) {
        return Ok(false);
    }
    write_atomic(hook, new, 0o755)?;
    Ok(true)
}

/// A hook's `# neochumby-hook-version=N` line; 0 for a hook from before
/// there was one.
pub fn hook_version(hook: &[u8]) -> u32 {
    String::from_utf8_lossy(hook)
        .lines()
        .take(10)
        .find_map(|l| l.strip_prefix("# neochumby-hook-version=")?.trim().parse().ok())
        .unwrap_or(0)
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
    fn hook_versions() {
        assert_eq!(super::hook_version(b"#!/bin/sh\n# neochumby-hook-version=3\nexit 0\n"), 3);
        assert_eq!(super::hook_version(b"#!/bin/sh\n# neochumby's boot hook\n"), 0);
        assert_eq!(super::hook_version(b"#!/bin/sh\n# neochumby-hook-version=x\n"), 0);
        assert_eq!(super::hook_version(b""), 0);
    }

    #[test]
    fn names() {
        assert_eq!(super::sanitise_name("Kitchen Chumby!"), "kitchen_chumby_");
        assert_eq!(super::sanitise_name("(none)"), "_none_");
        assert_eq!(super::sanitise_name(&"x".repeat(40)).len(), 32);
    }
}
