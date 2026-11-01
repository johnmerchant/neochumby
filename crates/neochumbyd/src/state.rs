//! What neochumbyd keeps, in its state directory:
//!
//!   identity          its private key (0600; the daemon's alone)
//!   identity.pub      its public key, hex, for `neochumbyd add` run by an
//!                     admin who can't read `identity`
//!   paired/<key>      a Chumby: file named for its public key (hex), holding
//!                     its name. `neochumbyd add` writes it, as a stick is
//!                     made; no connection can
//!   config/<name>     a Chumby's settings, key=value lines (`start=<app>`),
//!                     sent to it on every connection (`Msg::Config`). Kept
//!                     by name, so a stick made again under its name keeps
//!                     them
//!   owner-token       the owner's token (0640, the state directory's
//!                     group), which the command line's catalog commands
//!                     pass to the admin protocol (admin.rs)
//!   apps/<app>        each app's own directory, for its companions and its
//!                     server program: the only part of the state directory
//!                     they can reach (confine.rs); with --app-uids, the
//!                     app's uid's, 0750 in neochumbyd's group (isolate.rs)
//!   app-uids          each app's uid, with --app-uids (isolate.rs)
//!   logs/, dev/       what the developer link keeps and is asked (dev.rs)
//!
//! None of it is readable by other users (`secure`), as apps' programs may
//! run under uids of their own.
//!
//! The commands work on these files directly, so whoever is in the daemon's
//! group can run them.

use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use neochumby_proto::settings::{self, Settings};
use neochumby_proto::{Identity, PublicKey, hex, unhex_key, valid_name};

#[derive(Clone)]
pub struct State {
    dir: PathBuf,
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_string())
}

impl State {
    pub fn open(dir: PathBuf) -> io::Result<State> {
        std::fs::create_dir_all(dir.join("paired"))?;
        std::fs::create_dir_all(dir.join("config"))?;
        Ok(State { dir })
    }

    /// The state directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Makes what neochumbyd keeps unreadable to other users, apps'
    /// programs with uids of their own among them: its key 0600, and its
    /// directories with no permission for others (the owner's and group's
    /// stay as they are, for the commands run by its group). Run by `serve`
    /// as their owner, with a umask that keeps what it makes the same.
    pub fn secure(&self) -> io::Result<()> {
        let named = |path: &std::path::Path, e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", path.display()));
        for d in ["paired", "config", "logs", "dev"] {
            let path = self.dir.join(d);
            std::fs::create_dir_all(&path).map_err(|e| named(&path, e))?;
            let mode = std::fs::metadata(&path).map_err(|e| named(&path, e))?.permissions().mode();
            // Only if it changes anything: under systemd's RestrictSUIDSGID,
            // a chmod that keeps a directory's setgid bit is refused.
            if mode & 0o007 != 0 {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & 0o7770))
                    .map_err(|e| named(&path, e))?;
            }
        }
        let key = self.dir.join("identity");
        match std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r.map_err(|e| named(&key, e)),
        }
    }

    /// Its own key: made on first start.
    pub fn identity(&self) -> io::Result<Identity> {
        let path = self.dir.join("identity");
        match std::fs::read_to_string(&path) {
            Ok(s) => Identity::from_secret(&s),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let id = Identity::generate()?;
                let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
                io::Write::write_all(&mut f, id.secret().as_bytes())?;
                Ok(id)
            }
            Err(e) => Err(e),
        }
    }

    /// Its public key, from `identity.pub` (which `serve` keeps, for those
    /// who can't read `identity`), else from `identity`, made if need be.
    pub fn public_key(&self) -> io::Result<PublicKey> {
        match std::fs::read_to_string(self.dir.join("identity.pub")) {
            Ok(s) => unhex_key(s.trim()),
            Err(_) => Ok(self.identity()?.public()),
        }
    }

    /// Keeps `identity.pub`, for [`State::public_key`]; run by `serve`.
    pub fn publish_key(&self, key: &PublicKey) -> io::Result<()> {
        let path = self.dir.join("identity.pub");
        let tmp = path.with_extension("pub.new");
        std::fs::write(&tmp, format!("{}\n", hex(key)))?;
        std::fs::rename(tmp, path)
    }

    /// The owner's token, made on first use: 16 random bytes as hex. The
    /// owner reads it with `neochumbyd token`.
    pub fn owner_token(&self) -> io::Result<String> {
        let path = self.dir.join("owner-token");
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(s.trim().to_string()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let (tmp, token) = self.write_token()?;
                // A link fails if the file is there: whoever made it first
                // wins, and the other reads theirs.
                let linked = std::fs::hard_link(&tmp, &path);
                let _ = std::fs::remove_file(&tmp);
                match linked {
                    Ok(()) => Ok(token),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        Ok(std::fs::read_to_string(&path)?.trim().to_string())
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Replaces the owner's token, for when it may have leaked. The old one
    /// stops working at once: the token is read on every check.
    pub fn new_owner_token(&self) -> io::Result<String> {
        let (tmp, token) = self.write_token()?;
        std::fs::rename(&tmp, self.dir.join("owner-token")).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
        Ok(token)
    }

    /// A new token, in a file of its own beside owner-token, ready to be
    /// put in its place.
    ///
    /// Its mode is 0640 and its group the state directory's, whoever writes
    /// it: neochumbyd often runs as a user of its own, while its admins,
    /// in its group, run `neochumbyd token` as themselves. The daemon reads
    /// a file an admin wrote through the group, and the admins read the
    /// daemon's the same way. Both are set on the file before it takes the
    /// name, so it is never there with a mode the daemon cannot read.
    fn write_token(&self) -> io::Result<(PathBuf, String)> {
        let token = random_token()?;
        let tmp = self.dir.join(format!("owner-token.{}.new", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        let done = (|| {
            let gid = std::fs::metadata(&self.dir)?.gid();
            std::os::unix::fs::fchown(&f, None, Some(gid))?;
            // Explicitly, as the umask may have taken the group's read.
            f.set_permissions(std::fs::Permissions::from_mode(0o640))?;
            io::Write::write_all(&mut f, format!("{token}\n").as_bytes())?;
            f.sync_all()
        })();
        if let Err(e) = done {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok((tmp, token))
    }

    /// Whether `token` is the owner's. Compared in constant time, so its
    /// timing gives nothing away; with no token yet, nothing matches.
    pub fn is_owner_token(&self, token: &str) -> bool {
        let Ok(owner) = std::fs::read_to_string(self.dir.join("owner-token")) else { return false };
        let (a, b) = (owner.trim().as_bytes(), token.as_bytes());
        !a.is_empty() && a.len() == b.len() && a.iter().zip(b).fold(0, |d, (x, y)| d | (x ^ y)) == 0
    }

    /// The name `key` paired under, if it has.
    pub fn paired(&self, key: &PublicKey) -> Option<String> {
        let name = std::fs::read_to_string(self.dir.join("paired").join(hex(key))).ok()?;
        let name = name.trim();
        valid_name(name).then(|| name.to_string())
    }

    /// (name, key) of each paired Chumby.
    pub fn pairs(&self) -> io::Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(self.dir.join("paired"))?.flatten() {
            let key = e.file_name().to_string_lossy().into_owned();
            if let Ok(k) = unhex_key(&key)
                && let Some(name) = self.paired(&k)
            {
                out.push((name, key));
            }
        }
        out.sort();
        Ok(out)
    }

    /// Registers `key` as the Chumby `name`: how a stick gets in. Another
    /// key under `name`, or `key` under another name, stays unless `replace`.
    pub fn add(&self, name: &str, key: &PublicKey, replace: bool) -> io::Result<()> {
        if !valid_name(name) {
            return Err(bad("a Chumby's name is 1-32 of a-z, 0-9, _ and -"));
        }
        let hexed = hex(key);
        let pairs = self.pairs()?;
        if !replace {
            if let Some((_, old)) = pairs.iter().find(|(n, k)| n == name && *k != hexed) {
                return Err(bad(&format!(
                    "{name} is already registered, with key {old}; to put this one in its place, use --replace"
                )));
            }
            if let Some((other, _)) = pairs.iter().find(|(n, k)| n != name && *k == hexed) {
                return Err(bad(&format!("that key is already registered as {other}; --replace moves it to {name}")));
            }
        }
        for (n, k) in &pairs {
            if (n == name || *k == hexed) && !(n == name && *k == hexed) {
                std::fs::remove_file(self.dir.join("paired").join(k))?;
            }
        }
        let path = self.dir.join("paired").join(&hexed);
        let tmp = path.with_extension("new");
        std::fs::write(&tmp, format!("{name}\n"))?;
        std::fs::rename(tmp, path)
    }

    pub fn forget(&self, name: &str) -> io::Result<()> {
        for (n, key) in self.pairs()? {
            if n == name {
                std::fs::remove_file(self.dir.join("paired").join(key))?;
            }
        }
        Ok(())
    }

    /// `name`'s settings, as sent to it.
    pub fn settings(&self, name: &str) -> Settings {
        Settings::from_pairs(self.config(name))
    }

    /// `name`'s settings as pairs, in the order they were set.
    pub fn config(&self, name: &str) -> Vec<(String, String)> {
        if !valid_name(name) {
            return Vec::new();
        }
        let s = std::fs::read_to_string(self.dir.join("config").join(name)).unwrap_or_default();
        s.lines()
            .filter_map(|l| l.split_once('='))
            .filter(|(k, v)| settings::valid(k, v))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Sets one of a paired Chumby's settings; an empty value removes it.
    pub fn set(&self, name: &str, key: &str, value: &str) -> io::Result<()> {
        if !self.pairs()?.iter().any(|(n, _)| n == name) {
            return Err(bad("no Chumby paired under that name"));
        }
        // An empty value removes the key, whatever it is.
        let check = if value.is_empty() { Settings::check_key(key) } else { Settings::check(key, value) };
        check.map_err(|e| bad(&e))?;
        let mut c = self.config(name);
        c.retain(|(k, _)| k != key);
        if !value.is_empty() {
            c.push((key.into(), value.into()));
        }
        let body: String = c.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        let path = self.dir.join("config").join(name);
        let tmp = path.with_extension("new");
        std::fs::write(&tmp, body)?;
        std::fs::rename(tmp, path)
    }
}

/// 128 bits from the system, as hex.
fn random_token() -> io::Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(hex(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings() {
        let dir = std::env::temp_dir().join(format!("neochumbyd-state-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = State::open(dir.clone()).unwrap();
        let key = Identity::generate().unwrap().public();
        assert!(st.set("kitchen", "start", "wx").is_err(), "not paired");
        std::fs::write(dir.join("paired").join(hex(&key)), "kitchen\n").unwrap();

        st.set("kitchen", "start", "wx").unwrap();
        st.set("kitchen", "theme", "dark").unwrap();
        st.set("kitchen", "start", "home").unwrap();
        assert_eq!(st.config("kitchen"), [("theme".into(), "dark".into()), ("start".into(), "home".into())]);
        st.set("kitchen", "theme", "").unwrap();
        assert_eq!(st.config("kitchen"), [("start".to_string(), "home".to_string())]);

        assert!(st.set("kitchen", "Home", "wx").is_err());
        assert!(st.set("kitchen", "start", "w x").is_err());
        assert!(st.set("kitchen", "start", "a\nb=c").is_err());
        // Typed settings take only their own values.
        assert!(st.set("kitchen", "dev", "yes").is_err());
        st.set("kitchen", "dev", "on").unwrap();
        assert!(st.settings("kitchen").dev);
        st.set("kitchen", "dev", "").unwrap();
        // Keys of its own are an owner's to add.
        st.set("kitchen", "theme", "dark").unwrap();
        assert_eq!(st.settings("kitchen").other, [("theme".to_string(), "dark".to_string())]);
        assert!(st.config("../kitchen").is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The test runs as one user, so it checks what lets another member of
    /// the state directory's group read the token: mode 0640 and the
    /// directory's group, from a first `token` and from `token new` alike.
    /// The file is created 0600 and then given 0640, so a mode the umask
    /// decided would show here as 0600. Where this user is in a second group, the
    /// directory is given that one, as a setgid directory of neochumbyd's
    /// group would be for an admin whose own group differs.
    #[test]
    fn owner_token() {
        let dir = std::env::temp_dir().join(format!("neochumbyd-token-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = State::open(dir.clone()).unwrap();
        let egid = neochumby_sys::user::effective().gid;
        if let Some(other) = neochumby_sys::user::groups().unwrap().into_iter().find(|&g| g != egid) {
            std::os::unix::fs::chown(&dir, None, Some(other)).unwrap();
        }
        let group = std::fs::metadata(&dir).unwrap().gid();
        let path = dir.join("owner-token");
        let group_can_read = || {
            let m = std::fs::metadata(&path).unwrap();
            assert_eq!(m.permissions().mode() & 0o777, 0o640);
            assert_eq!(m.gid(), group, "the state directory's group");
        };
        assert!(!st.is_owner_token(""), "no token yet");
        let t = st.owner_token().unwrap();
        assert_eq!(t.len(), 32);
        assert_eq!(st.owner_token().unwrap(), t, "made once");
        assert!(st.is_owner_token(&t));
        assert!(!st.is_owner_token("") && !st.is_owner_token(&t[1..]) && !st.is_owner_token(&format!("{t}0")));
        group_can_read();

        // Replaced, even over a file its writer cannot write (as one
        // another admin made would be), and with nothing left beside it.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o440)).unwrap();
        let new = st.new_owner_token().unwrap();
        assert_ne!(new, t);
        assert!(st.is_owner_token(&new) && !st.is_owner_token(&t));
        group_can_read();
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert!(!left.iter().any(|f| f.to_string_lossy().ends_with(".new")), "{left:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn adding() {
        let dir = std::env::temp_dir().join(format!("neochumbyd-add-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = State::open(dir.clone()).unwrap();
        let (k1, k2) = (Identity::generate().unwrap().public(), Identity::generate().unwrap().public());
        assert!(st.add("Den!", &k1, false).is_err(), "not a name");
        st.add("den", &k1, false).unwrap();
        assert_eq!(st.paired(&k1).as_deref(), Some("den"));
        // Again, the same: nothing changes.
        st.add("den", &k1, false).unwrap();
        assert_eq!(st.pairs().unwrap().len(), 1);

        // Another key under the name replaces it only when asked to.
        let e = st.add("den", &k2, false).unwrap_err().to_string();
        assert!(e.contains("--replace") && e.contains(&hex(&k1)), "{e}");
        assert_eq!(st.paired(&k1).as_deref(), Some("den"));
        st.add("den", &k2, true).unwrap();
        assert_eq!((st.paired(&k1), st.paired(&k2).as_deref()), (None, Some("den")));

        // The key under another name, likewise.
        assert!(st.add("attic", &k2, false).unwrap_err().to_string().contains("as den"));
        st.add("attic", &k2, true).unwrap();
        assert_eq!(st.pairs().unwrap().len(), 1);
        assert_eq!(st.paired(&k2).as_deref(), Some("attic"));

        st.forget("attic").unwrap();
        assert!(st.pairs().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn its_public_key() {
        let dir = std::env::temp_dir().join(format!("neochumbyd-pub-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = State::open(dir.clone()).unwrap();
        let id = st.identity().unwrap();
        assert_eq!(st.public_key().unwrap(), id.public());
        st.publish_key(&id.public()).unwrap();
        // Whoever can't read `identity` reads this.
        std::fs::remove_file(dir.join("identity")).unwrap();
        assert_eq!(st.public_key().unwrap(), id.public());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn secured() {
        let dir = std::env::temp_dir().join(format!("neochumbyd-secure-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = State::open(dir.clone()).unwrap();
        std::fs::set_permissions(dir.join("paired"), std::fs::Permissions::from_mode(0o2775)).unwrap();
        std::fs::write(dir.join("identity"), "x").unwrap();
        std::fs::set_permissions(dir.join("identity"), std::fs::Permissions::from_mode(0o644)).unwrap();
        st.secure().unwrap();
        let mode = |p: &str| std::fs::metadata(dir.join(p)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("paired"), 0o2770);
        assert_eq!(mode("identity"), 0o600);
        for d in ["config", "logs", "dev"] {
            assert_eq!(mode(d) & 0o007, 0, "{d}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
