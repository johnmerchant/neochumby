//! Which apps in the tree may be a Chumby's start app: those whose
//! manifest says `start=true` and has an `exec`, and doesn't take the top
//! button (`button=true`). A Chumby refuses any
//! other name as its `start` setting and opens `home` instead, so the
//! settings suggest these (`neochumbyd set`, `neochumbyd pairing`).
//!
//! Likewise its music apps, for the `music` setting: those whose manifest
//! says `music=true` and has an `exec`. A Chumby refuses any other name,
//! and uses [`DEFAULT_MUSIC`] if it's one, else the first by name.

use std::path::Path;

use neochumby_proto::valid_name;

/// The app a Chumby opens when its `start` names no start app.
pub const HOME: &str = "home";
/// The music app a Chumby uses when its `music` names none, if it's here.
pub const DEFAULT_MUSIC: &str = "somafm";

/// The start apps in `tree`, in name order.
pub fn start_apps(tree: &Path) -> Vec<String> {
    apps_where(tree, |m| {
        field(m, "start") == Some("true")
            && field(m, "exec").is_some_and(|e| !e.is_empty())
            && field(m, "button") != Some("true")
    })
}

/// The music apps in `tree`, in name order.
pub fn music_apps(tree: &Path) -> Vec<String> {
    apps_where(tree, |m| field(m, "music") == Some("true") && field(m, "exec").is_some_and(|e| !e.is_empty()))
}

/// The apps in `tree` whose manifest passes `test`, in name order.
fn apps_where(tree: &Path, test: impl Fn(&str) -> bool) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(tree.join("apps"))
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.retain(|n| {
        valid_name(n) && std::fs::read_to_string(tree.join("apps").join(n).join("manifest")).is_ok_and(|m| test(&m))
    });
    names.sort();
    names
}

/// `key`'s value in a manifest's key=value lines.
fn field<'a>(manifest: &'a str, key: &str) -> Option<&'a str> {
    manifest.lines().filter_map(|l| l.split_once('=')).find(|(k, _)| k.trim() == key).map(|(_, v)| v.trim())
}

/// The start apps, as a line for a person.
pub fn hint(apps: &[String]) -> String {
    if apps.is_empty() {
        format!("no start apps in the tree (start = true in a manifest); a Chumby opens {HOME}")
    } else {
        format!("start apps in the tree: {}", apps.join(", "))
    }
}

/// The music app a Chumby uses when its `music` names none of `apps`.
pub fn default_music(apps: &[String]) -> Option<&str> {
    apps.iter().find(|a| *a == DEFAULT_MUSIC).or(apps.first()).map(String::as_str)
}

/// The music apps, as a line for a person.
pub fn music_hint(apps: &[String]) -> String {
    match default_music(apps) {
        None => "no music apps in the tree (music = true in a manifest); MUSIC buttons do nothing".into(),
        Some(d) => format!("music apps in the tree: {} (unless music= names one, {d})", apps.join(", ")),
    }
}

/// What to tell the owner about Chumby `name`'s `music` setting: None if
/// it's unset, or names a music app here.
pub fn music_note(name: &str, music: Option<&str>, apps: &[String]) -> Option<String> {
    let m = music.filter(|m| !apps.iter().any(|a| a == m))?;
    let uses = default_music(apps).map_or("none".to_string(), |d| format!("{d} instead"));
    Some(format!(
        "music={m} isn't a music app in the tree, so it uses {uses}; to choose one: neochumbyd set {name} music=<app>"
    ))
}

/// What to tell the owner about Chumby `name`'s start app, given its
/// `start` setting: None if it names a start app here.
pub fn start_note(name: &str, start: Option<&str>, apps: &[String]) -> Option<String> {
    let others: Vec<&str> = apps.iter().map(String::as_str).filter(|a| *a != HOME).collect();
    let suggest = match others[..] {
        [] => String::new(),
        [one] => format!("; to start with it: neochumbyd set {name} start={one}"),
        _ => format!("; to start with one: neochumbyd set {name} start=<{}>", others.join("|")),
    };
    match start {
        None => Some(format!("start not set, so it opens {HOME}{suggest}")),
        Some(s) if !apps.iter().any(|a| a == s) => {
            Some(format!("start={s} isn't a start app in the tree, so it opens {HOME}{suggest}"))
        }
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_suggests_start_apps() {
        let tree = std::env::temp_dir().join(format!("neochumbyd-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tree);
        for (app, manifest) in [
            ("home", "exec=neochumby-home\nstart=true\n"),
            ("wx", "title=WX\nexec=neochumby-wx\nstart=true\n"),
            ("doom", "exec=neochumby-doom\nbutton=true\n"),
            // Not start apps: says false, or has nothing to run.
            ("quake", "exec=neochumby-quake\nstart=false\n"),
            ("airplay", "service=neochumby-airplay\nstart=true\n"),
            // Nor one that takes the button, which then couldn't bring it back.
            ("arcade", "exec=neochumby-arcade\nstart=true\nbutton=true\n"),
        ] {
            std::fs::create_dir_all(tree.join("apps").join(app)).unwrap();
            std::fs::write(tree.join("apps").join(app).join("manifest"), manifest).unwrap();
        }
        let apps = start_apps(&tree);
        assert_eq!(apps, ["home", "wx"]);
        assert_eq!(hint(&apps), "start apps in the tree: home, wx");
        assert_eq!(hint(&[]), "no start apps in the tree (start = true in a manifest); a Chumby opens home");

        assert_eq!(start_note("kitchen", Some("wx"), &apps), None);
        assert_eq!(start_note("kitchen", Some("home"), &apps), None);
        assert_eq!(
            start_note("kitchen", None, &apps).unwrap(),
            "start not set, so it opens home; to start with it: neochumbyd set kitchen start=wx"
        );
        assert_eq!(
            start_note("kitchen", Some("doom"), &apps).unwrap(),
            "start=doom isn't a start app in the tree, so it opens home; to start with it: neochumbyd set kitchen start=wx"
        );
        let more = vec!["classic".to_string(), "home".into(), "wx".into()];
        assert_eq!(
            start_note("kitchen", None, &more).unwrap(),
            "start not set, so it opens home; to start with one: neochumbyd set kitchen start=<classic|wx>"
        );
        assert_eq!(start_note("kitchen", None, &["home".into()]).unwrap(), "start not set, so it opens home");
        let _ = std::fs::remove_dir_all(&tree);
    }

    #[test]
    fn finds_music_apps_and_the_default() {
        let tree = std::env::temp_dir().join(format!("neochumbyd-music-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tree);
        for (app, manifest) in [
            ("somafm", "exec=neochumby-somafm\nservice=neochumby-somafm-service\nmusic=true\n"),
            ("jazz", "exec=neochumby-jazz\nmusic=true\n"),
            // Not music apps: nothing to open, or not saying so.
            ("airplay", "service=neochumby-airplay\nmusic=true\n"),
            ("doom", "exec=neochumby-doom\nmusic=false\n"),
        ] {
            std::fs::create_dir_all(tree.join("apps").join(app)).unwrap();
            std::fs::write(tree.join("apps").join(app).join("manifest"), manifest).unwrap();
        }
        let apps = music_apps(&tree);
        assert_eq!(apps, ["jazz", "somafm"]);
        assert_eq!(default_music(&apps), Some("somafm"));
        assert_eq!(default_music(&apps[..1]), Some("jazz"));
        assert_eq!(default_music(&[]), None);
        assert_eq!(music_hint(&apps), "music apps in the tree: jazz, somafm (unless music= names one, somafm)");
        assert!(music_hint(&[]).starts_with("no music apps"));

        assert_eq!(music_note("kitchen", None, &apps), None);
        assert_eq!(music_note("kitchen", Some("jazz"), &apps), None);
        assert_eq!(
            music_note("kitchen", Some("doom"), &apps).unwrap(),
            "music=doom isn't a music app in the tree, so it uses somafm instead; to choose one: neochumbyd set kitchen music=<app>"
        );
        assert!(music_note("kitchen", Some("doom"), &[]).unwrap().contains("so it uses none;"));
        let _ = std::fs::remove_dir_all(&tree);
    }
}
