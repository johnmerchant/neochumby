//! Keeping catalog apps up to date, and the catalog's log.
//!
//! After each refresh (`serve`'s daily one, or `neochumbyd catalog
//! refresh`), each catalog app in the tree whose listing has a newer
//! version is updated to it ([`auto_update`]) when that version asks for
//! nothing the installed one wasn't granted: no access word and no
//! companion host it didn't have. One that asks for more waits for the
//! owner, who approves it as an install (`neochumbyd install
//! crate:NAME@VERSION`, or GET APPS on a Chumby); it is logged once. Every
//! update is checked as an install is (install.rs): pinned, its checksums,
//! its policy, and its label shown in the log.
//!
//! Automatic updates are on unless the owner turns them off: `neochumbyd
//! catalog auto-update off` (or the admin socket's `auto-update off`),
//! kept in `<state>/catalog-auto-update`. Then an update asking for
//! nothing new waits for the owner too.
//!
//! Every install, update and removal, whoever asked for it, goes in the
//! catalog's log, `<state>/catalog.log` (`neochumbyd catalog log`), as well
//! as neochumbyd's own.

use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::discover;
use super::http::Http;
use super::install::{self, Spec, prepare};
use super::wamrc::Wamrc;

/// The log, in the state directory.
pub const LOG: &str = "catalog.log";
/// How large it gets before it is moved to `catalog.log.1`.
const LOG_LIMIT: u64 = 1 << 20;
/// The owner's setting, in the state directory: `off` turns automatic
/// updates off.
const SETTING: &str = "catalog-auto-update";
/// The updates left for the owner, one empty file each
/// (`neochumby-weather@1.3.0`), so each is logged once.
const HELD: &str = "catalog-held";

/// Writes `line` in the catalog's log, with the time, and in neochumbyd's.
pub fn log(state: &Path, line: &str) {
    eprintln!("neochumbyd: catalog: {line}");
    let path = state.join(LOG);
    if path.metadata().is_ok_and(|m| m.len() > LOG_LIMIT) {
        let _ = std::fs::rename(&path, state.join(format!("{LOG}.1")));
    }
    let r = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{} {}", utc(now()), line.replace('\n', " ")));
    if let Err(e) = r {
        eprintln!("neochumbyd: {}: {e}", path.display());
    }
}

/// Whether catalog apps update by themselves: unless the owner said not.
pub fn auto_updates(state: &Path) -> bool {
    std::fs::read_to_string(state.join(SETTING)).map_or(true, |s| s.trim() != "off")
}

/// The owner's setting.
pub fn set_auto_updates(state: &Path, on: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(state)?;
    let tmp = state.join(format!(".{SETTING}.{}", super::unique()));
    std::fs::write(&tmp, if on { "on\n" } else { "off\n" })?;
    std::fs::rename(&tmp, state.join(SETTING))?;
    log(state, &format!("automatic updates turned {} by the owner", if on { "on" } else { "off" }));
    Ok(())
}

/// Whether version `new` comes after `have` (semver; either unreadable:
/// no).
pub fn newer(new: &str, have: &str) -> bool {
    match (semver::Version::parse(new), semver::Version::parse(have)) {
        (Ok(n), Ok(h)) => n > h,
        _ => false,
    }
}

/// What became of one installed app.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Updated, from the version it had.
    Updated { app: String, from: String, to: String },
    /// Left for the owner: it asks for more.
    Held { app: String, to: String },
    /// It couldn't be: the reason is logged.
    Failed { app: String, to: String },
}

/// Updates each catalog app in `tree` that the cache lists in a newer
/// version, when that asks for nothing new; logs each, and each it leaves
/// for the owner, once. Does nothing if the owner turned this off.
pub fn auto_update(http: &Http, state: &Path, tree: &Path, wamrc: Option<&Wamrc>) -> Vec<Outcome> {
    let mut out = Vec::new();
    if !auto_updates(state) {
        return out;
    }
    let Some(cache) = discover::load(state).ok().flatten() else { return out };
    for (app, have) in install::catalog_apps(tree) {
        let Some((krate, version)) = have.krate.rsplit_once('@') else { continue };
        let Some(l) = cache.apps.iter().find(|l| l.name == krate) else { continue };
        if !newer(&l.version, version) {
            continue;
        }
        let Ok(spec) = Spec::parse(&l.spec()) else { continue };
        if spec.app != app {
            continue;
        }
        let held = state.join(HELD).join(format!("{}@{}", spec.name, spec.version));
        if held.exists() {
            continue;
        }
        let to = spec.version.clone();
        match prepare(http, &spec, tree, wamrc.cloned()) {
            Err(e) => {
                log(state, &format!("apps/{app}: not updated to {to}: {e}"));
                out.push(Outcome::Failed { app, to });
            }
            Ok(plan) if plan.asks_more() => {
                let (access, hosts) = plan.new_asks();
                log(
                    state,
                    &format!(
                        "apps/{app}: {spec} asks for more than {version} had ({}): left for the owner to approve, `{}` or GET APPS",
                        [access, hosts].concat().join(", "),
                        plan.command()
                    ),
                );
                let _ = std::fs::create_dir_all(state.join(HELD)).and_then(|()| std::fs::write(&held, ""));
                out.push(Outcome::Held { app, to });
            }
            Ok(plan) => match plan.write(tree, &state.join("scratch")) {
                Ok(_) => {
                    let label = if plan.verified() { "a verified build" } else { "unverified" };
                    log(
                        state,
                        &format!(
                            "apps/{app}: updated from {version} to {to} by itself ({label}; it asks for nothing new)"
                        ),
                    );
                    out.push(Outcome::Updated { app, from: version.into(), to });
                }
                Err(e) => {
                    log(state, &format!("apps/{app}: not updated to {to}: {e}"));
                    out.push(Outcome::Failed { app, to });
                }
            },
        }
    }
    out
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `secs` since 1970 as `2026-10-31T05:00:00Z`.
pub fn utc(secs: u64) -> String {
    // Howard Hinnant's civil_from_days.
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}
