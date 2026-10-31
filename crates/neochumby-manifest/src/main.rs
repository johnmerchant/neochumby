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
//! `player`, which are true or false, and the lists `access`, what the app
//! reaches beyond its jail ([`ACCESS`]), and `listen`, the TCP ports a
//! `server` program may bind, each written as one comma-separated value.
//! `linux` is the oldest Linux the app runs on, MAJOR.MINOR (as "5.9"):
//! tools/tree leaves it out of a tree for an older one.
//! `neochumbyd` is `required` or `optional` (absent: the app never needs
//! one), and an app with a `companion` must set it: only a neochumbyd runs
//! companions. It is stated, not inferred from `companion`. Nothing in the
//! manifest gives an app neochumbyd's admin socket; only its owner does
//! (`neochumbyd allow <app> pairing`).
//!
//! An app's exec runs in text mode, on a terminal, unless its `access`
//! lists `fb`. In text mode, `font` names its terminal's font ([`FONTS`],
//! or a PSF file in its directory), and `tap` what a tap types while the
//! program hasn't asked for the mouse, written to the manifest with
//! escapes ([`escape`]).

use std::process::ExitCode;

use toml::Value;

/// The keys an app may set, as apps.rs reads them on the Chumby (and
/// neochumbyd reads `companion`, `server` and `listen`).
const KEYS: &[&str] = &[
    "title",
    "exec",
    "color",
    "service",
    "companion",
    "server",
    "listen",
    "neochumbyd",
    "start",
    "button",
    "interrupt",
    "player",
    "access",
    "linux",
    "font",
    "tap",
];

/// The fonts built in, as neochumby::term::font has them: Spleen, in every
/// size.
const FONTS: &[&str] = &["spleen-5x8", "spleen-6x12", "spleen-8x16", "spleen-12x24", "spleen-16x32", "spleen-32x64"];
/// The keys among them that are true or false:
/// - `start`: the app may be the start app;
/// - `button`: while in front, it gets the top button;
/// - `interrupt`: false if no service may replace it in front;
/// - `player`: a player, which may be a Chumby's default one. It needs an
///   `exec` (that's what a MUSIC button opens) and `speaker` (or `root`) in
///   its `access`.
const FLAGS: &[&str] = &["start", "button", "interrupt", "player"];

/// Other spellings a key may have, as (spelling, key): `colour` for
/// `color`. The key's own spelling wins when both are given.
const SPELLINGS: &[(&str, &str)] = &[("colour", "color")];

/// What an app may list in `access`, as the session grants it (jail.rs);
/// without it, an app has only its own directory and a /tmp of its own,
/// and its `exec` a terminal:
/// - `fb`: /dev/fb0, for its `exec` (in front), never its service, in place
///   of a terminal: it draws the screen itself;
/// - `touch`: the touchscreen and its calibration, likewise (in text mode,
///   touches come on stdin without it);
/// - `speaker`: /dev/dsp and the speaker's handover; the mute and the
///   volume are the session's, asked for over the request pipe;
/// - `accel`: the accelerometer;
/// - `system`: /proc, /sys and /etc, as the app's own user sees them;
/// - `network`: the names it needs to look hosts up (/etc/resolv.conf and
///   /etc/hosts); without it, the session ends an app it finds with an
///   internet socket open;
/// - `terminal`: /dev/ptmx and /dev/pts, for a pseudo-terminal
///   (neochumby::term);
/// - `logs`: the Chumby's logs, read-only: the session's, /tmp's *.log
///   and /var/log's files;
/// - `storage`: a directory of its own that lasts across boots
///   (/psp/apps/<name>, NEOCHUMBY_STORAGE), shared by its exec and service,
///   capped at 64 KiB;
/// - `root`: no jail: it runs as root, as the session does. Listed alone,
///   or with `fb` for an app that draws the screen itself (without it, a
///   root app runs in text mode too).
const ACCESS: &[&str] =
    &["fb", "touch", "speaker", "accel", "system", "network", "terminal", "logs", "storage", "root"];

/// What a value may hold: a manifest line is `key=value`, one per line.
fn plain(s: &str) -> bool {
    !s.is_empty() && !s.contains(['\n', '\r', '='])
}

/// `tap`'s keys as a manifest's value: printable ASCII as it is, and \n,
/// \r, \t, \e, \\ and \xHH for the rest (and for `=`), as
/// neochumby::apps::unescape reads them.
fn escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'\n' => out.push_str(r"\n"),
            b'\r' => out.push_str(r"\r"),
            b'\t' => out.push_str(r"\t"),
            0x1b => out.push_str(r"\e"),
            b'\\' => out.push_str(r"\\"),
            b'=' | 0..0x20 | 0x7f.. => out.push_str(&format!(r"\x{b:02x}")),
            _ => out.push(b as char),
        }
    }
    out
}

/// `access`'s list, checked, as one value.
fn access(list: &[Value]) -> Result<String, String> {
    let mut words: Vec<&str> = Vec::new();
    for v in list {
        match v.as_str() {
            Some(w) if words.contains(&w) => return Err(format!("access: {w} twice")),
            Some(w) if ACCESS.contains(&w) => words.push(w),
            Some("screen") => {
                return Err(r#"access: "screen" is "fb" now, for an app that draws the screen itself; without it, the app runs in text mode, on a terminal"#.into());
            }
            _ => return Err(format!("access: {v} is not one of {}", ACCESS.join(", "))),
        }
    }
    if words.contains(&"root") && words.iter().any(|w| !matches!(*w, "root" | "fb")) {
        return Err("access: root is everything already; list it alone, or with fb".into());
    }
    if words.is_empty() {
        return Err("access: an empty list; leave it out".into());
    }
    Ok(words.join(","))
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
        let spelled = SPELLINGS.iter().filter(|(_, k)| k == key).find_map(|(s, _)| meta.get(*s));
        let Some(v) = meta.get(*key).or(spelled) else { continue };
        let value = match (FLAGS.contains(key), v) {
            (_, Value::Array(a)) if *key == "listen" => {
                let ports = a.iter().map(|p| p.as_integer().filter(|p| (1..=65535).contains(p)));
                let ports: Option<Vec<String>> = ports.map(|p| p.map(|p| p.to_string())).collect();
                ports.ok_or("listen: a list of TCP ports, 1 to 65535")?.join(",")
            }
            (_, _) if *key == "listen" => return Err("listen: not a list".into()),
            (false, Value::Array(a)) if *key == "access" => access(a)?,
            (false, _) if *key == "access" => return Err("access: not a list of strings".into()),
            (false, Value::String(s)) if *key == "tap" => escape(s),
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
    let known = |k: &str| KEYS.contains(&k) || SPELLINGS.iter().any(|(s, _)| *s == k);
    if let Some(unknown) = meta.keys().find(|k| !known(k)) {
        return Err(format!("unknown key {unknown} (known: {})", KEYS.join(", ")));
    }
    match meta.get("neochumbyd").and_then(Value::as_str) {
        None | Some("required" | "optional") => {}
        Some(v) => return Err(format!(r#"neochumbyd: {v:?} is not "required" or "optional" (or leave it out)"#)),
    }
    if let Some(v) = meta.get("linux").and_then(Value::as_str) {
        let ok = v.split_once('.').is_some_and(|(a, b)| {
            !a.is_empty() && !b.is_empty() && (a.chars().chain(b.chars())).all(|c| c.is_ascii_digit())
        });
        if !ok {
            return Err(format!(r#"linux: {v:?} is not MAJOR.MINOR (as "5.9")"#));
        }
    }
    if meta.contains_key("listen") && !meta.contains_key("server") {
        return Err("listen is a server program's: set server too".into());
    }
    if meta.get("player").and_then(Value::as_bool) == Some(true) {
        if !meta.contains_key("exec") {
            return Err("player: a player is opened, so it needs an exec".into());
        }
        // Jailed, it plays only through the speaker it lists (or as root).
        let listed = |w: &str| {
            meta.get("access").and_then(Value::as_array).is_some_and(|a| a.iter().any(|v| v.as_str() == Some(w)))
        };
        if !listed("speaker") && !listed("root") {
            return Err(r#"player: a player plays, so its access lists "speaker""#.into());
        }
    }
    let fb = meta.get("access").and_then(Value::as_array).is_some_and(|a| a.iter().any(|v| v.as_str() == Some("fb")));
    for key in ["font", "tap"] {
        if !meta.contains_key(key) {
            continue;
        }
        if fb {
            return Err(format!("{key}: an app with fb draws the screen itself, and has no terminal"));
        }
        if !meta.contains_key("exec") {
            return Err(format!("{key}: only an exec has a terminal"));
        }
    }
    if let Some(f) = meta.get("font").and_then(Value::as_str) {
        let file = (f.ends_with(".psf") || f.ends_with(".psfu")) && !f.contains('/') && !f.starts_with('.');
        if !file && !FONTS.contains(&f) {
            return Err(format!(
                "font: {f} is not one of {}, or a .psf or .psfu file in the app's directory",
                FONTS.join(", ")
            ));
        }
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
            color = "8c5a28"
            exec = "neochumby-quake"
            button = true
            title = "QUAKE"
            "#,
        )
        .unwrap();
        assert_eq!(m, "title=QUAKE\nexec=neochumby-quake\ncolor=8c5a28\nbutton=true\ninterrupt=false\n");
        let home = "[package]\nname = \"neochumby-home\"\n[package.metadata.neochumby]\nexec = \"neochumby-home\"\nstart = true";
        assert_eq!(manifest(home).unwrap(), "exec=neochumby-home\nstart=true\n");
    }

    #[test]
    fn a_server_alone() {
        let m =
            manifest("[package]\nname = \"neochumby-web\"\n[package.metadata.neochumby]\nserver = \"neochumby-web\"\n");
        assert_eq!(m.unwrap(), "server=neochumby-web\n");
        let m = manifest(&wrap("server = \"w\"\nlisten = [8080, 8443]"));
        assert_eq!(m.unwrap(), "server=w\nlisten=8080,8443\n");
        // The owner grants pairing; an app can't take it.
        assert!(manifest(&wrap("server = \"w\"\naccess = [\"pairing\"]")).is_err());
        assert!(manifest(&wrap("server = \"w\"\nlisten = [0]")).is_err());
        assert!(manifest(&wrap("server = \"w\"\nlisten = [\"80\"]")).is_err());
        assert!(manifest(&wrap("exec = \"w\"\nlisten = [80]")).is_err(), "no server");
        assert!(manifest(&wrap("daemon = \"w\"")).is_err());
    }

    #[test]
    fn color_or_colour_and_color_wins() {
        assert_eq!(manifest(&wrap("colour = \"8c5a28\"")).unwrap(), "color=8c5a28\n");
        assert_eq!(manifest(&wrap("color = \"112233\"\ncolour = \"8c5a28\"")).unwrap(), "color=112233\n");
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
    fn linux_is_major_minor() {
        assert_eq!(manifest(&wrap(r#"linux = "5.9""#)).unwrap(), "linux=5.9\n");
        for bad in ["5", "5.", "five.9", "5.9.1"] {
            assert!(manifest(&wrap(&format!("linux = \"{bad}\""))).is_err(), "{bad}");
        }
    }

    #[test]
    fn neochumbyd_is_required_optional_or_absent() {
        assert_eq!(manifest(&wrap(r#"neochumbyd = "required""#)).unwrap(), "neochumbyd=required\n");
        assert_eq!(manifest(&wrap(r#"neochumbyd = "optional""#)).unwrap(), "neochumbyd=optional\n");
        let e = manifest(&wrap(r#"neochumbyd = "yes""#)).unwrap_err();
        assert!(e.contains(r#"not "required" or "optional""#), "{e}");
    }

    #[test]
    fn access_is_a_list_of_what_the_session_grants() {
        assert_eq!(manifest(&wrap(r#"access = ["fb", "touch"]"#)).unwrap(), "access=fb,touch\n");
        assert_eq!(manifest(&wrap(r#"access = ["root"]"#)).unwrap(), "access=root\n");
        assert_eq!(manifest(&wrap(r#"access = ["speaker", "storage"]"#)).unwrap(), "access=speaker,storage\n");
        for bad in [r#"access = "screen""#, r#"access = ["disk"]"#, r#"access = ["fb", "fb"]"#, "access = []"] {
            assert!(manifest(&wrap(bad)).unwrap_err().starts_with("access:"), "{bad}");
        }
        assert_eq!(manifest(&wrap(r#"access = ["root", "fb"]"#)).unwrap(), "access=root,fb\n");
        assert!(manifest(&wrap(r#"access = ["root", "touch"]"#)).unwrap_err().contains("alone"));
        // The old word for fb, refused with what replaced it.
        let e = manifest(&wrap(r#"access = ["screen", "touch"]"#)).unwrap_err();
        assert!(e.contains(r#""fb""#) && e.contains("text mode"), "{e}");
    }

    #[test]
    fn a_text_mode_app_has_a_font_and_a_tap() {
        let m = manifest(&wrap("exec = \"neochumby-x\"\nfont = \"spleen-8x16\"\ntap = \"q\"")).unwrap();
        assert_eq!(m, "exec=neochumby-x\nfont=spleen-8x16\ntap=q\n");
        let own = manifest(&wrap("exec = \"neochumby-x\"\nfont = \"mine.psfu\"\ntap = \"\\n=\\u001b\\\\\"")).unwrap();
        assert_eq!(own, "exec=neochumby-x\nfont=mine.psfu\ntap=\\n\\x3d\\e\\\\\n");
        for bad in ["font = \"spleen-7x9\"", "font = \"../x.psf\"", "font = \"/x.psfu\""] {
            let e = manifest(&wrap(&format!("exec = \"neochumby-x\"\n{bad}"))).unwrap_err();
            assert!(e.starts_with("font:"), "{bad}: {e}");
        }
        // Not without a terminal: with fb, or without an exec.
        let e = manifest(&wrap("exec = \"neochumby-x\"\nfont = \"spleen-8x16\"\naccess = [\"fb\"]")).unwrap_err();
        assert!(e.starts_with("font:") && e.contains("fb"), "{e}");
        assert!(manifest(&wrap("service = \"neochumby-x\"\ntap = \"q\"")).unwrap_err().starts_with("tap:"));
        assert!(manifest(&wrap("exec = \"neochumby-x\"\ntap = \"\"")).unwrap_err().starts_with("tap:"));
    }

    #[test]
    fn a_player_has_an_exec_and_the_speaker() {
        let speaker = r#"access = ["speaker"]"#;
        let m = manifest(&wrap(&format!(
            "exec = \"neochumby-x\"\nservice = \"neochumby-x-service\"\nplayer = true\n{speaker}"
        )))
        .unwrap();
        assert_eq!(m, "exec=neochumby-x\nservice=neochumby-x-service\nplayer=true\naccess=speaker\n");
        let root = manifest(&wrap("exec = \"neochumby-x\"\nplayer = true\naccess = [\"root\"]")).unwrap();
        assert_eq!(root, "exec=neochumby-x\nplayer=true\naccess=root\n");
        // No exec, or nothing to play through.
        assert!(
            manifest(&wrap(&format!("service = \"neochumby-x\"\nplayer = true\n{speaker}")))
                .unwrap_err()
                .starts_with("player:")
        );
        for access in ["", r#"access = ["fb", "touch"]"#] {
            let e = manifest(&wrap(&format!("exec = \"neochumby-x\"\nplayer = true\n{access}"))).unwrap_err();
            assert!(e.starts_with("player:") && e.contains("speaker"), "{e}");
        }
        assert_eq!(
            manifest(&wrap("service = \"neochumby-x\"\nplayer = false")).unwrap(),
            "service=neochumby-x\nplayer=false\n"
        );
        assert!(manifest(&wrap("exec = \"neochumby-x\"\nplayer = \"yes\"")).is_err());
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
