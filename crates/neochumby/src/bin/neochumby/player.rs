//! The default player: what a start app's MUSIC button opens, and what
//! its MUTE button pauses and resumes.
//!
//! It's named by `player=<app>` in this Chumby's settings from its
//! neochumbyd, else in the stick's neochumby.conf, as `start` is (start.rs). Only
//! an app whose manifest says `player = true`, and that can be opened here,
//! may be it. A name that isn't one is refused, with the reason logged.
//! Then, or when nothing names one, it's [`DEFAULT`] if that's a player
//! here, else the first player by name, else there's none.
//!
//! The session tells everything it runs which app it is
//! (NEOCHUMBY_PLAYER), and passes the start app's `player` requests on to
//! its service as `pause` and `resume` (run.rs).

use neochumby::apps::App;

/// The player a Chumby uses when its settings name none: SomaFM, which
/// is in this repository, as `home` is.
pub const DEFAULT: &str = "somafm";

/// `name`, if it may be the default player here; else why not.
pub fn eligible<'a>(apps: &'a [App], name: &str) -> Result<&'a App, &'static str> {
    let app = apps.iter().find(|a| a.name == name).ok_or("no such app here")?;
    if !app.player {
        Err("its manifest doesn't say player = true")
    } else if app.exec.is_none() {
        Err("it has no exec")
    } else if !app.runnable() {
        Err("it needs a paired neochumbyd")
    } else {
        Ok(app)
    }
}

/// The default player: `choice` (where the name came from, and the
/// name) if it may be one, else [`DEFAULT`], else the first player in
/// `apps` (the tree's, in name order). None if there's no player here.
pub fn choose<'a>(apps: &'a [App], choice: Option<(&str, &str)>) -> Option<&'a App> {
    if let Some((from, name)) = choice {
        match eligible(apps, name) {
            Ok(app) => return Some(app),
            Err(why) => eprintln!("neochumby: player={name} ({from}) can't be the player: {why}"),
        }
    }
    eligible(apps, DEFAULT).ok().or_else(|| apps.iter().find(|a| eligible(apps, &a.name).is_ok()))
}

#[cfg(test)]
mod tests {
    use neochumby::apps::Needs;

    use super::*;
    use crate::start::tests::app;

    fn player(name: &str) -> App {
        App { player: true, ..app(name, false) }
    }

    #[test]
    fn the_setting_then_somafm_then_the_first_by_name() {
        let mut apps = vec![app("doom", false), player("jazz"), player("radio"), player("somafm")];
        let name = |a: Option<&App>| a.map(|a| a.name.clone());

        assert_eq!(name(choose(&apps, Some(("settings", "radio")))), Some("radio".into()));
        assert_eq!(name(choose(&apps, None)), Some("somafm".into()));
        // Refused: not a player, or not here; the default instead.
        assert_eq!(eligible(&apps, "doom").err(), Some("its manifest doesn't say player = true"));
        assert_eq!(eligible(&apps, "gone").err(), Some("no such app here"));
        assert_eq!(name(choose(&apps, Some(("settings", "doom")))), Some("somafm".into()));
        // Without somafm: the first player by name.
        apps[3].player = false;
        assert_eq!(name(choose(&apps, Some(("settings", "gone")))), Some("jazz".into()));
        // One that can't be opened, here or at all, is passed over.
        apps[1].needs = Needs::Required;
        assert_eq!(eligible(&apps, "jazz").err(), Some("it needs a paired neochumbyd"));
        apps[2].exec = None;
        assert_eq!(eligible(&apps, "radio").err(), Some("it has no exec"));
        assert_eq!(name(choose(&apps, None)), None);
    }
}
