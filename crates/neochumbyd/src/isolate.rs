//! Apps' companions and server programs under uids of their own
//! (`--app-uids`), so that they can't read neochumbyd's key or pairings,
//! change the tree, or reach another app's state. One of two sandboxes,
//! with this system's own (Landlock, or Apple's on macOS; confine.rs).
//! Either is enough, and both may apply at once ([`plan`]):
//!
//!   sandbox  uids  what runs
//!   yes      yes   both, layered
//!   yes      no    the sandbox
//!   no       yes   uids
//!   no       no    nothing, unless the owner passes --unsandboxed; and
//!                  never as root, where they would run as root
//!
//! neochumbyd started as root with `--app-uids FIRST-LAST` ([`Uids::start`]):
//!
//! - becomes the user that owns its state directory, keeping only what it
//!   needs to start programs as other uids and to stop them
//!   (neochumby_sys::privilege);
//! - gives each app the lowest uid in the range that no other app has and
//!   /etc/passwd and /etc/group don't use, as its uid and its group, and
//!   records it in `<state>/app-uids` ("app uid" lines), so the app keeps
//!   it;
//! - runs the app's companions and its server program (`server=`) as that
//!   one uid, with no other groups, in `<state>/apps/<app>`, which it owns
//!   (0750, in neochumbyd's group), with its `tmp` (0700)
//!   ([`Uids::ready`]). A directory left there by
//!   a neochumbyd that ran them as itself is handed over, with what's in
//!   it.
//!
//! The state directory must be other-executable (0711 or 0751, say), as a
//! program reaches its directory and its binary in the tree through it;
//! neochumbyd's own files in it are not readable by others (State::secure).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where each app's uid is recorded, in the state directory, and where it
/// was before the flag was --app-uids.
const RECORD: &str = "app-uids";
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const OLD_RECORD: &str = "plugin-uids";

/// Which sandbox programs get, as [`plan`] decides.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// A uid per app (and this system's sandbox too, where it has one).
    Uids,
    /// This system's sandbox alone.
    Sandbox,
    /// Neither: --unsandboxed, not as root.
    Unsandboxed,
}

/// What to do to get a sandbox, for the refusals.
const HOW: &str = "start neochumbyd as root with --app-uids FIRST-LAST, or on a kernel with Landlock; or, to run \
                   them as neochumbyd's own user, which they can then read and change, pass --unsandboxed \
                   (SECURITY.md)";

/// The sandbox, from whether neochumbyd is root, whether this system can
/// change uids (Linux), whether --app-uids was given, whether the system
/// has a sandbox of its own, and whether --unsandboxed was given; or why
/// nothing runs.
pub fn plan(root: bool, linux: bool, app_uids: bool, sandbox: bool, unsandboxed: bool) -> Result<Plan, String> {
    if app_uids && root && linux {
        return Ok(Plan::Uids);
    }
    if sandbox {
        return Ok(Plan::Sandbox);
    }
    if root && app_uids {
        return Err(format!("--app-uids works on Linux only: {HOW}"));
    }
    if root {
        return Err(format!(
            "neochumbyd is root, without --app-uids, so they would run as root, which --unsandboxed doesn't \
             allow: {HOW}"
        ));
    }
    if unsandboxed {
        return Ok(Plan::Unsandboxed);
    }
    if app_uids {
        return Err(format!("--app-uids needs neochumbyd started as root: {HOW}"));
    }
    Err(HOW.to_string())
}

/// The uids apps' programs get, and neochumbyd's own.
#[derive(Debug)]
pub struct Uids {
    state: PathBuf,
    /// `--app-uids`, FIRST and LAST.
    pub range: (u32, u32),
    own: (u32, u32),
    /// Ids /etc/passwd or /etc/group already use: never given to an app.
    taken: BTreeSet<u32>,
    /// Each app's uid, as recorded; locked while an app's directory is made
    /// ready, so two Chumbys opening an app at once don't race.
    apps: Mutex<BTreeMap<String, u32>>,
}

fn invalid(what: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_string())
}

/// `FIRST-LAST`, both at least 1000 (below are the system's own users),
/// FIRST no more than LAST, and LAST below 4294967295 (-1, "no uid").
pub fn parse_range(s: &str) -> Result<(u32, u32), String> {
    let bad = || format!("--app-uids takes FIRST-LAST, each 1000 or more, not {s}");
    let (a, b) = s.split_once('-').ok_or_else(bad)?;
    let (a, b): (u32, u32) = (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?);
    if a < 1000 || a > b || b == u32::MAX {
        return Err(bad());
    }
    Ok((a, b))
}

/// The ids in the third field of passwd(5) or group(5) lines.
// Read on Linux only, where --app-uids works.
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

impl Uids {
    /// Called first thing by `serve`, before any thread is started or any
    /// file written, when [`plan`] says [`Plan::Uids`]: becomes the state
    /// directory's owner, as above, or says what's wrong.
    #[cfg(not(target_os = "linux"))]
    pub fn start(_state: &Path, _range: (u32, u32)) -> Result<Uids, String> {
        Err("--app-uids works on Linux only".to_string())
    }

    #[cfg(target_os = "linux")]
    pub fn start(state: &Path, range: (u32, u32)) -> Result<Uids, String> {
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
            return Err(format!("--app-uids {}-{} holds neochumbyd's own uid or group", range.0, range.1));
        }
        if m.mode() & 0o001 == 0 {
            return Err(format!(
                "apps' programs reach their directories and binaries through {0}: `chmod o+x {0}`",
                state.display()
            ));
        }
        let mut taken = BTreeSet::new();
        for f in ["/etc/passwd", "/etc/group"] {
            taken.extend(ids_in(&std::fs::read_to_string(f).unwrap_or_default()));
        }
        neochumby_sys::privilege::become_user(own.0, own.1)
            .map_err(|e| format!("becoming uid {} group {}: {e}", own.0, own.1))?;
        // The record under its old name, from before --app-uids.
        if !state.join(RECORD).exists() && state.join(OLD_RECORD).exists() {
            std::fs::rename(state.join(OLD_RECORD), state.join(RECORD))
                .map_err(|e| format!("{OLD_RECORD} to {RECORD}: {e}"))?;
        }
        let apps = parse_record(&std::fs::read_to_string(state.join(RECORD)).unwrap_or_default());
        eprintln!("neochumbyd: running as uid {} group {}", own.0, own.1);
        Ok(Uids { state: state.to_path_buf(), range, own, taken, apps: Mutex::new(apps) })
    }

    /// `app`'s uid, given and recorded if it has none, with its directory
    /// (`<state>/apps/<app>`) and that directory's `tmp` its own. neochumbyd
    /// can't write in the directory after this, so `tmp` is made here.
    pub fn ready(&self, app: &str) -> io::Result<u32> {
        let mut apps = self.apps.lock().unwrap_or_else(|e| e.into_inner());
        let uid = match apps.get(app) {
            Some(&uid) if (self.range.0..=self.range.1).contains(&uid) => uid,
            Some(&uid) => {
                return Err(invalid(format!(
                    "{app} has uid {uid}, outside --app-uids; give it back its range, or remove its line in \
                     {RECORD} and its directory"
                )));
            }
            None => {
                let uid = allocate(&apps, self.range, &self.taken)
                    .ok_or_else(|| io::Error::other("no uid left in --app-uids"))?;
                apps.insert(app.to_string(), uid);
                let path = self.state.join(RECORD);
                let tmp = path.with_extension("new");
                std::fs::write(&tmp, format_record(&apps))?;
                std::fs::rename(tmp, path)?;
                eprintln!("neochumbyd: {app} is uid {uid}");
                uid
            }
        };
        // apps/ lists names only: others may pass through, not read it.
        let parent = self.state.join("apps");
        std::fs::DirBuilder::new().recursive(true).create(&parent)?;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o711))?;
        let home = parent.join(app);
        // The directory is the app's, in neochumbyd's group, 0750: neochumbyd
        // reads it to sandbox the program with it (Landlock's rule is made
        // from a descriptor of it, which some kernels open only with read
        // permission), and no other app can reach it (they have no other
        // groups). neochumbyd can't change it: it has only CAP_CHOWN, so it
        // takes it back to change the mode.
        let (own, group) = (self.own.0, self.own.1);
        let give = |m: u32| -> io::Result<()> {
            std::os::unix::fs::lchown(&home, Some(own), Some(group))?;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(m))?;
            std::os::unix::fs::lchown(&home, Some(uid), Some(group))
        };
        match std::fs::symlink_metadata(&home) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new().mode(0o750).create(&home)?;
                std::fs::DirBuilder::new().mode(0o700).create(home.join("tmp"))?;
                std::os::unix::fs::lchown(home.join("tmp"), Some(uid), Some(uid))?;
                give(0o750)?;
            }
            Err(e) => return Err(e),
            Ok(m) if m.is_dir() && m.uid() == uid => {
                if m.gid() != group || m.mode() & 0o7777 != 0o750 {
                    give(0o750)?;
                }
            }
            Ok(m) if m.is_dir() && m.uid() == own => {
                std::fs::DirBuilder::new().recursive(true).mode(0o700).create(home.join("tmp"))?;
                hand_over(&home, uid)?;
                give(0o750)?;
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
    fn fail_closed() {
        use Plan::*;
        // (root, Linux, --app-uids, a sandbox, --unsandboxed)
        let ok = |a, b, c, d, e| plan(a, b, c, d, e).unwrap();
        let refused = |a, b, c, d, e| plan(a, b, c, d, e).is_err();
        // Either sandbox is enough; uids where they can be had.
        assert_eq!(ok(true, true, true, true, false), Uids);
        assert_eq!(ok(true, true, true, false, false), Uids);
        assert_eq!(ok(false, true, false, true, false), Sandbox);
        assert_eq!(ok(true, true, false, true, false), Sandbox);
        // --app-uids that can't apply: the sandbox covers it, or a refusal.
        assert_eq!(ok(false, true, true, true, false), Sandbox);
        assert_eq!(ok(true, false, true, true, false), Sandbox);
        assert!(refused(false, true, true, false, false));
        assert!(refused(true, false, true, false, true));
        // Neither: only --unsandboxed, and never as root.
        assert!(refused(false, true, false, false, false));
        assert_eq!(ok(false, true, false, false, true), Unsandboxed);
        assert_eq!(ok(false, true, true, false, true), Unsandboxed);
        assert!(refused(true, true, false, false, false));
        assert!(refused(true, true, false, false, true));
        assert!(refused(true, false, false, false, true));
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

    /// As root only (CI runs it with sudo; see CONTRIBUTING.md): neochumbyd
    /// becomes a user of its own, and two apps' programs, each under its
    /// own uid (and in this system's sandbox, where it has one), can't read
    /// neochumbyd's key, pairings or settings, or each other's directory,
    /// and have no capabilities; and neochumbyd can still stop them.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs root"]
    fn as_root() {
        use crate::confine::Confine;
        use crate::state::State;
        use neochumby_sys::process::{self, Signal};
        assert_eq!(neochumby_sys::user::effective().uid, 0, "run as root");
        // neochumbyd's user and group, made up, and a range no user has.
        let (own, range) = (60999, "61000-61049");
        let dir = PathBuf::from(format!("/tmp/neochumbyd-as-root-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o711).create(&dir).unwrap();
        std::os::unix::fs::chown(&dir, Some(own), Some(own)).unwrap();
        let tree = dir.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::os::unix::fs::chown(&tree, Some(own), Some(own)).unwrap();

        std::fs::write(dir.join(OLD_RECORD), "").unwrap();
        let confine = Confine::choose(&dir, Some(range), false).unwrap();
        assert!(dir.join(RECORD).exists() && !dir.join(OLD_RECORD).exists(), "moved to its new name");
        assert_eq!(neochumby_sys::user::effective().uid, own);
        let state = State::open(dir.clone()).unwrap();
        state.secure().unwrap();
        state.identity().unwrap();
        state.owner_token().unwrap();
        std::fs::write(dir.join("paired/abcd"), "kitchen\n").unwrap();
        // Left by a neochumbyd that ran companions as itself.
        std::fs::create_dir_all(dir.join("apps/feeds/cache")).unwrap();
        std::fs::write(dir.join("apps/feeds/cache/old"), "x").unwrap();

        let command = |app: &str, script: &str| {
            let home = dir.join("apps").join(app);
            let plan = confine.plan(app).unwrap();
            let mut cmd = plan.command(Path::new("/bin/sh"), &dir, &tree, &home, None).unwrap();
            plan.prepare(&mut cmd, &home, None).unwrap();
            cmd.args(["-c", script]);
            cmd
        };
        // What it printed, and then what it said went wrong.
        let run = |app: &str, script: &str| {
            let out = command(app, script).output().unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
        };
        let probe = "id -u; id -G; grep CapEff /proc/self/status; echo \"$HOME $TMPDIR\"; \
                     cat ../../identity && echo READ-KEY; cat ../../owner-token && echo READ-TOKEN; \
                     ls ../../paired && echo LISTED-PAIRED; ls ../../config && echo LISTED-CONFIG; \
                     touch ../../paired/x && echo WROTE-PAIRED; echo mine > mine && echo WROTE-OWN; \
                     echo t > \"$TMPDIR/t\" && echo WROTE-TMP; ls ../ && echo LISTED-APPS";
        let a = run("airplay", probe);
        let a_uid: u32 = a.lines().next().unwrap().parse().unwrap();
        assert!((61000..=61049).contains(&a_uid), "{a}");
        // Its own group, and no other.
        assert_eq!(a.lines().nth(1), Some(a_uid.to_string().as_str()), "{a}");
        assert!(a.contains("CapEff:\t0000000000000000"), "{a}");
        for refused in ["READ-KEY", "READ-TOKEN", "LISTED-PAIRED", "LISTED-CONFIG", "WROTE-PAIRED", "LISTED-APPS"] {
            assert!(!a.contains(refused), "{refused}: {a}");
        }
        assert!(a.contains("WROTE-OWN") && a.contains("WROTE-TMP"), "{a}");
        assert!(a.contains("/apps/airplay /"), "HOME and TMPDIR in its own directory: {a}");

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

        // neochumbyd, as its own user, can still stop one, as it stops a
        // server program (CAP_KILL).
        let mut child = command("wx", "exec sleep 600").spawn().unwrap();
        process::kill_group(child.id(), Signal::Term).unwrap();
        assert!(!child.wait().unwrap().success());
        // Left in /tmp: neochumbyd's user can't remove the apps' files.
    }
}
