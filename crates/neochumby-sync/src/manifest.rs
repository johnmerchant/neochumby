//! Manifests: a set's files, in path order, as `<sha256 hex> <size>
//! <path>` lines. Both ends compute a manifest's digest the same way, over
//! those lines, so a Chumby can say what its live set is in 32 bytes.
//!
//! A set's completion marker is its manifest between a header and an end
//! line carrying the count and the digest:
//!
//!   neochumby-set 1
//!   9f86d0...  1234  neochumby
//!   ...
//!   end 42 <digest hex>
//!
//! The boot hook (BusyBox sh) checks the first and last lines; `neochumby
//! check-set` checks the rest.

use std::collections::{BTreeMap, HashSet};
use std::io;

use neochumby_proto::{MAX_MSG, Msg, hex, unhex};
use ring::digest::{Context, SHA256};

use crate::{Entry, Limits, invalid};

pub const HEADER: &str = "neochumby-set 1";

/// The session: a set without it cannot boot, so none is taken.
pub const SESSION: &str = "neochumby";

/// Whether `p` may name a file in a set: relative and '/'-separated, at
/// most 16 deep and 240 bytes. Each component is 1 to 128 of A-Z a-z 0-9
/// `.` `_` `+` `-`, neither starting nor ending with '.'. So never `.`,
/// `..` or an absolute path; never a name FAT would change (it drops
/// trailing dots); never the receiver's own temporary names, which start
/// with '.'; and never a name the Chumby's vfat refuses: `aux`, `con`,
/// `nul`, `prn`, `com1` to `com9` or `lpt1` to `lpt9` before the first '.',
/// in any case (`con`, `Aux.txt`).
pub fn valid_path(p: &str) -> bool {
    (1..=240).contains(&p.len())
        && p.split('/').count() <= 16
        && p.split('/').all(|c| {
            (1..=128).contains(&c.len())
                && !c.starts_with('.')
                && !c.ends_with('.')
                && c.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
                && !reserved(c)
        })
}

/// A DOS device name, which 2.6.16's vfat_valid_longname refuses.
fn reserved(c: &str) -> bool {
    let stem = c.split('.').next().unwrap_or("").to_ascii_lowercase();
    match stem.as_bytes() {
        b"aux" | b"con" | b"nul" | b"prn" => true,
        [b'c', b'o', b'm', d] | [b'l', b'p', b't', d] => (b'1'..=b'9').contains(d),
        _ => false,
    }
}

pub fn line(e: &Entry) -> String {
    format!("{} {} {}\n", hex(&e.sha256), e.size, e.path)
}

/// One line (without its newline), if it is a whole, valid one.
pub fn parse_line(l: &str) -> Option<Entry> {
    let mut f = l.splitn(3, ' ');
    let (h, size, path) = (f.next()?, f.next()?, f.next()?);
    let sha256 = (h.len() == 64).then(|| unhex(h).ok()).flatten()?.try_into().ok()?;
    let size = size.parse().ok().filter(|_| size.bytes().all(|b| b.is_ascii_digit()))?;
    valid_path(path).then(|| Entry { path: path.into(), size, sha256 })
}

/// The digest of a manifest: SHA-256 over its lines.
pub fn digest(entries: &[Entry]) -> [u8; 32] {
    let mut c = Context::new(&SHA256);
    for e in entries {
        c.update(line(e).as_bytes());
    }
    c.finish().as_ref().try_into().expect("32 bytes")
}

/// Whether a Chumby will write `entries` as a set: within its limits,
/// strictly in path order, every path valid, no two the same to FAT (which
/// ignores case), no file where another needs a directory, and the session
/// among them.
pub fn validate(entries: &[Entry], limits: &Limits) -> io::Result<()> {
    if entries.len() > limits.max_files {
        return Err(invalid(format!("{} files, more than {}", entries.len(), limits.max_files)));
    }
    let mut total = 0u64;
    let mut files = HashSet::new();
    for (i, e) in entries.iter().enumerate() {
        if !valid_path(&e.path) {
            return Err(invalid(format!("bad path {:?}", e.path)));
        }
        if i > 0 && entries[i - 1].path >= e.path {
            return Err(invalid(format!("{} out of order", e.path)));
        }
        if e.size > limits.max_file {
            return Err(invalid(format!("{}: {} bytes, more than {}", e.path, e.size, limits.max_file)));
        }
        total = total.saturating_add(e.size);
        if !files.insert(e.path.to_ascii_lowercase()) {
            return Err(invalid(format!("{}: twice, to FAT", e.path)));
        }
    }
    if total > limits.max_total {
        return Err(invalid(format!("{total} bytes in all, more than {}", limits.max_total)));
    }
    for e in entries {
        let lower = e.path.to_ascii_lowercase();
        for (i, _) in lower.match_indices('/') {
            if files.contains(&lower[..i]) {
                return Err(invalid(format!("{}: {} is a file", e.path, &e.path[..i])));
            }
        }
    }
    if !entries.iter().any(|e| e.path == SESSION) {
        return Err(invalid("no session (neochumby) in the set"));
    }
    Ok(())
}

/// A set's completion marker.
pub fn marker(entries: &[Entry]) -> String {
    let mut s = format!("{HEADER}\n");
    for e in entries {
        s.push_str(&line(e));
    }
    s.push_str(&format!("end {} {}\n", entries.len(), hex(&digest(entries))));
    s
}

/// The manifest in a marker, if the marker is whole and the manifest one
/// [`validate`] takes.
pub fn parse_marker(s: &str, limits: &Limits) -> io::Result<Vec<Entry>> {
    let body = s.strip_suffix('\n').ok_or_else(|| invalid("marker cut short"))?;
    let mut lines: Vec<&str> = body.split('\n').collect();
    if lines.len() < 2 || lines[0] != HEADER {
        return Err(invalid("not a marker"));
    }
    let end = lines.pop().expect("two lines");
    let mut f = end.split(' ');
    let (count, sum) = match (f.next(), f.next(), f.next(), f.next()) {
        (Some("end"), Some(n), Some(d), None) => (n, d),
        _ => return Err(invalid("marker cut short")),
    };
    let entries = lines[1..]
        .iter()
        .map(|l| parse_line(l).ok_or_else(|| invalid(format!("bad line {l:?}"))))
        .collect::<io::Result<Vec<Entry>>>()?;
    if count.parse::<usize>().ok() != Some(entries.len()) || sum != hex(&digest(&entries)) {
        return Err(invalid("marker does not match its manifest"));
    }
    validate(&entries, limits)?;
    Ok(entries)
}

/// The files a journal lists: whole, valid lines only (the last may have
/// been cut short), the latest for each path, in path order.
pub fn parse_journal(s: &str) -> Vec<Entry> {
    let whole = s.rfind('\n').map_or("", |i| &s[..i]);
    let latest: BTreeMap<String, Entry> = whole.lines().filter_map(parse_line).map(|e| (e.path.clone(), e)).collect();
    latest.into_values().collect()
}

/// The files of `tree` that a set holding `have` lacks: those whose size or
/// hash differs, or that it does not have at all. Each is sent whole.
pub fn plan<'a>(tree: &'a [Entry], have: &[Entry]) -> Vec<&'a Entry> {
    let have: HashSet<&Entry> = have.iter().collect();
    tree.iter().filter(|e| !have.contains(e)).collect()
}

/// A manifest as `SyncManifest` messages, each within [`MAX_MSG`].
pub fn messages(entries: &[Entry]) -> Vec<Msg> {
    // A tag, the `last` flag and a count, then each entry: its path (a u16
    // length and the bytes), size and hash.
    let budget = MAX_MSG - 4;
    let mut out = Vec::new();
    let mut chunk = Vec::new();
    let mut used = 0;
    for e in entries {
        let n = 2 + e.path.len() + 8 + 32;
        if used + n > budget || chunk.len() == u16::MAX as usize {
            out.push(Msg::SyncManifest { entries: std::mem::take(&mut chunk), last: false });
            used = 0;
        }
        chunk.push(e.clone());
        used += n;
    }
    out.push(Msg::SyncManifest { entries: chunk, last: true });
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn entry(path: &str, data: &[u8]) -> Entry {
        let h = ring::digest::digest(&SHA256, data);
        Entry { path: path.into(), size: data.len() as u64, sha256: h.as_ref().try_into().unwrap() }
    }

    #[test]
    fn paths_stay_inside_the_set() {
        for good in ["neochumby", "apps/home/manifest", "apps/quake/id1/pak0.pak", "a+b_c-d.e", "x/y.z"] {
            assert!(valid_path(good), "{good}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "..",
            "../x",
            "apps/../../x",
            "apps/./x",
            ".",
            "apps//x",
            "apps/",
            "/",
            ".hidden",
            "apps/.x.part",
            "trailing.",
            "a b",
            "a\\b",
            "c:x",
            "a\nb",
            "é",
        ] {
            assert!(!valid_path(bad), "{bad:?}");
        }
        for dos in ["con", "AUX", "nul.txt", "apps/Prn.raw", "com1", "LPT9.x.y", "a/com5/b"] {
            assert!(!valid_path(dos), "{dos}");
        }
        for fine in ["com10", "com0", "console", "lpt", "auxv", "x.con", "nul_", "comx"] {
            assert!(valid_path(fine), "{fine}");
        }
        assert!(valid_path(&"x".repeat(128)) && !valid_path(&"x".repeat(129)));
        assert!(!valid_path(&["a"; 17].join("/")));
        assert!(!valid_path(&[&"x".repeat(120)[..], &"y".repeat(120), "z"].join("/")));
    }

    #[test]
    fn validation() {
        let l = Limits { max_files: 4, max_file: 10, max_total: 20 };
        let s = entry("neochumby", b"session");
        assert!(validate(&[entry("a", b"1"), s.clone()], &l).is_ok());
        assert!(validate(&[s.clone(), entry("a", b"1")], &l).is_err(), "order");
        assert!(validate(&[entry("a", b"1"), entry("a", b"2"), s.clone()], &l).is_err(), "twice");
        assert!(validate(&[entry("A", b"1"), entry("a", b"2"), s.clone()], &l).is_err(), "case");
        assert!(validate(&[entry("a", b"1"), entry("a/b", b"2"), s.clone()], &l).is_err(), "file and dir");
        assert!(validate(&[entry("A", b"1"), entry("a/b", b"2"), s.clone()], &l).is_err(), "file and dir, case");
        assert!(validate(&[entry("a", b"1")], &l).is_err(), "no session");
        assert!(validate(&[entry("a", &[0; 11]), s.clone()], &l).is_err(), "file size");
        assert!(validate(&[entry("a", &[0; 10]), entry("b", &[0; 10]), s.clone()], &l).is_err(), "total size");
        let five: Vec<Entry> = ["a", "b", "c", "d"].iter().map(|p| entry(p, b"")).chain([s.clone()]).collect();
        assert!(validate(&five, &l).is_err(), "count");
        let mut bad = s.clone();
        bad.path = "../neochumby".into();
        assert!(validate(&[bad], &l).is_err());
    }

    #[test]
    fn markers() {
        let m = vec![entry("apps/home/manifest", b"home=x\n"), entry("neochumby", b"session")];
        let text = marker(&m);
        assert!(text.starts_with("neochumby-set 1\n") && text.ends_with('\n'));
        assert_eq!(parse_marker(&text, &crate::LIMITS).unwrap(), m);
        // Cut short anywhere: refused.
        for n in 0..text.len() {
            assert!(parse_marker(&text[..n], &crate::LIMITS).is_err(), "cut at {n}");
        }
        // A line changed, a line dropped, the count or digest wrong.
        let changed = text.replace("neochumby\n", "neochumbx\n");
        assert!(parse_marker(&changed, &crate::LIMITS).is_err());
        let lines: Vec<&str> = text.lines().collect();
        let dropped = [lines[0], lines[2], lines[3]].join("\n") + "\n";
        assert!(parse_marker(&dropped, &crate::LIMITS).is_err());
        let count = text.replace("end 2 ", "end 3 ");
        assert!(parse_marker(&count, &crate::LIMITS).is_err());
        assert!(parse_marker(&text.replace("neochumby-set 1", "neochumby-set 2"), &crate::LIMITS).is_err());
        // An empty manifest is whole but has no session.
        assert!(parse_marker(&marker(&[]), &crate::LIMITS).is_err());
    }

    #[test]
    fn journals() {
        let (a, b) = (entry("a", b"1"), entry("b", b"2"));
        let newer = entry("a", b"3");
        let j = line(&b) + &line(&a) + &line(&newer) + "garbage\n" + &line(&entry("c", b"4"))[..20];
        assert_eq!(parse_journal(&j), [newer, b]);
        assert!(parse_journal("").is_empty());
    }

    #[test]
    fn planning() {
        let s = entry("neochumby", b"session 2");
        let tree = vec![entry("a", b"same"), entry("b", b"new"), entry("d", b"added"), s.clone()];
        let have = vec![entry("a", b"same"), entry("b", b"old"), entry("c", b"gone"), entry("neochumby", b"session 1")];
        let paths: Vec<&str> = plan(&tree, &have).iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["b", "d", "neochumby"]);
        assert!(plan(&tree, &tree).is_empty());
        assert_eq!(plan(&tree, &[]).len(), 4);
        // Same hash, different size claimed: sent.
        let mut lie = entry("a", b"same");
        lie.size += 1;
        assert_eq!(plan(&tree, &[lie]).len(), 4);
        assert_ne!(digest(&tree), digest(&have));
        assert_eq!(digest(&tree), digest(&tree.clone()));
    }

    #[test]
    fn manifest_messages_fit() {
        let many: Vec<Entry> = (0..2000).map(|i| entry(&format!("apps/x/{}{i}", "f".repeat(200)), b"")).collect();
        let msgs = messages(&many);
        assert!(msgs.len() > 1);
        let mut back = Vec::new();
        for (i, m) in msgs.iter().enumerate() {
            assert!(m.encode().len() <= MAX_MSG);
            let Msg::SyncManifest { entries, last } = Msg::decode(&m.encode()).unwrap() else { panic!() };
            assert_eq!(last, i == msgs.len() - 1);
            back.extend(entries);
        }
        assert_eq!(back, many);
        assert_eq!(messages(&[]), [Msg::SyncManifest { entries: vec![], last: true }]);
    }
}
