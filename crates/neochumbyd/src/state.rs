//! What neochumbyd keeps, in its state directory:
//!
//!   identity          its private key (0600; the daemon's alone)
//!   paired/<key>      a paired Chumby: file named for its public key (hex),
//!                     holding its name
//!   pending/<code>    a Chumby asking to pair: "name key address"
//!   config/<name>     a Chumby's settings, key=value lines (`start=<app>`),
//!                     sent to it on every connection (`Msg::Config`). Kept
//!                     by name, so a Chumby paired again under its name
//!                     keeps them
//!   owner-token       the owner's token (0640, the state directory's
//!                     group), which an app's server program such as the
//!                     household web page must pass to change anything
//!                     through the admin socket (admin.rs)
//!   allow/<app>       what its owner allows an app's server program beyond
//!                     its sandbox, a word a line: `pairing`, the admin
//!                     socket (`neochumbyd allow <app> pairing`)
//!   apps/<app>        each app's own directory, for its companions and its
//!                     server program: the only part of the state directory
//!                     they can reach (confine.rs)
//!   logs/, dev/       what the developer link keeps and is asked (dev.rs)
//!
//! The daemon writes pending/; `neochumbyd approve <code>` (whoever is in
//! the daemon's group), or a server program through the admin socket, turns one
//! into paired/, which the daemon, waiting on the Chumby's connection,
//! sees. The command line and the admin socket share these functions.

use std::io::{self, Read};
use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use neochumby_proto::{Identity, PublicKey, hex, unhex_key, valid_name};

/// What the owner may allow an app's server program: the admin socket,
/// with which it pairs Chumbys and changes their settings.
pub const PAIRING: &str = "pairing";
const GRANTS: &[&str] = &[PAIRING];

#[derive(Clone)]
pub struct State {
    dir: PathBuf,
}

/// A request to pair.
pub struct Pending {
    pub code: String,
    pub name: String,
    pub key: PublicKey,
    pub addr: String,
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_string())
}

fn valid_code(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit())
}

impl State {
    pub fn open(dir: PathBuf) -> io::Result<State> {
        std::fs::create_dir_all(dir.join("paired"))?;
        std::fs::create_dir_all(dir.join("pending"))?;
        std::fs::create_dir_all(dir.join("config"))?;
        Ok(State { dir })
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

    /// The owner's token, made on first use: 16 random bytes as hex. The
    /// owner reads it with `neochumbyd token` and gives it to the household
    /// web page, which passes it on to approve or forget a pairing.
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

    /// What the owner allows `app` (`pairing`), in the order allowed.
    pub fn allowed(&self, app: &str) -> Vec<String> {
        if !valid_name(app) {
            return Vec::new();
        }
        let s = std::fs::read_to_string(self.dir.join("allow").join(app)).unwrap_or_default();
        s.lines().filter(|w| GRANTS.contains(w)).map(str::to_string).collect()
    }

    /// Allows `app` `what`, or with `on` false, no longer.
    pub fn allow(&self, app: &str, what: &str, on: bool) -> io::Result<()> {
        if !valid_name(app) {
            return Err(bad("not an app's name"));
        }
        if !GRANTS.contains(&what) {
            return Err(bad(&format!("only {} can be allowed", GRANTS.join(", "))));
        }
        let mut words = self.allowed(app);
        words.retain(|w| w != what);
        if on {
            words.push(what.into());
        }
        let dir = self.dir.join("allow");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(app);
        if words.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                r => r,
            };
        }
        let tmp = path.with_extension("new");
        std::fs::write(&tmp, words.iter().map(|w| format!("{w}\n")).collect::<String>())?;
        std::fs::rename(tmp, path)
    }

    /// Every app the owner has allowed something, and what.
    pub fn allowances(&self) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = std::fs::read_dir(self.dir.join("allow"))
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|app| {
                let w = self.allowed(&app);
                (app, w)
            })
            .filter(|(_, w)| !w.is_empty())
            .collect();
        out.sort();
        out
    }

    /// The name `key` paired under, if it has.
    pub fn paired(&self, key: &PublicKey) -> Option<String> {
        let name = std::fs::read_to_string(self.dir.join("paired").join(hex(key))).ok()?;
        let name = name.trim();
        valid_name(name).then(|| name.to_string())
    }

    pub fn request(&self, code: &str, name: &str, key: &PublicKey, addr: SocketAddr) -> io::Result<()> {
        if !valid_code(code) || !valid_name(name) {
            return Err(bad("bad pairing request"));
        }
        let mut f =
            std::fs::OpenOptions::new().write(true).create_new(true).open(self.dir.join("pending").join(code))?;
        io::Write::write_all(&mut f, format!("{name} {} {}\n", hex(key), addr.ip()).as_bytes())
    }

    /// Some(true) once approved, Some(false) once refused, None meanwhile.
    pub fn decision(&self, code: &str, key: &PublicKey) -> Option<bool> {
        if self.paired(key).is_some() {
            Some(true)
        } else if !self.dir.join("pending").join(code).exists() {
            Some(false)
        } else {
            None
        }
    }

    pub fn pending(&self) -> io::Result<Vec<Pending>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(self.dir.join("pending"))?.flatten() {
            let code = e.file_name().to_string_lossy().into_owned();
            if let Some(p) = self.read_pending(&code) {
                out.push(p);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn read_pending(&self, code: &str) -> Option<Pending> {
        if !valid_code(code) {
            return None;
        }
        let s = std::fs::read_to_string(self.dir.join("pending").join(code)).ok()?;
        let mut f = s.split_whitespace();
        let (name, key, addr) = (f.next()?, f.next()?, f.next()?);
        Some(Pending { code: code.into(), name: name.into(), key: unhex_key(key).ok()?, addr: addr.into() })
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

    /// Pairs the Chumby asking with `code`, in place of any other under its
    /// name.
    pub fn approve(&self, code: &str) -> io::Result<String> {
        let p = self.read_pending(code).ok_or_else(|| bad("no such request"))?;
        self.forget(&p.name)?;
        std::fs::write(self.dir.join("paired").join(hex(&p.key)), format!("{}\n", p.name))?;
        std::fs::remove_file(self.dir.join("pending").join(code))?;
        Ok(p.name)
    }

    pub fn refuse(&self, code: &str) -> io::Result<()> {
        if !valid_code(code) {
            return Err(bad("not a code"));
        }
        match std::fs::remove_file(self.dir.join("pending").join(code)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }

    pub fn forget(&self, name: &str) -> io::Result<()> {
        for (n, key) in self.pairs()? {
            if n == name {
                std::fs::remove_file(self.dir.join("paired").join(key))?;
            }
        }
        Ok(())
    }

    /// `name`'s settings, in the order they were set.
    pub fn config(&self, name: &str) -> Vec<(String, String)> {
        if !valid_name(name) {
            return Vec::new();
        }
        let s = std::fs::read_to_string(self.dir.join("config").join(name)).unwrap_or_default();
        s.lines()
            .filter_map(|l| l.split_once('='))
            .filter(|(k, v)| valid_setting(k, v))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Sets one of a paired Chumby's settings; an empty value removes it.
    pub fn set(&self, name: &str, key: &str, value: &str) -> io::Result<()> {
        if !self.pairs()?.iter().any(|(n, _)| n == name) {
            return Err(bad("no Chumby paired under that name"));
        }
        if !(value.is_empty() && valid_setting(key, "x")) && !valid_setting(key, value) {
            return Err(bad("bad setting: keys are a-z and _, values A-Z a-z 0-9 . : / _ -"));
        }
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

/// A setting the Chumby will take: a short lower-case key, and a value of
/// the characters its /psp files accept (neochumby::conf::valid_value).
fn valid_setting(key: &str, value: &str) -> bool {
    (1..=32).contains(&key.len())
        && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        && (1..=128).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b".:/_-".contains(&b))
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
        st.set("kitchen", "delivery", "sync").unwrap();
        st.set("kitchen", "start", "home").unwrap();
        assert_eq!(st.config("kitchen"), [("delivery".into(), "sync".into()), ("start".into(), "home".into())]);
        st.set("kitchen", "delivery", "").unwrap();
        assert_eq!(st.config("kitchen"), [("start".to_string(), "home".to_string())]);

        assert!(st.set("kitchen", "Home", "wx").is_err());
        assert!(st.set("kitchen", "start", "w x").is_err());
        assert!(st.set("kitchen", "start", "a\nb=c").is_err());
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
    fn allowances() {
        let dir = std::env::temp_dir().join(format!("neochumbyd-allow-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = State::open(dir.clone()).unwrap();
        assert!(st.allowed("web").is_empty());
        st.allow("web", PAIRING, true).unwrap();
        st.allow("web", PAIRING, true).unwrap();
        assert_eq!(st.allowed("web"), [PAIRING]);
        assert_eq!(st.allowances(), [("web".to_string(), vec![PAIRING.to_string()])]);
        assert!(st.allow("web", "root", true).is_err());
        assert!(st.allow("../x", PAIRING, true).is_err());
        assert!(st.allowed("../allow/web").is_empty());
        st.allow("web", PAIRING, false).unwrap();
        assert!(st.allowed("web").is_empty() && st.allowances().is_empty());
        st.allow("web", PAIRING, false).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
}
