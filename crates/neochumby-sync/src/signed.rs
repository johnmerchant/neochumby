//! `delivery=nfs`, checked: the tree's manifest, signed by neochumbyd's
//! key, which the Chumby pinned when it paired. NFS itself is
//! unauthenticated, so anyone who can answer for the server could serve a
//! tree of their own; a Chumby runs nothing from the tree that this
//! manifest doesn't vouch for.
//!
//! neochumbyd serves the manifest as [`FILE`] in the tree's root (it is no
//! file of the tree: names starting with '.' are left out of it). It is
//! the same manifest a sync sends, with the same digest:
//!
//!   neochumby-tree 1
//!   9f86d0...  1234  neochumby
//!   ...
//!   end 42 <digest hex>
//!   sig <Ed25519 signature hex, over [`CONTEXT`] and every line above>
//!
//! The Chumby checks the signature with the key it pinned, then checks
//! each file it uses against its line, as it reads it: a program is
//! copied out of the tree into RAM ([`Verified::copy`]) and run from
//! there, so what was checked is what runs, whatever NFS serves later; a
//! small file (a manifest, the boot hook) is read whole and used from what
//! was read ([`Verified::read`]). A file the manifest doesn't name, or
//! whose bytes differ, is refused.
//!
//! An older manifest, with its older tree, still bears the key's
//! signature: someone who can answer for the server can serve one that
//! neochumbyd served before. That runs only code the owner once deployed.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use neochumby_proto::{PublicKey, hex, unhex};
use ring::digest::{Context, SHA256};
use ring::signature::{ED25519, UnparsedPublicKey};

use crate::manifest::{self, line, parse_line};
use crate::{CHUNK, Entry, LIMITS, invalid};

/// The manifest's name in the tree's root, as neochumbyd serves it.
pub const FILE: &str = ".neochumby-manifest";

pub const HEADER: &str = "neochumby-tree 1";

/// What the signature covers first, so it can be taken for no other
/// message the key signs.
pub const CONTEXT: &[u8] = b"neochumby tree manifest\0";

/// The longest manifest taken: LIMITS' files at the longest line each.
pub const MAX_LEN: u64 = LIMITS.max_files as u64 * 330 + 300;

/// The signed manifest of `entries`, with `sign` giving the signature of
/// what it is passed.
pub fn sign(entries: &[Entry], sign: impl FnOnce(&[u8]) -> [u8; 64]) -> String {
    let mut body = format!("{HEADER}\n");
    for e in entries {
        body.push_str(&line(e));
    }
    body.push_str(&format!("end {} {}\n", entries.len(), hex(&manifest::digest(entries))));
    let sig = sign(&[CONTEXT, body.as_bytes()].concat());
    body + &format!("sig {}\n", hex(&sig))
}

/// A manifest whose signature checked: the tree's files, by path.
#[derive(Debug)]
pub struct Verified {
    pub digest: [u8; 32],
    files: BTreeMap<String, Entry>,
}

/// The manifest in `text`, if `key` signed it and it is whole and one a
/// Chumby takes ([`manifest::validate`]).
pub fn open(text: &[u8], key: &PublicKey) -> io::Result<Verified> {
    let text = std::str::from_utf8(text).map_err(|_| invalid("the manifest is not text"))?;
    let body_end = text.rfind("sig ").filter(|i| *i == 0 || text.as_bytes()[i - 1] == b'\n');
    let Some(i) = body_end else { return Err(invalid("the manifest is not signed")) };
    let (body, sig) = text.split_at(i);
    let sig = sig.strip_prefix("sig ").and_then(|s| s.strip_suffix('\n')).ok_or_else(|| invalid("no signature"))?;
    let sig = unhex(sig).ok().filter(|s| s.len() == 64).ok_or_else(|| invalid("a bad signature"))?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&[CONTEXT, body.as_bytes()].concat(), &sig)
        .map_err(|_| invalid("the manifest's signature is not the paired neochumbyd's"))?;

    let mut lines: Vec<&str> = body.strip_suffix('\n').ok_or_else(|| invalid("cut short"))?.split('\n').collect();
    if lines.len() < 2 || lines[0] != HEADER {
        return Err(invalid("not a tree's manifest"));
    }
    let end = lines.pop().expect("two lines");
    let entries = lines[1..]
        .iter()
        .map(|l| parse_line(l).ok_or_else(|| invalid(format!("bad line {l:?}"))))
        .collect::<io::Result<Vec<Entry>>>()?;
    let digest = manifest::digest(&entries);
    if end != format!("end {} {}", entries.len(), hex(&digest)) {
        return Err(invalid("the manifest's end does not match it"));
    }
    manifest::validate(&entries, &LIMITS)?;
    Ok(Verified { digest, files: entries.into_iter().map(|e| (e.path.clone(), e)).collect() })
}

/// The manifest at `root`/[`FILE`], if `key` signed it.
pub fn load(root: &Path, key: &PublicKey) -> io::Result<Verified> {
    open(&read_capped(&root.join(FILE), MAX_LEN)?, key)
}

/// At most `max` bytes of `path`; more is an error.
pub fn read_capped(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let mut b = Vec::new();
    open_file(path)?.take(max + 1).read_to_end(&mut b)?;
    if b.len() as u64 > max {
        return Err(invalid(format!("{}: longer than {max} bytes", path.display())));
    }
    Ok(b)
}

/// `path` to read, not through a link at its end.
fn open_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).custom_flags(neochumby_sys::fs::NO_FOLLOW).open(path)
}

impl Verified {
    /// `rel`'s line, if the manifest names it.
    pub fn entry(&self, rel: &str) -> io::Result<&Entry> {
        self.files.get(rel).ok_or_else(|| invalid(format!("{rel}: not in the signed manifest")))
    }

    /// `root`/`rel`, read whole, if its bytes are the manifest's.
    pub fn read(&self, root: &Path, rel: &str) -> io::Result<Vec<u8>> {
        let e = self.entry(rel)?;
        let b = read_capped(&root.join(rel), e.size)?;
        if b.len() as u64 != e.size || ring::digest::digest(&SHA256, &b).as_ref() != e.sha256 {
            return Err(invalid(format!("{rel}: not the file the signed manifest names")));
        }
        Ok(b)
    }

    /// Copies `root`/`rel` to `to`, mode `mode`, if its bytes are the
    /// manifest's; else nothing is left at `to`. The file is read once:
    /// what is written is what was hashed. Of an ELF, only what is loaded
    /// is written ([`loaded_len`]): the rest (symbols, line tables) is
    /// never read on the Chumby, and RAM is short there.
    pub fn copy(&self, root: &Path, rel: &str, to: &Path, mode: u32) -> io::Result<()> {
        let e = self.entry(rel)?;
        let tmp = to.with_extension("part");
        let r = copy_checked(e, &root.join(rel), &tmp, mode).and_then(|()| fs::rename(&tmp, to));
        if r.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        r
    }
}

fn copy_checked(e: &Entry, from: &Path, to: &Path, mode: u32) -> io::Result<()> {
    let _ = fs::remove_file(to);
    let mut src = open_file(from)?;
    let mut out = OpenOptions::new().write(true).create_new(true).mode(mode).open(to)?;
    let mut c = Context::new(&SHA256);
    let mut buf = vec![0u8; CHUNK];
    let (mut n, mut keep) = (0u64, None);
    loop {
        let k = src.read(&mut buf)?;
        if k == 0 {
            break;
        }
        if n + k as u64 > e.size {
            return Err(invalid(format!("{}: longer than the signed manifest says", e.path)));
        }
        if n == 0 {
            keep = Some(loaded_len(&buf[..k]).unwrap_or(e.size));
        }
        c.update(&buf[..k]);
        let wanted = keep.unwrap_or(e.size).saturating_sub(n).min(k as u64) as usize;
        out.write_all(&buf[..wanted])?;
        n += k as u64;
    }
    if n != e.size || c.finish().as_ref() != e.sha256 {
        return Err(invalid(format!("{}: not the file the signed manifest names", e.path)));
    }
    out.sync_all()
}

/// How much of a 32-bit little-endian ELF (the Chumby's) the kernel reads
/// to run it: up to the end of the furthest segment, or of the program
/// headers. None if `head` is no such ELF, or doesn't hold its program
/// headers; then the whole file is kept.
pub fn loaded_len(head: &[u8]) -> Option<u64> {
    let u16_at = |o: usize| head.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as u64);
    let u32_at = |o: usize| head.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64);
    if head.get(..6)? != b"\x7fELF\x01\x01" {
        return None;
    }
    let (phoff, phentsize, phnum) = (u32_at(0x1c)?, u16_at(0x2a)?, u16_at(0x2c)?);
    if phentsize < 32 {
        return None;
    }
    let mut end = phoff + phentsize * phnum;
    for i in 0..phnum {
        let ph = (phoff + i * phentsize) as usize;
        end = end.max(u32_at(ph + 4)? + u32_at(ph + 16)?);
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::tests::entry;
    use neochumby_proto::Identity;

    fn signed(id: &Identity, entries: &[Entry]) -> String {
        sign(entries, |m| id.sign(m).expect("a TLS build's key signs"))
    }

    /// A tree on disk, its manifest, and the key that signs it.
    fn tree(name: &str, files: &[(&str, &[u8])]) -> (std::path::PathBuf, Vec<Entry>) {
        let d = std::env::temp_dir().join(format!("neochumby-signed-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let mut entries = Vec::new();
        for (p, b) in files {
            let f = d.join("tree").join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(&f, b).unwrap();
            entries.push(entry(p, b));
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        fs::create_dir_all(d.join("run")).unwrap();
        (d, entries)
    }

    #[test]
    fn signed_by_the_paired_key_only() {
        let (id, other) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let m = vec![entry("apps/home/manifest", b"exec=x\n"), entry("neochumby", b"session")];
        let text = signed(&id, &m);
        let v = open(text.as_bytes(), &id.public()).unwrap();
        assert_eq!(v.digest, manifest::digest(&m));
        assert_eq!(v.entry("neochumby").unwrap(), &m[1]);
        assert!(v.entry("apps/doom/neochumby-doom").is_err());

        assert!(open(text.as_bytes(), &other.public()).is_err(), "another key");
        assert!(open(signed(&other, &m).as_bytes(), &id.public()).is_err(), "signed by another");
        // Any change at all: a line, the end, the header, the signature.
        for (from, to) in [
            ("neochumby\n", "neochumbx\n"),
            ("end 2", "end 3"),
            ("neochumby-tree 1", "neochumby-tree 2"),
            ("sig ", "sig 00"),
        ] {
            let changed = text.replacen(from, to, 1);
            assert_ne!(changed, text);
            assert!(open(changed.as_bytes(), &id.public()).is_err(), "{from} -> {to}");
        }
        // A line taken out, though the end and signature are made to match:
        // not by this key.
        let fewer = signed(&other, &m[1..]);
        assert!(open(fewer.as_bytes(), &id.public()).is_err());
        // Cut short anywhere.
        for n in 0..text.len() {
            assert!(open(&text.as_bytes()[..n], &id.public()).is_err(), "cut at {n}");
        }
        // A manifest a Chumby wouldn't take is refused, signed or not.
        assert!(open(signed(&id, &m[..1]).as_bytes(), &id.public()).is_err(), "no session");
        let mut bad = m[1].clone();
        bad.path = "../neochumby".into();
        assert!(open(signed(&id, &[bad]).as_bytes(), &id.public()).is_err(), "a path out of the tree");
    }

    /// A tampered file on the export is refused, and leaves nothing behind
    /// to run; an untampered one is copied whole.
    #[test]
    fn tampered_files_are_refused() {
        let id = Identity::generate().unwrap();
        let files: &[(&str, &[u8])] =
            &[("neochumby", b"the session"), ("apps/home/manifest", b"exec=neochumby-home\n"), ("data", b"x")];
        let (d, m) = tree("tamper", files);
        let root = d.join("tree");
        fs::write(root.join(FILE), signed(&id, &m)).unwrap();
        let v = load(&root, &id.public()).unwrap();
        let to = d.join("run/neochumby");
        v.copy(&root, "neochumby", &to, 0o700).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"the session");
        assert_eq!(v.read(&root, "apps/home/manifest").unwrap(), b"exec=neochumby-home\n");

        // The same size, other bytes; longer; shorter; not named at all.
        fs::write(root.join("neochumby"), b"the sessiom").unwrap();
        fs::remove_file(&to).unwrap();
        assert!(v.copy(&root, "neochumby", &to, 0o700).is_err());
        assert!(!to.exists() && !to.with_extension("part").exists(), "nothing left to run");
        for bytes in [&b"the session, and more"[..], b"the"] {
            fs::write(root.join("neochumby"), bytes).unwrap();
            assert!(v.copy(&root, "neochumby", &to, 0o700).is_err());
        }
        fs::write(root.join("apps/home/manifest"), b"exec=evil\n\n\n\n\n\n\n\n\n\n").unwrap();
        assert!(v.read(&root, "apps/home/manifest").is_err());
        fs::write(root.join("apps/home/evil"), b"x").unwrap();
        assert!(v.read(&root, "apps/home/evil").is_err());
        assert!(v.copy(&root, "apps/home/evil", &to, 0o700).is_err());

        // A link in the tree's place is not followed.
        fs::remove_file(root.join("data")).unwrap();
        std::os::unix::fs::symlink(d.join("run/x"), root.join("data")).unwrap();
        fs::write(d.join("run/x"), b"x").unwrap();
        assert!(v.read(&root, "data").is_err());

        // A tree whose manifest was replaced by one of someone else's.
        let other = Identity::generate().unwrap();
        fs::write(root.join(FILE), signed(&other, &m)).unwrap();
        assert!(load(&root, &id.public()).is_err());
        fs::remove_dir_all(d).unwrap();
    }

    /// An ELF is copied only as far as it is loaded.
    #[test]
    fn elf_copies_stop_at_what_is_loaded() {
        // A header, one program header at 52 (PT_LOAD, offset 0, 200 bytes
        // in the file), and debug sections after.
        let mut elf = vec![0u8; 1000];
        elf[..6].copy_from_slice(b"\x7fELF\x01\x01");
        elf[0x1c..0x20].copy_from_slice(&52u32.to_le_bytes());
        elf[0x2a..0x2c].copy_from_slice(&32u16.to_le_bytes());
        elf[0x2c..0x2e].copy_from_slice(&1u16.to_le_bytes());
        elf[52..56].copy_from_slice(&1u32.to_le_bytes());
        elf[52 + 16..52 + 20].copy_from_slice(&200u32.to_le_bytes());
        assert_eq!(loaded_len(&elf), Some(200));
        assert_eq!(loaded_len(b"#!/bin/sh\n"), None);
        assert_eq!(loaded_len(&elf[..60]), None, "program headers not all there");

        let id = Identity::generate().unwrap();
        let (d, m) = tree("elf", &[("neochumby", &elf)]);
        let root = d.join("tree");
        let v = open(signed(&id, &m).as_bytes(), &id.public()).unwrap();
        let to = d.join("run/neochumby");
        v.copy(&root, "neochumby", &to, 0o700).unwrap();
        assert_eq!(fs::read(&to).unwrap(), &elf[..200]);
        // A byte changed past what is loaded still fails the whole file.
        let mut tampered = elf.clone();
        tampered[900] = 1;
        fs::write(root.join("neochumby"), &tampered).unwrap();
        assert!(v.copy(&root, "neochumby", &to, 0o700).is_err());
        fs::remove_dir_all(d).unwrap();
    }
}
