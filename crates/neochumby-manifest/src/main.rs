//! neochumby-manifest: an app crate's `[package.metadata.neochumby]`, as
//! the `manifest` of key=value lines a Chumby's tree holds for it (read by
//! the session, the apps and neochumbyd). Run on the host by
//! tools/tree.
//!
//!   neochumby-manifest path/to/Cargo.toml
//!
//! Prints the manifest, or fails with what is wrong: a crate not named
//! neochumby-<name>, no metadata, an unknown key, or a value of the wrong
//! kind. Values are strings, except `start`, `button`, `interrupt` and
//! `music`, which are true or false. `neochumbyd` is `required` or `optional`
//! (absent: the app never needs one), and an app with a `companion` must
//! set it: only a neochumbyd runs companions. It is stated, not inferred
//! from `companion`.

use std::process::ExitCode;

use toml::Value;

/// The keys an app may set, as apps.rs reads them on the Chumby.
const KEYS: &[&str] =
    &["title", "exec", "colour", "service", "companion", "neochumbyd", "start", "button", "interrupt", "music"];
/// The keys among them that are true or false:
/// - `start`: the app may be the start app;
/// - `button`: while in front, it gets the top button;
/// - `interrupt`: false if no service may replace it in front;
/// - `music`: a music app, which may be a Chumby's default one. It needs an
///   `exec`: that's what a MUSIC button opens.
const FLAGS: &[&str] = &["start", "button", "interrupt", "music"];

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
        let value = match (FLAGS.contains(key), v) {
            (false, Value::String(s)) => s.clone(),
            (false, _) => return Err(format!("{key}: not a string")),
            (true, Value::Boolean(b)) => b.to_string(),
            (true, _) => return Err(format!("{key}: not true or false")),
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
    // TODO: once `access` is in (#45), refuse a music app without
    // `speaker` (or `root`) too: it couldn't play.
    if meta.get("music").and_then(Value::as_bool) == Some(true) && !meta.contains_key("exec") {
        return Err("music: a music app is opened, so it needs an exec".into());
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
            interrupt = false
            colour = "8c5a28"
            exec = "neochumby-quake"
            button = true
            title = "QUAKE"
            "#,
        )
        .unwrap();
        assert_eq!(m, "title=QUAKE\nexec=neochumby-quake\ncolour=8c5a28\nbutton=true\ninterrupt=false\n");
        let home = "[package]\nname = \"neochumby-home\"\n[package.metadata.neochumby]\nexec = \"neochumby-home\"\nstart = true";
        assert_eq!(manifest(home).unwrap(), "exec=neochumby-home\nstart=true\n");
    }

    #[test]
    fn refuses_what_a_chumby_could_not_read() {
        assert!(manifest("[package]\nname = \"doom\"\n[package.metadata.neochumby]\ntitle = \"D\"").is_err());
        assert!(manifest("[package]\nname = \"neochumby-x\"").is_err());
        assert!(manifest(&wrap("titel = \"X\"")).is_err());
        assert!(manifest(&wrap("title = \"two\\nlines\"")).is_err());
        assert!(manifest(&wrap("title = 3")).is_err());
        assert!(manifest(&wrap("start = \"true\"")).is_err());
        assert!(manifest(&wrap("title = true")).is_err());
        // Gone with proposal 0002: every app is a plain exec app.
        assert!(manifest(&wrap("home = \"neochumby-x\"")).is_err());
        assert!(manifest(&wrap("shells = [\"wx\"]")).is_err());
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
    fn a_music_app_has_an_exec() {
        let m = manifest(&wrap("exec = \"neochumby-x\"\nservice = \"neochumby-x-service\"\nmusic = true")).unwrap();
        assert_eq!(m, "exec=neochumby-x\nservice=neochumby-x-service\nmusic=true\n");
        assert!(manifest(&wrap("service = \"neochumby-x\"\nmusic = true")).unwrap_err().starts_with("music:"));
        assert_eq!(
            manifest(&wrap("service = \"neochumby-x\"\nmusic = false")).unwrap(),
            "service=neochumby-x\nmusic=false\n"
        );
        assert!(manifest(&wrap("exec = \"neochumby-x\"\nmusic = \"yes\"")).is_err());
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
