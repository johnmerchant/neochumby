//! The kernel's modules, as depmod lists them in /lib/modules/<release>:
//! modules.dep (each module's file and the modules it needs) and
//! modules.alias (the device patterns each one drives). A device's
//! modalias, matched against the patterns, names the modules to load.

use std::collections::{HashMap, HashSet};

/// A module: its file, relative to the modules directory, and the modules
/// it needs loaded first.
#[derive(Debug, PartialEq, Eq)]
pub struct Module {
    pub path: String,
    pub deps: Vec<String>,
}

/// What depmod wrote.
#[derive(Debug, Default)]
pub struct Db {
    modules: HashMap<String, Module>,
    aliases: Vec<(String, String)>,
}

/// How deep a module's dependencies may go: a loop in modules.dep (which
/// depmod never writes) ends here.
const MAX_DEPTH: usize = 16;

impl Db {
    /// From the text of modules.dep and modules.alias. Lines it can't read
    /// are skipped.
    pub fn parse(dep: &str, alias: &str) -> Db {
        let mut modules = HashMap::new();
        for line in dep.lines() {
            let Some((path, deps)) = line.split_once(':') else { continue };
            let path = path.trim();
            let Some(name) = name_of(path) else { continue };
            let deps = deps.split_whitespace().filter_map(name_of).collect();
            modules.insert(name, Module { path: path.to_owned(), deps });
        }
        let mut aliases = Vec::new();
        for line in alias.lines() {
            let mut words = line.split_whitespace();
            if let (Some("alias"), Some(pattern), Some(module), None) =
                (words.next(), words.next(), words.next(), words.next())
            {
                aliases.push((pattern.to_owned(), normalize(module)));
            }
        }
        Db { modules, aliases }
    }

    pub fn module(&self, name: &str) -> Option<&Module> {
        self.modules.get(&normalize(name))
    }

    /// The modules whose patterns match `modalias`, each once, in the
    /// order modules.alias has them.
    pub fn matching(&self, modalias: &str) -> Vec<String> {
        let mut seen = HashSet::new();
        self.aliases
            .iter()
            .filter(|(pattern, _)| glob(pattern.as_bytes(), modalias.as_bytes()))
            .filter(|(_, m)| seen.insert(m.as_str()))
            .map(|(_, m)| m.clone())
            .collect()
    }

    /// `name` and what it needs, in the order to load them: each module
    /// after its dependencies, none twice, none in `loaded`. A module
    /// modules.dep doesn't list is left out.
    pub fn load_order(&self, name: &str, loaded: &HashSet<String>) -> Vec<String> {
        let mut order = Vec::new();
        let mut path = Vec::new();
        self.visit(&normalize(name), loaded, &mut order, &mut path);
        order
    }

    /// `path` holds the modules being visited, from `name`'s dependent up:
    /// a module on it again is a loop, and is cut there.
    fn visit(&self, name: &str, loaded: &HashSet<String>, order: &mut Vec<String>, path: &mut Vec<String>) {
        if path.len() > MAX_DEPTH
            || loaded.contains(name)
            || order.iter().any(|m| m == name)
            || path.iter().any(|m| m == name)
        {
            return;
        }
        let Some(m) = self.modules.get(name) else { return };
        path.push(name.to_owned());
        // depmod lists a module's dependencies with the ones they need
        // after them: load from the end.
        for dep in m.deps.iter().rev() {
            self.visit(dep, loaded, order, path);
        }
        path.pop();
        order.push(name.to_owned());
    }
}

/// A module's name from its file: kernel/drivers/hid/usbhid/usbhid.ko is
/// usbhid. Compressed modules (.ko.xz, .ko.gz) are not ours to load.
fn name_of(path: &str) -> Option<String> {
    let file = path.rsplit('/').next()?;
    let stem = file.strip_suffix(".ko")?;
    (!stem.is_empty()).then(|| normalize(stem))
}

/// The kernel's spelling of a module name: dashes are underscores.
pub fn normalize(name: &str) -> String {
    name.replace('-', "_")
}

/// The names in /proc/modules's text: the modules loaded now.
pub fn loaded(proc_modules: &str) -> HashSet<String> {
    proc_modules.lines().filter_map(|l| l.split_whitespace().next()).map(normalize).collect()
}

/// Whether `text` matches the shell pattern `pattern`, as fnmatch(3) with
/// no flags: `*` any run of bytes, `?` any byte, `[...]` a set (ranges,
/// `!` or `^` to negate), `\` quotes the next byte. Backtracks only to the
/// last `*`, so it takes time linear in the pattern times the text.
pub fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, t));
                p += 1;
                continue;
            }
            Some(&c) => {
                let step = match c {
                    b'?' => Some(1),
                    b'[' => match set(&pattern[p..], text[t]) {
                        Set::Hit(n) => Some(n),
                        Set::Miss => None,
                        Set::Unclosed => (text[t] == b'[').then_some(1),
                    },
                    b'\\' => match pattern.get(p + 1) {
                        Some(&q) if q == text[t] => Some(2),
                        Some(_) => None,
                        None => (text[t] == b'\\').then_some(1),
                    },
                    _ => (c == text[t]).then_some(1),
                };
                if let Some(n) = step {
                    p += n;
                    t += 1;
                    continue;
                }
            }
            None => {}
        }
        // A mismatch: let the last `*` take one more byte, if there was one.
        let Some((sp, st)) = star else { return false };
        star = Some((sp, st + 1));
        p = sp + 1;
        t = st + 1;
    }
    pattern[p.min(pattern.len())..].iter().all(|&c| c == b'*')
}

enum Set {
    /// It matches; the set is this long.
    Hit(usize),
    Miss,
    /// No closing `]`: a plain `[`.
    Unclosed,
}

/// Matches `c` against the set at the start of `pattern`, which begins
/// with `[`. A `]` first in the set is one of its members.
fn set(pattern: &[u8], c: u8) -> Set {
    let mut i = 1;
    let negate = matches!(pattern.get(i), Some(b'!' | b'^'));
    if negate {
        i += 1;
    }
    let mut hit = false;
    let mut first = true;
    loop {
        let Some(&lo) = pattern.get(i) else { return Set::Unclosed };
        if lo == b']' && !first {
            break;
        }
        first = false;
        match (pattern.get(i + 1), pattern.get(i + 2)) {
            (Some(b'-'), Some(&hi)) if hi != b']' => {
                hit |= lo <= c && c <= hi;
                i += 3;
            }
            _ => {
                hit |= lo == c;
                i += 1;
            }
        }
    }
    if hit != negate { Set::Hit(i + 1) } else { Set::Miss }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEP: &str = "\
kernel/drivers/hid/usbhid/usbhid.ko: kernel/drivers/hid/hid.ko
kernel/drivers/hid/hid-generic.ko: kernel/drivers/hid/hid.ko
kernel/drivers/hid/hid.ko:
kernel/drivers/mtd/nand/raw/mxc_nand.ko: kernel/drivers/mtd/nand/raw/nand.ko kernel/drivers/mtd/mtd.ko
kernel/drivers/mtd/nand/raw/nand.ko: kernel/drivers/mtd/mtd.ko
kernel/drivers/mtd/mtd.ko:
kernel/fs/xz/packed.ko.xz:
";
    const ALIAS: &str = "\
# Aliases extracted from modules themselves.
alias usb:v*p*d*dc*dsc*dp*ic03isc*ip*in* usbhid
alias hid:b*g0001v*p* hid_generic
alias of:N*T*Cfsl,imx21-nand mxc_nand
alias of:N*T*Cfsl,imx21-nandC* mxc_nand
";

    #[test]
    fn parses() {
        let db = Db::parse(DEP, ALIAS);
        assert_eq!(db.module("usbhid").unwrap().path, "kernel/drivers/hid/usbhid/usbhid.ko");
        assert_eq!(db.module("hid-generic").unwrap().deps, ["hid"]);
        assert!(db.module("packed").is_none());
    }

    #[test]
    fn matches_a_keyboard() {
        let db = Db::parse(DEP, ALIAS);
        // QEMU's usb-kbd, as the kernel announces its interface.
        assert_eq!(db.matching("usb:v0627p0001d0000dc00dsc00dp00ic03isc01ip01in00"), ["usbhid"]);
        assert_eq!(db.matching("hid:b0003g0001v00000627p00000001"), ["hid_generic"]);
        assert!(db.matching("usb:v0627p0001d0000dc09dsc00dp00ic09isc00ip00in00").is_empty());
        // Two patterns, one module, once.
        assert_eq!(db.matching("of:NnandT(null)Cfsl,imx21-nand"), ["mxc_nand"]);
    }

    #[test]
    fn orders_dependencies_first() {
        let db = Db::parse(DEP, ALIAS);
        let none = HashSet::new();
        assert_eq!(db.load_order("usbhid", &none), ["hid", "usbhid"]);
        assert_eq!(db.load_order("mxc_nand", &none), ["mtd", "nand", "mxc_nand"]);
        assert_eq!(
            db.load_order("mxc-nand", &loaded("mtd 1 0 - Live 0x0\nnand 2 1 mxc_nand, Live 0x0\n")),
            ["mxc_nand"]
        );
        assert!(db.load_order("nonesuch", &none).is_empty());
    }

    #[test]
    fn survives_a_loop() {
        let db = Db::parse("a.ko: b.ko\nb.ko: a.ko\n", "");
        assert_eq!(db.load_order("a", &HashSet::new()), ["b", "a"]);
    }

    #[test]
    fn globs() {
        assert!(glob(b"*", b""));
        assert!(glob(b"a*c", b"abbbc"));
        assert!(!glob(b"a*c", b"abbbd"));
        assert!(glob(b"a?c", b"abc"));
        assert!(!glob(b"a?c", b"ac"));
        assert!(glob(b"[a-c]x", b"bx"));
        assert!(!glob(b"[!a-c]x", b"bx"));
        assert!(glob(b"[^a-c]x", b"dx"));
        assert!(glob(b"[]]", b"]"));
        assert!(glob(b"[a-]", b"-"));
        assert!(glob(b"\\*", b"*"));
        assert!(!glob(b"\\*", b"a"));
        assert!(glob(b"[ab", b"[ab"));
        assert!(glob(b"*a*a*a*b", b"aaaaaaaaaaaaaaaaaaaaaaaab"));
        assert!(!glob(b"*a*a*a*b", b"aaaaaaaaaaaaaaaaaaaaaaaaa"));
    }
}
