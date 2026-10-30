//! Frontends: an app that shows another app, in its place, through that
//! app's service (neochumby::service). Its manifest names the app it's for
//! (`frontend=airplay`), and the owner approves it with the Chumby's
//! settings (`frontend_airplay=<frontend>`, from neochumbyd or else
//! /psp/neochumby.conf), read as the session starts. Once approved:
//!
//! - **it's opened in the app's place**, however the app is opened (from
//!   an app list, a MUSIC button for a player, or the app's own service);
//! - **it reaches the app's service:** the service is told the frontend's
//!   uid (NEOCHUMBY_CLIENTS) and takes it, and the frontend the service's
//!   uid (NEOCHUMBY_FRONTEND), to check it's talking to the real one.
//!
//! An app has at most one approved frontend. One that isn't here, has no
//! exec, can't run here, or whose manifest names another app, is refused,
//! with the reason logged, and the app opens as itself.

use std::sync::OnceLock;

use neochumby::apps::App;
use neochumby::service;

use crate::jail;

/// The approvals, as (app, frontend): set once as the session starts.
static APPROVED: OnceLock<Vec<Approval>> = OnceLock::new();

/// `frontend`, if it may be `app`'s frontend here; else why not.
pub fn eligible<'a>(apps: &'a [App], app: &str, frontend: &str) -> Result<&'a App, &'static str> {
    let f = apps.iter().find(|a| a.name == frontend).ok_or("no such app here")?;
    if f.frontend.as_deref() != Some(app) {
        Err("its manifest doesn't say it's this app's frontend")
    } else if f.exec.is_none() {
        Err("it has no exec")
    } else if !f.runnable() {
        Err("it needs a paired neochumbyd")
    } else if !apps.iter().any(|a| a.name == app) {
        Err("the app it's for isn't here")
    } else {
        Ok(f)
    }
}

/// The approvals among `apps`, from `setting` (a setting's value by key,
/// where it came from): each app that a frontend names, whose setting
/// names that frontend and which may be it.
pub fn resolve(apps: &[App], setting: impl Fn(&str) -> Option<(&'static str, String)>) -> Vec<(String, String)> {
    let mut wanted: Vec<&str> = apps.iter().filter_map(|a| a.frontend.as_deref()).collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut out = Vec::new();
    for app in wanted {
        let key = neochumby_proto::settings::frontend(app);
        let Some((from, name)) = setting(&key) else { continue };
        match eligible(apps, app, &name) {
            Ok(f) => {
                eprintln!("neochumby: {name} is {app}'s frontend ({key}, {from})");
                out.push((app.to_string(), f.name.clone()));
            }
            Err(why) => eprintln!("neochumby: {key}={name} ({from}) can't be: {why}"),
        }
    }
    out
}

/// One approval, with each side's uid while jails are on (jail::uid_of).
struct Approval {
    app: String,
    frontend: String,
    app_uid: Option<u32>,
    frontend_uid: Option<u32>,
}

/// Keeps the approvals for the session's run. After jail::init, for the
/// uids.
pub fn init(apps: &[App], approved: Vec<(String, String)>) {
    let all = approved
        .into_iter()
        .map(|(app, frontend)| Approval {
            app_uid: jail::uid_of(apps, &app),
            frontend_uid: jail::uid_of(apps, &frontend),
            app,
            frontend,
        })
        .collect();
    let _ = APPROVED.set(all);
}

fn approved() -> &'static [Approval] {
    APPROVED.get().map(Vec::as_slice).unwrap_or_default()
}

/// The frontend opened in `app`'s place, if one is approved.
pub fn instead_of(app: &str) -> Option<&'static str> {
    approved().iter().find(|a| a.app == app).map(|a| a.frontend.as_str())
}

/// What the session tells `app`'s exec and service, and an approved
/// frontend's, beside the rest (child.rs): the uids that may connect to
/// it (neochumby::service::CLIENTS_ENV), and the app it's a frontend for
/// (FRONTEND_ENV).
pub fn env(app: &str) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let clients: Vec<String> =
        approved().iter().filter(|a| a.app == app).filter_map(|a| a.frontend_uid).map(|u| u.to_string()).collect();
    if !clients.is_empty() {
        env.push((service::CLIENTS_ENV.into(), clients.join(",")));
    }
    if let Some(a) = approved().iter().find(|a| a.frontend == app) {
        let v = match a.app_uid {
            Some(uid) => format!("{}:{uid}", a.app),
            None => a.app.clone(),
        };
        env.push((service::FRONTEND_ENV.into(), v));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::start::tests::app;

    fn frontend(name: &str, of: &str) -> App {
        App { frontend: Some(of.into()), ..app(name, false) }
    }

    #[test]
    fn only_what_the_owner_approved_and_the_manifest_names() {
        let mut apps = vec![app("airplay", false), frontend("nowplaying", "airplay"), frontend("radio", "somafm")];
        let settings = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(key, _)| *key == k).map(|(_, v)| ("settings", v.to_string()))
        };
        // Approved.
        let a = resolve(&apps, settings(&[("frontend_airplay", "nowplaying")]));
        assert_eq!(a, [("airplay".to_string(), "nowplaying".to_string())]);
        // Not approved: nothing.
        assert!(resolve(&apps, settings(&[])).is_empty());
        // Named for an app it doesn't say it's for, or one that isn't here.
        assert!(resolve(&apps, settings(&[("frontend_airplay", "radio")])).is_empty());
        assert_eq!(
            eligible(&apps, "airplay", "radio").err(),
            Some("its manifest doesn't say it's this app's frontend")
        );
        assert_eq!(eligible(&apps, "somafm", "radio").err(), Some("the app it's for isn't here"));
        assert_eq!(eligible(&apps, "airplay", "gone").err(), Some("no such app here"));
        apps[1].exec = None;
        assert!(resolve(&apps, settings(&[("frontend_airplay", "nowplaying")])).is_empty());
    }
}
