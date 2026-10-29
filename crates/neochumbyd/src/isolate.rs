//! Each app's companion under a uid of its own, so that a companion can't
//! read neochumbyd's key or pairings, change the tree, or reach another
//! app's state.
//!
//! neochumbyd started as root with `--plugin-uids FIRST-LAST`:
//!
//! - becomes the user that owns its state directory, keeping only what it
//!   needs to start companions as other uids (neochumby_sys::privilege);
//! - gives each app the lowest uid in the range that no other app has and
//!   /etc/passwd and /etc/group don't use, as its uid and its group, and
//!   records it in `<state>/plugin-uids` ("app uid" lines), so the app
//!   keeps it;
//! - runs the app's companions as that uid, with no other groups, in
//!   `<state>/apps/<app>`, which it owns, mode 0700. A directory left there
//!   by a neochumbyd that ran companions as itself is handed over, with
//!   what's in it.
//!
//! The state directory must be other-executable (0711 or 0751, say), as a
//! companion reaches its directory and its binary in the tree through it;
//! neochumbyd's own files in it are not readable by others (State::secure).
//!
//! Without root, or without the flag, companions run as neochumbyd's own
//! user, as they always have, and neochumbyd says so as it starts.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Where each app's uid is recorded, in the state directory.
const RECORD: &str = "plugin-uids";

/// How companions are run.
pub struct Plugins {
    state: PathBuf,
    uids: Option<Uids>,
}

/// The uids companions get, and neochumbyd's own.
struct Uids {
    range: (u32, u32),
    own: (u32, u32),
    /// Ids /etc/passwd or /etc/group already use: never given to an app.
    taken: BTreeSet<u32>,
    /// Each app's uid, as recorded; locked while a companion's directory
    /// is made ready, so two Chumbys opening an app at once don't race.
    apps: Mutex<BTreeMap<String, u32>>,
}

fn invalid(what: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_string())
}

/// `FIRST-LAST`, both at least 1000 (below are the system's own users),
/// FIRST no more than LAST, and LAST below 4294967295 (-1, "no uid").
pub fn parse_range(s: &str) -> Result<(u32, u32), String> {
    let bad = || format!("--plugin-uids takes FIRST-LAST, each 1000 or more, not {s}");
    let (a, b) = s.split_once('-').ok_or_else(bad)?;
    let (a, b): (u32, u32) = (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?);
    if a < 1000 || a > b || b == u32::MAX {
        return Err(bad());
    }
    Ok((a, b))
}

/// The ids in the third field of passwd(5) or group(5) lines.
// Read on Linux only, where --plugin-uids works.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn ids_in(text: &str) -> impl Iterator<Item = u32> + '_ {
    text.lines().filter_map(|l| l.split(':').nth(2)?.parse().ok())
}

/// The record's "app uid" lines; anything else is left out.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_record(text: &str) -> BTreeMap<String, u32> {
    text.lines()
        .filter_map(|l| {
            let (app, uid) = l.split_once(' ')?;
            Some((app.to_string(), uid.trim().parse().ok()?))
        })
        .filter(|(app, _)| neochumby_proto::valid_name(app))
        .collect()
}

fn format_record(apps: &BTreeMap<String, u32>) -> String {
    apps.iter().map(|(app, uid)| format!("{app} {uid}\n")).collect()
}

/// The lowest uid in `range` that no app in `apps` has and that isn't
/// `taken`.
fn allocate(apps: &BTreeMap<String, u32>, range: (u32, u32), taken: &BTreeSet<u32>) -> Option<u32> {
    let used: BTreeSet<u32> = apps.values().copied().collect();
    (range.0..=range.1).find(|u| !used.contains(u) && !taken.contains(u))
}

impl Plugins {
    /// Called first thing by `serve`, before any thread is started or any
    /// file written: as root with a range, becomes the state directory's
    /// owner, as above. Anything else is said on stderr, and companions
    /// run as this process's user.
    pub fn start(state: &Path, range: Option<&str>) -> Result<Plugins, String> {
        let root = neochumby_sys::privilege::effective_uid() == 0;
        let range = range.map(parse_range).transpose()?;
        let uids = match (root, range) {
            (true, Some(range)) => Uids::start(state, range)?,
            (false, Some(_)) => {
                eprintln!(
                    "neochumbyd: WARNING: --plugin-uids needs neochumbyd started as root. Companions run as this \
                     user, and can read its key and pairings."
                );
                None
            }
            (true, None) => {
                eprintln!(
                    "neochumbyd: WARNING: running as root without --plugin-uids. Companions run as root. Start \
                     neochumbyd as root with --plugin-uids FIRST-LAST to give each app a uid of its own (SECURITY.md)."
                );
                None
            }
            (false, None) => {
                eprintln!(
                    "neochumbyd: WARNING: companions run as this user, and can read its key and pairings. Start \
                     neochumbyd as root with --plugin-uids FIRST-LAST to give each app a uid of its own (SECURITY.md)."
                );
                None
            }
        };
        Ok(Plugins { state: state.to_path_buf(), uids })
    }

    /// Companions as neochumbyd's own user, for tests.
    #[cfg(test)]
    pub fn as_neochumbyd(state: &Path) -> Plugins {
        Plugins { state: state.to_path_buf(), uids: None }
    }

    /// The command that runs `exe`, a companion of `app` (a valid name), in
    /// the app's own directory, which it makes ready. `NEOCHUMBY_STATE` is
    /// that directory; with a uid of its own, so is `HOME`.
    pub fn command(&self, exe: &Path, app: &str) -> io::Result<Command> {
        let home = self.state.join("apps").join(app);
        let mut cmd = Command::new(exe);
        match &self.uids {
            None => std::fs::create_dir_all(&home)?,
            Some(u) => {
                let uid = u.ready(&self.state, app, &home)?;
                cmd.uid(uid).gid(uid).env("HOME", &home);
            }
        }
        // As the companion: with a uid of its own, neochumbyd can't enter
        // the directory.
        cmd.current_dir(&home).env("NEOCHUMBY_STATE", &home);
        Ok(cmd)
    }
}

impl Uids {
    #[cfg(not(target_os = "linux"))]
    fn start(_state: &Path, _range: (u32, u32)) -> Result<Option<Uids>, String> {
        eprintln!(
            "neochumbyd: WARNING: --plugin-uids works on Linux only. Companions run as root here: run neochumbyd \
             as its own user instead."
        );
        Ok(None)
    }

    #[cfg(target_os = "linux")]
    fn start(state: &Path, range: (u32, u32)) -> Result<Option<Uids>, String> {
        let m = std::fs::metadata(state)
            .map_err(|e| format!("{}: {e} (make it, owned by neochumbyd's user)", state.display()))?;
        let own = (m.uid(), m.gid());
        if own.0 == 0 || own.1 == 0 {
            return Err(format!(
                "{} belongs to root: give it to neochumbyd's own user and group, which neochumbyd then runs as",
                state.display()
            ));
        }
        if (range.0..=range.1).contains(&own.0) || (range.0..=range.1).contains(&own.1) {
            return Err(format!("--plugin-uids {}-{} holds neochumbyd's own uid or group", range.0, range.1));
        }
        if m.mode() & 0o001 == 0 {
            return Err(format!(
                "companions reach their directories and binaries through {0}: `chmod o+x {0}`",
                state.display()
            ));
        }
        let mut taken = BTreeSet::new();
        for f in ["/etc/passwd", "/etc/group"] {
            taken.extend(ids_in(&std::fs::read_to_string(f).unwrap_or_default()));
        }
        neochumby_sys::privilege::become_user(own.0, own.1)
            .map_err(|e| format!("becoming uid {} group {}: {e}", own.0, own.1))?;
        let apps = parse_record(&std::fs::read_to_string(state.join(RECORD)).unwrap_or_default());
        eprintln!("neochumbyd: running as uid {} group {}; companions get uids {}-{}", own.0, own.1, range.0, range.1);
        Ok(Some(Uids { range, own, taken, apps: Mutex::new(apps) }))
    }

    /// `app`'s uid, given and recorded if it has none, with `home` its own.
    fn ready(&self, state: &Path, app: &str, home: &Path) -> io::Result<u32> {
        let mut apps = self.apps.lock().unwrap_or_else(|e| e.into_inner());
        let uid = match apps.get(app) {
            Some(&uid) if (self.range.0..=self.range.1).contains(&uid) => uid,
            Some(&uid) => {
                return Err(invalid(format!(
                    "{app} has uid {uid}, outside --plugin-uids; give it back its range, or remove its line in \
                     {RECORD} and its directory"
                )));
            }
            None => {
                let uid = allocate(&apps, self.range, &self.taken)
                    .ok_or_else(|| io::Error::other("no uid left in --plugin-uids"))?;
                apps.insert(app.to_string(), uid);
                let path = state.join(RECORD);
                let tmp = path.with_extension("new");
                std::fs::write(&tmp, format_record(&apps))?;
                std::fs::rename(tmp, path)?;
                eprintln!("neochumbyd: {app} is uid {uid}");
                uid
            }
        };
        // apps/ lists names only: others may pass through, not read it.
        let parent = state.join("apps");
        std::fs::DirBuilder::new().recursive(true).create(&parent)?;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o711))?;
        match std::fs::symlink_metadata(home) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new().mode(0o700).create(home)?;
                std::os::unix::fs::lchown(home, Some(uid), Some(uid))?;
            }
            Err(e) => return Err(e),
            Ok(m) if m.is_dir() && m.uid() == uid => {}
            Ok(m) if m.is_dir() && m.uid() == self.own.0 => {
                hand_over(home, uid)?;
                std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
                std::os::unix::fs::lchown(home, Some(uid), Some(uid))?;
                eprintln!("neochumbyd: {} handed over to uid {uid}", home.display());
            }
            Ok(_) => {
                return Err(invalid(format!("{} isn't a directory {app}'s or neochumbyd's own", home.display())));
            }
        }
        Ok(uid)
    }
}

/// Gives everything under `dir` to `uid`, deepest first, without following
/// symbolic links (they are handed over themselves). A file with a second
/// name is refused: the other name could be neochumbyd's key.
fn hand_over(dir: &Path, uid: u32) -> io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let m = std::fs::symlink_metadata(e.path())?;
        if m.is_dir() {
            hand_over(&e.path(), uid)?;
        } else if m.nlink() > 1 {
            return Err(invalid(format!("{} has other names: not handed over", e.path().display())));
        }
        std::os::unix::fs::lchown(e.path(), Some(uid), Some(uid))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("61000-61099"), Ok((61000, 61099)));
        assert_eq!(parse_range("61000-61000"), Ok((61000, 61000)));
        for bad in ["", "61000", "61000-", "-61099", "61099-61000", "999-2000", "0-10", "a-b", "1000-4294967295"] {
            assert!(parse_range(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_lowest_free_uid() {
        let mut apps = BTreeMap::new();
        let taken: BTreeSet<u32> = [61001].into();
        assert_eq!(allocate(&apps, (61000, 61003), &taken), Some(61000));
        apps.insert("airplay".to_string(), 61000);
        // 61001 is a user's already.
        assert_eq!(allocate(&apps, (61000, 61003), &taken), Some(61002));
        apps.insert("wx".to_string(), 61002);
        apps.insert("feeds".to_string(), 61003);
        assert_eq!(allocate(&apps, (61000, 61003), &taken), None);
        // An app gone frees its uid.
        apps.remove("airplay");
        assert_eq!(allocate(&apps, (61000, 61003), &taken), Some(61000));
    }

    #[test]
    fn the_record_round_trips() {
        let apps = parse_record("airplay 61000\nwx 61002\n../x 61003\nbroken\nfeeds x\n");
        assert_eq!(apps, BTreeMap::from([("airplay".to_string(), 61000), ("wx".to_string(), 61002)]));
        assert_eq!(parse_record(&format_record(&apps)), apps);
    }

    #[test]
    fn ids_already_used() {
        let passwd = "root:x:0:0:root:/root:/bin/sh\nneochumbyd:x:998:997::/var/lib/neochumbyd:/usr/sbin/nologin\n\
                      # a comment\nbroken\n";
        assert_eq!(ids_in(passwd).collect::<Vec<_>>(), [0, 998]);
        assert_eq!(ids_in("neochumbyd:x:997:matt\n").collect::<Vec<_>>(), [997]);
    }

    #[test]
    fn without_a_range_companions_run_as_neochumbyd() {
        let dir = std::env::temp_dir().join(format!("neochumbyd-isolate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = Plugins::as_neochumbyd(&dir);
        let out = p.command(Path::new("/bin/sh"), "airplay").unwrap().args(["-c", "pwd -P"]).output().unwrap();
        let want = dir.join("apps/airplay").canonicalize().unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), want.to_str().unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// As root only (CI runs it with sudo; see CONTRIBUTING.md): neochumbyd
    /// becomes a user of its own, and two apps' companions, each under its
    /// own uid, can't read neochumbyd's key, pairings or settings, or each
    /// other's directory, and have no capabilities.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs root"]
    fn as_root() {
        use crate::state::State;
        assert_eq!(neochumby_sys::privilege::effective_uid(), 0, "run as root");
        // neochumbyd's user and group, made up, and a range no user has.
        let (own, range) = (60999, "61000-61049");
        let dir = PathBuf::from(format!("/tmp/neochumbyd-as-root-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o711).create(&dir).unwrap();
        std::os::unix::fs::chown(&dir, Some(own), Some(own)).unwrap();

        let plugins = Plugins::start(&dir, Some(range)).unwrap();
        assert_eq!(neochumby_sys::privilege::effective_uid(), own);
        let state = State::open(dir.clone()).unwrap();
        state.secure().unwrap();
        state.identity().unwrap();
        std::fs::write(dir.join("paired/abcd"), "kitchen\n").unwrap();
        // Left by a neochumbyd that ran companions as itself.
        std::fs::create_dir_all(dir.join("apps/feeds/cache")).unwrap();
        std::fs::write(dir.join("apps/feeds/cache/old"), "x").unwrap();

        let run = |app: &str, script: &str| {
            let out = plugins.command(Path::new("/bin/sh"), app).unwrap().args(["-c", script]).output().unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        let probe = "id -u; id -G; grep CapEff /proc/self/status; \
                     cat ../../identity && echo READ-KEY; ls ../../paired && echo LISTED-PAIRED; \
                     ls ../../config && echo LISTED-CONFIG; touch ../../paired/x && echo WROTE-PAIRED; \
                     echo mine > mine && echo WROTE-OWN; ls ../ && echo LISTED-APPS";
        let a = run("airplay", probe);
        let a_uid: u32 = a.lines().next().unwrap().parse().unwrap();
        assert!((61000..=61049).contains(&a_uid), "{a}");
        // Its own group, and no other.
        assert_eq!(a.lines().nth(1), Some(a_uid.to_string().as_str()), "{a}");
        assert!(a.contains("CapEff:\t0000000000000000"), "{a}");
        for refused in ["READ-KEY", "LISTED-PAIRED", "LISTED-CONFIG", "WROTE-PAIRED", "LISTED-APPS"] {
            assert!(!a.contains(refused), "{refused}: {a}");
        }
        assert!(a.contains("WROTE-OWN"), "{a}");

        let b = run("wx", "id -u; cat ../airplay/mine && echo READ-OTHER; echo ok");
        let b_uid: u32 = b.lines().next().unwrap().parse().unwrap();
        assert_ne!(a_uid, b_uid);
        assert!(!b.contains("READ-OTHER") && b.contains("ok"), "{b}");

        // The same uid next time, as recorded.
        assert_eq!(run("airplay", "id -u").trim(), a_uid.to_string());
        let record = std::fs::read_to_string(dir.join(RECORD)).unwrap();
        assert!(record.contains(&format!("airplay {a_uid}\n")) && record.contains(&format!("wx {b_uid}\n")));

        // feeds' old directory is its own now, with what's in it.
        let f = run("feeds", "id -u; cat cache/old && echo; touch cache/new && echo WROTE");
        assert!(f.contains("x\n") && f.contains("WROTE"), "{f}");
        // Left in /tmp: neochumbyd's user can't remove the apps' files.
    }
}
