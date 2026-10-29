//! neochumby-manifest: an app crate's `[package.metadata.neochumby]`, as
//! the `manifest` of key=value lines a Chumby's tree holds for it (read by
//! the session, the apps and neochumbyd). Run on the host by
//! tools/tree.
//!
//!   neochumby-manifest path/to/Cargo.toml
//!
//! Prints the manifest, or fails with what is wrong: a crate not named
//! neochumby-<name>, no metadata, an unknown key, or a value of the wrong
//! kind. Values are strings, except `start`, `button` and `interrupt`,
//! which are true or false, and `listen`, a list of TCP ports, written
//! space-separated. `neochumbyd` is
//! `required` or `optional` (absent: the app never needs one), and an app
//! with a `companion` must set it: only a neochumbyd runs companions. It is
//! stated, not inferred from `companion`. `listen` is for a `server`
//! program: the TCP ports it may bind. Nothing in the manifest gives an
//! app access to neochumbyd's admin socket; only its owner does
//! (`neochumbyd allow <app> pairing`).

use std::process::ExitCode;

use toml::Value;

/// The keys an app may set, as apps.rs reads them on the Chumby (and
/// neochumbyd reads `companion`, `server` and `listen`).
const KEYS: &[&str] = &[
    "title",
    "exec",
    "colour",
    "service",
    "companion",
    "server",
    "listen",
    "neochumbyd",
    "start",
    "button",
    "interrupt",
];
/// The keys among them that are true or false:
/// - `start`: the app may be the start app;
/// - `button`: while in front, it gets the top button;
/// - `interrupt`: false if no service may replace it in front.
const FLAGS: &[&str] = &["start", "button", "interrupt"];

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
            (_, Value::Array(a)) if *key == "listen" => {
                let ports = a.iter().map(|p| p.as_integer().filter(|p| (1..=65535).contains(p)));
                let ports: Option<Vec<String>> = ports.map(|p| p.map(|p| p.to_string())).collect();
                ports.ok_or("listen: a list of TCP ports, 1 to 65535")?.join(" ")
            }
            (_, _) if *key == "listen" => return Err("listen: not a list".into()),
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
    if meta.contains_key("listen") && !meta.contains_key("server") {
        return Err("listen is a server program's: set server too".into());
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
    fn a_server_alone() {
        let m =
            manifest("[package]\nname = \"neochumby-web\"\n[package.metadata.neochumby]\nserver = \"neochumby-web\"\n");
        assert_eq!(m.unwrap(), "server=neochumby-web\n");
        let m = manifest(&wrap("server = \"w\"\nlisten = [8080, 8443]"));
        assert_eq!(m.unwrap(), "server=w\nlisten=8080 8443\n");
        // The owner grants pairing; an app can't take it.
        assert!(manifest(&wrap("server = \"w\"\naccess = [\"pairing\"]")).is_err());
        assert!(manifest(&wrap("server = \"w\"\nlisten = [0]")).is_err());
        assert!(manifest(&wrap("server = \"w\"\nlisten = [\"80\"]")).is_err());
        assert!(manifest(&wrap("exec = \"w\"\nlisten = [80]")).is_err(), "no server");
        assert!(manifest(&wrap("daemon = \"w\"")).is_err());
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
