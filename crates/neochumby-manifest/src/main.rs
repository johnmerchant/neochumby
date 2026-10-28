//! neochumby-manifest: an app crate's `[package.metadata.neochumby]`, as
//! the `manifest` of key=value lines a Chumby's tree holds for it (read by
//! the session, the home screen and neochumbyd). Run on the host by
//! tools/tree.
//!
//!   neochumby-manifest path/to/Cargo.toml
//!
//! Prints the manifest, or fails with what is wrong: a crate not named
//! neochumby-<name>, no metadata, an unknown key, or a value that is not a
//! plain word. Values are strings; `shells` may also be a list, written
//! space-separated. `neochumbyd` is `required` or `optional` (absent: the
//! app never needs one), and an app with a `companion` must set it: only a
//! neochumbyd runs companions. It is stated, not inferred from `companion`.

use std::process::ExitCode;

use toml::Value;

/// The keys an app may set, as apps.rs reads them on the Chumby.
const KEYS: &[&str] = &["title", "exec", "colour", "service", "companion", "neochumbyd", "home", "shells"];

/// What a value may hold: a manifest line is `key=value`, one per line.
fn plain(s: &str) -> bool {
    !s.is_empty() && !s.contains(['\n', '\r', '='])
}

fn manifest(cargo_toml: &str) -> Result<String, String> {
    let doc: Value = cargo_toml.parse().map_err(|e| format!("not TOML: {e}"))?;
    let package = doc.get("package").ok_or("no [package]")?;
    let name = package.get("name").and_then(Value::as_str).ok_or("no package name")?;
    if !name.starts_with("neochumby-") {
        return Err(format!("{name}: an app's crate is named neochumby-<name>"));
    }
    let meta = package
        .get("metadata")
        .and_then(|m| m.get("neochumby"))
        .and_then(Value::as_table)
        .ok_or("no [package.metadata.neochumby]")?;

    let mut out = String::new();
    for key in KEYS {
        let Some(v) = meta.get(*key) else { continue };
        let value = match (key, v) {
            (_, Value::String(s)) => s.clone(),
            (&"shells", Value::Array(a)) => a
                .iter()
                .map(|s| s.as_str().map(str::to_string).ok_or(format!("shells: {s} is not a string")))
                .collect::<Result<Vec<_>, _>>()?
                .join(" "),
            _ => return Err(format!("{key}: not a string")),
        };
        if !plain(&value) {
            return Err(format!("{key}: {value:?} is not a plain value"));
        }
        out.push_str(&format!("{key}={value}\n"));
    }
    if let Some(unknown) = meta.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown key {unknown} (known: {})", KEYS.join(", ")));
    }
    match meta.get("neochumbyd").and_then(Value::as_str) {
        None | Some("required" | "optional") => {}
        Some(v) => return Err(format!(r#"neochumbyd: {v:?} is not "required" or "optional" (or leave it out)"#)),
    }
    if meta.contains_key("companion") && !meta.contains_key("neochumbyd") {
        return Err(r#"companion: only a neochumbyd runs it; set neochumbyd = "required" or "optional""#.into());
    }
    if out.is_empty() {
        return Err("[package.metadata.neochumby] sets nothing".into());
    }
    Ok(out)
}

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: neochumby-manifest path/to/Cargo.toml");
        return ExitCode::from(2);
    };
    match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| manifest(&s)) {
        Ok(m) => {
            print!("{m}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("neochumby-manifest: {path}: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::manifest;

    #[test]
    fn writes_the_keys_in_order() {
        let m = manifest(
            r#"
            [package]
            name = "neochumby-quake"
            [package.metadata.neochumby]
            colour = "8c5a28"
            exec = "neochumby-quake"
            title = "QUAKE"
            shells = ["home", "wx"]
            "#,
        )
        .unwrap();
        assert_eq!(m, "title=QUAKE\nexec=neochumby-quake\ncolour=8c5a28\nshells=home wx\n");
    }

    #[test]
    fn refuses_what_a_chumby_could_not_read() {
        assert!(manifest("[package]\nname = \"doom\"\n[package.metadata.neochumby]\ntitle = \"D\"").is_err());
        assert!(manifest("[package]\nname = \"neochumby-x\"").is_err());
        assert!(manifest(&wrap("titel = \"X\"")).is_err());
        assert!(manifest(&wrap("title = \"two\\nlines\"")).is_err());
        assert!(manifest(&wrap("title = 3")).is_err());
    }

    fn wrap(meta: &str) -> String {
        format!("[package]\nname = \"neochumby-x\"\n[package.metadata.neochumby]\n{meta}\n")
    }

    #[test]
    fn neochumbyd_is_required_optional_or_absent() {
        assert_eq!(manifest(&wrap(r#"neochumbyd = "required""#)).unwrap(), "neochumbyd=required\n");
        assert_eq!(manifest(&wrap(r#"neochumbyd = "optional""#)).unwrap(), "neochumbyd=optional\n");
        let e = manifest(&wrap(r#"neochumbyd = "yes""#)).unwrap_err();
        assert!(e.contains(r#"not "required" or "optional""#), "{e}");
    }

    #[test]
    fn a_companion_needs_neochumbyd() {
        let c = "service = \"neochumby-x\"\ncompanion = \"neochumby-x-companion\"";
        let e = manifest(&wrap(c)).unwrap_err();
        assert!(e.starts_with("companion:"), "{e}");
        for needs in ["required", "optional"] {
            let m = manifest(&wrap(&format!("{c}\nneochumbyd = \"{needs}\""))).unwrap();
            assert!(m.ends_with(&format!("companion=neochumby-x-companion\nneochumbyd={needs}\n")), "{m}");
        }
    }
}
