//! Which apps in the tree may be a Chumby's start app: those whose
//! manifest says `start=true` and has an `exec`, and doesn't take the top
//! button (`button=true`). A Chumby refuses any
//! other name as its `start` setting and opens `home` instead, so the
//! settings suggest these (`neochumbyd set`, `neochumbyd pairing`).

use std::path::Path;

use neochumby_proto::valid_name;

/// The app a Chumby opens when its `start` names no start app.
pub const HOME: &str = "home";

/// The start apps in `tree`, in name order.
pub fn start_apps(tree: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(tree.join("apps"))
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.retain(|n| valid_name(n) && is_start_app(&tree.join("apps").join(n)));
    names.sort();
    names
}

fn is_start_app(dir: &Path) -> bool {
    let Ok(m) = std::fs::read_to_string(dir.join("manifest")) else { return false };
    let field =
        |key: &str| m.lines().filter_map(|l| l.split_once('=')).find(|(k, _)| k.trim() == key).map(|(_, v)| v.trim());
    field("start") == Some("true") && field("exec").is_some_and(|e| !e.is_empty()) && field("button") != Some("true")
}

/// The start apps, as a line for a person.
pub fn hint(apps: &[String]) -> String {
    if apps.is_empty() {
        format!("no start apps in the tree (start = true in a manifest); a Chumby opens {HOME}")
    } else {
        format!("start apps in the tree: {}", apps.join(", "))
    }
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
}
