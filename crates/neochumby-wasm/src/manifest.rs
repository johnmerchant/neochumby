//! What the runner reads of its app's `manifest`, in the app's directory
//! beside the module (neochumby::apps has the whole format).

use std::path::{Path, PathBuf};

/// The app's manifest, as far as the runner goes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    /// Its `access`, word by word.
    pub access: Vec<String>,
    /// `aot=`: the compiled module beside the bytecode, if the tree has one.
    pub aot: Option<PathBuf>,
    /// `aot-key=`: what that was compiled from and for ([`crate::aot::key`]).
    pub aot_key: Option<String>,
    /// `aot-sha256=`: the compiled module's own hash, as its compiler wrote it.
    pub aot_sha256: Option<String>,
    /// `source=catalog`: installed from the catalog, so untrusted.
    pub catalog: bool,
    /// It has a companion.
    pub companion: bool,
}

impl Manifest {
    /// From the manifest's text, file names relative to `dir`.
    pub fn parse(text: &str, dir: &Path) -> Manifest {
        let field = |k: &str| {
            text.lines()
                .filter_map(|l| l.split_once('='))
                .find(|(key, _)| key.trim() == k)
                .map(|(_, v)| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        Manifest {
            access: field("access")
                .map(|a| a.split(',').map(|w| w.trim().to_string()).filter(|w| !w.is_empty()).collect())
                .unwrap_or_default(),
            aot: field("aot").map(|f| dir.join(f)),
            aot_key: field("aot-key"),
            aot_sha256: field("aot-sha256"),
            catalog: field("source").as_deref() == Some("catalog"),
            companion: field("companion").is_some(),
        }
    }

    /// Whether `access` lists `word`.
    /// `screen` is `fb`'s name in trees from before text mode, which named
    /// the framebuffer so.
    pub fn grants(&self, word: &str) -> bool {
        self.access.iter().any(|w| w == word || (word == "fb" && w == "screen"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_runner_needs() {
        let m = Manifest::parse(
            "title=CLOCK\nwasm=clock.wasm\naot=clock.aot\naot-key=k\naot-sha256=ab\naccess=fb, touch\nsource=catalog\n",
            Path::new("/t/apps/clock"),
        );
        assert_eq!(m.access, ["fb", "touch"]);
        assert_eq!(m.aot.as_deref(), Some(Path::new("/t/apps/clock/clock.aot")));
        assert_eq!((m.aot_key.as_deref(), m.aot_sha256.as_deref()), (Some("k"), Some("ab")));
        assert!(m.catalog && !m.companion && m.grants("touch") && !m.grants("speaker"));
        assert_eq!(Manifest::parse("exec=x\n", Path::new("/")), Manifest::default());
    }
}
