//! What a module may import, by what its manifest grants. Every import is
//! checked before the module runs a single instruction: one the app isn't
//! granted, or one nothing provides, refuses the module, rather than leaving
//! it unlinked to trap later.

use neochumby_wamr::{Import, ImportKind};

use crate::manifest::Manifest;

/// The access words a catalog app is never granted, whatever it asks:
/// `root` (no jail at all), `system` (/proc, /sys and /etc), `terminal`
/// (the Chumby's pseudo-terminals), `logs` (what the Chumby and its apps
/// wrote, which may be anything) and, on the Chumby, `network`, which
/// belongs to its companion on the server.
pub const NEVER_FOR_CATALOG: &[&str] = &["root", "system", "terminal", "logs", "network"];

/// What the host's import `module`.`name` needs granted: `Some("")` for
/// nothing, `Some(word)` for an access word, None for no such import.
pub fn needs(module: &str, name: &str) -> Option<&'static str> {
    match (module, name) {
        ("wasi_snapshot_preview1", n) if crate::host::WASI.iter().any(|f| f.name == n) => Some(""),
        ("neochumby", "fb_size" | "fb_fill" | "fb_blit" | "fb_text") => Some("fb"),
        ("neochumby", "touch_poll") => Some("touch"),
        ("neochumby", "accel") => Some("accel"),
        ("neochumby", n) if crate::host::ALWAYS.contains(&n) => Some(""),
        _ => None,
    }
}

/// Whether `imports` are all granted to the app of `manifest`, and linked;
/// else why not, naming the first that isn't.
pub fn check(imports: &[Import], manifest: &Manifest) -> Result<(), String> {
    for i in imports {
        let what = format!("{}.{}", i.module, i.name);
        if i.kind != ImportKind::Func {
            return Err(format!("it imports {what}, which isn't a function: the host gives only functions"));
        }
        match needs(&i.module, &i.name) {
            None => return Err(format!("it imports {what}, which the host doesn't have")),
            Some(w) if !w.is_empty() && !manifest.grants(w) => {
                return Err(format!("it imports {what}, which needs \"{w}\" in its access"));
            }
            Some(_) if !i.linked => return Err(format!("it imports {what} with the wrong type")),
            Some(_) => {}
        }
    }
    Ok(())
}

/// Whether a catalog app's `manifest` asks for anything a catalog app is
/// never given.
pub fn catalog_allowed(manifest: &Manifest) -> Result<(), String> {
    match manifest.access.iter().find(|w| NEVER_FOR_CATALOG.contains(&w.as_str())) {
        Some(w) => Err(format!("a catalog app is never granted \"{w}\"")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(module: &str, name: &str, linked: bool) -> Import {
        Import { module: module.into(), name: name.into(), kind: ImportKind::Func, linked }
    }

    fn granted(words: &[&str]) -> Manifest {
        Manifest { access: words.iter().map(|w| w.to_string()).collect(), ..Manifest::default() }
    }

    #[test]
    fn imports_need_their_words() {
        let blit = [import("neochumby", "fb_blit", true), import("wasi_snapshot_preview1", "fd_write", true)];
        assert!(check(&blit, &granted(&["fb"])).is_ok());
        let e = check(&blit, &granted(&["touch"])).unwrap_err();
        assert!(e.contains("neochumby.fb_blit") && e.contains("\"fb\""), "{e}");
        assert!(check(&[import("neochumby", "now_ms", true)], &granted(&[])).is_ok());
        assert!(check(&[import("neochumby", "touch_poll", true)], &granted(&["fb"])).is_err());
    }

    #[test]
    fn unknown_unlinked_and_non_functions_are_refused() {
        let none = granted(&["fb", "touch", "speaker", "accel", "network", "system"]);
        for i in [
            import("wasi_snapshot_preview1", "path_open", false),
            import("wasi_snapshot_preview1", "sock_accept", false),
            import("env", "system", false),
            import("neochumby", "fb_blit", false),
        ] {
            assert!(check(std::slice::from_ref(&i), &none).is_err(), "{i:?}");
        }
        let mut mem = import("env", "memory", true);
        mem.kind = ImportKind::Memory;
        assert!(check(&[mem], &none).unwrap_err().contains("isn't a function"));
    }

    #[test]
    fn catalog_apps_never_get_root_system_terminal_or_network() {
        assert!(catalog_allowed(&granted(&["fb", "touch", "speaker", "accel"])).is_ok());
        for w in NEVER_FOR_CATALOG {
            assert!(catalog_allowed(&granted(&["fb", w])).unwrap_err().contains(w));
        }
    }
}
