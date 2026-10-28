//! neochumby's macros, re-exported by the `neochumby` crate: use them from
//! there.
//!
//! [`companion!`] names the compiling app's companion (its server half,
//! which neochumbyd runs), checked against the app's own Cargo.toml as it
//! compiles. An app whose `[package.metadata.neochumby]` has no
//! `companion`, or does not say it needs a neochumbyd, does not compile:
//! there would be nothing for it to open.
//!
//! ```compile_fail
//! // This crate's Cargo.toml declares no companion.
//! let _ = neochumby_macros::companion!();
//! ```

use proc_macro::{Literal, TokenStream};
use toml::Value;

/// The compiling app's companion, as `neochumby::companion::Companion`, for
/// `neochumby::companion::open`. Takes no arguments: the app is the crate
/// being compiled (`neochumby-<name>`, known to neochumbyd as `<name>`).
#[proc_macro]
pub fn companion(input: TokenStream) -> TokenStream {
    if !input.is_empty() {
        return error("companion!() takes no arguments: it opens the compiling app's own companion");
    }
    let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") else {
        return error("companion!(): no CARGO_MANIFEST_DIR; build the app with cargo");
    };
    let path = format!("{dir}/Cargo.toml");
    let app = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}")).and_then(|s| app(&s));
    match app {
        // The include makes Cargo.toml one of the app's sources, so that
        // changing it compiles the app, and this check, again.
        Ok(app) => format!(
            "{{ const _: &[u8] = ::core::include_bytes!({}); ::neochumby::companion::Companion::__checked({}) }}",
            Literal::string(&path),
            Literal::string(&app)
        )
        .parse()
        .expect("the expansion is Rust"),
        Err(e) => error(&format!("companion!(): {e}")),
    }
}

fn error(msg: &str) -> TokenStream {
    format!("::core::compile_error!({})", Literal::string(msg)).parse().expect("compile_error! is Rust")
}

/// What neochumbyd accepts as an app's name (neochumby_proto::valid_name).
fn valid_name(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The app's name, as neochumbyd knows it, if `cargo_toml` declares a
/// companion for it to open; else what is missing.
fn app(cargo_toml: &str) -> Result<String, String> {
    let doc: Value = cargo_toml.parse().map_err(|e| format!("not TOML: {e}"))?;
    let package = doc.get("package").ok_or("no [package]")?;
    let crate_name = package.get("name").and_then(Value::as_str).ok_or("no package name")?;
    let name = crate_name
        .strip_prefix("neochumby-")
        .filter(|n| valid_name(n))
        .ok_or(format!("{crate_name}: an app's crate is named neochumby-<name>, <name> in [a-z0-9_-]"))?;
    let meta = package
        .get("metadata")
        .and_then(|m| m.get("neochumby"))
        .and_then(Value::as_table)
        .ok_or("no [package.metadata.neochumby]: this is not an app, and has no companion")?;
    match meta.get("companion").and_then(Value::as_str) {
        Some(c) if !c.is_empty() => {}
        _ => {
            return Err(format!(
                "{crate_name} has no companion to open: [package.metadata.neochumby] sets no `companion`"
            ));
        }
    }
    match meta.get("neochumbyd").and_then(Value::as_str) {
        Some("required" | "optional") => Ok(name.to_string()),
        _ => Err(format!(
            "{crate_name} has a companion, which only a neochumbyd runs: \
             [package.metadata.neochumby] needs neochumbyd = \"required\" or \"optional\""
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::app;

    fn toml(name: &str, meta: &str) -> String {
        format!("[package]\nname = \"{name}\"\n[package.metadata.neochumby]\n{meta}\n")
    }

    #[test]
    fn names_an_app_with_a_companion() {
        let meta = "companion = \"neochumby-x-companion\"\nneochumbyd = \"required\"";
        assert_eq!(app(&toml("neochumby-airplay", meta)).unwrap(), "airplay");
        let meta = "companion = \"neochumby-x-companion\"\nneochumbyd = \"optional\"";
        assert_eq!(app(&toml("neochumby-wx", meta)).unwrap(), "wx");
    }

    #[test]
    fn refuses_an_app_without_one() {
        let e = app(&toml("neochumby-doom", "exec = \"neochumby-doom\"\nneochumbyd = \"optional\"")).unwrap_err();
        assert!(e.contains("no companion"), "{e}");
        assert!(app(&toml("neochumby-doom", "companion = \"\"\nneochumbyd = \"required\"")).is_err());
        assert!(app("[package]\nname = \"neochumby-doom\"").is_err());
    }

    #[test]
    fn refuses_a_companion_without_neochumbyd() {
        let c = "companion = \"neochumby-x-companion\"";
        let e = app(&toml("neochumby-x", c)).unwrap_err();
        assert!(e.contains("neochumbyd = \"required\" or \"optional\""), "{e}");
        assert!(app(&toml("neochumby-x", &format!("{c}\nneochumbyd = \"always\""))).is_err());
    }

    #[test]
    fn refuses_a_name_neochumbyd_would_not_open() {
        let meta = "companion = \"c\"\nneochumbyd = \"required\"";
        assert!(app(&toml("airplay", meta)).is_err());
        assert!(app(&toml("neochumby-", meta)).is_err());
        assert!(app(&toml("neochumby-Air.Play", meta)).is_err());
    }
}
