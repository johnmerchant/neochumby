//! What neochumbyd keeps, in its state directory:
//!
//!   identity          its private key (0600; the daemon's alone)
//!   paired/<key>      a paired Chumby: file named for its public key (hex),
//!                     holding its name
//!   pending/<id>      a Chumby asking to pair, under a random id: "code
//!                     name key address". At most MAX_PENDING at once,
//!                     none older than PENDING_TTL, and none kept from a
//!                     daemon before this one
//!   config/<name>     a Chumby's settings, key=value lines (`start=<app>`),
//!                     sent to it on every connection (`Msg::Config`). Kept
//!                     by name, so a Chumby paired again under its name
//!                     keeps them
//!   logs/, dev/       what the developer link keeps and is asked (dev.rs)
//!
//! The daemon writes pending/; `neochumbyd approve CODE` (whoever is in
//! the daemon's group) turns one into paired/, which the daemon, waiting on
//! the Chumby's connection, sees. Plain files: no socket between the two.

use std::io::{self, Read};
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::Duration;

use neochumby_proto::settings::{self, Settings};
use neochumby_proto::{Identity, PublicKey, hex, parse_code, unhex_key, valid_name};

/// The most Chumbys asking to pair at once: more are refused.
pub const MAX_PENDING: usize = 8;
/// How long a request waits to be approved or refused.
pub const PENDING_TTL: Duration = Duration::from_secs(600);

pub struct State {
    dir: PathBuf,
}

/// A request to pair.
pub struct Pending {
    /// Its file's name in pending/: random, so that no one asking can
    /// choose it or collide with another.
    pub id: String,
    /// The pairing code it showed (neochumby_proto::Unverified::code).
    pub code: String,
    pub name: String,
    pub key: PublicKey,
    pub addr: String,
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_string())
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn random_id() -> io::Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(hex(&b))
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

    /// The name `key` paired under, if it has.
    pub fn paired(&self, key: &PublicKey) -> Option<String> {
        let name = std::fs::read_to_string(self.dir.join("paired").join(hex(key))).ok()?;
        let name = name.trim();
        valid_name(name).then(|| name.to_string())
    }

    /// Keeps a request to pair, showing `code`: its id, for
    /// [`State::decision`] and [`State::withdraw`]. Refused while
    /// [`MAX_PENDING`] others wait.
    pub fn request(&self, code: &str, name: &str, key: &PublicKey, addr: SocketAddr) -> io::Result<String> {
        if parse_code(code).as_deref() != Some(code) || !valid_name(name) {
            return Err(bad("bad pairing request"));
        }
        if self.pending()?.len() >= MAX_PENDING {
            return Err(bad("too many Chumbys asking to pair at once"));
        }
        let id = random_id()?;
        let mut f =
            std::fs::OpenOptions::new().write(true).create_new(true).open(self.dir.join("pending").join(&id))?;
        io::Write::write_all(&mut f, format!("{code} {name} {} {}\n", hex(key), addr.ip()).as_bytes())?;
        Ok(id)
    }

    /// Some(true) once approved, Some(false) once refused, None meanwhile.
    pub fn decision(&self, id: &str, key: &PublicKey) -> Option<bool> {
        if self.paired(key).is_some() {
            Some(true)
        } else if !self.dir.join("pending").join(id).exists() {
            Some(false)
        } else {
            None
        }
    }

    /// Takes back the request `id`, as when no one answered it in time.
    pub fn withdraw(&self, id: &str) -> io::Result<()> {
        if !valid_id(id) {
            return Err(bad("not a request"));
        }
        match std::fs::remove_file(self.dir.join("pending").join(id)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }

    /// Forgets every request: on starting, as none of them has a connection
    /// waiting on it any more.
    pub fn clear_pending(&self) -> io::Result<()> {
        for e in std::fs::read_dir(self.dir.join("pending"))?.flatten() {
            std::fs::remove_file(e.path())?;
        }
        Ok(())
    }

    /// The requests waiting, removing any older than [`PENDING_TTL`].
    pub fn pending(&self) -> io::Result<Vec<Pending>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(self.dir.join("pending"))?.flatten() {
            let id = e.file_name().to_string_lossy().into_owned();
            let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            if age.is_some_and(|a| a > PENDING_TTL) {
                let _ = std::fs::remove_file(e.path());
                continue;
            }
            if let Some(p) = self.read_pending(&id) {
                out.push(p);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn read_pending(&self, id: &str) -> Option<Pending> {
        if !valid_id(id) {
            return None;
        }
        let s = std::fs::read_to_string(self.dir.join("pending").join(id)).ok()?;
        let mut f = s.split_whitespace();
        let (code, name, key, addr) = (f.next()?, f.next()?, f.next()?, f.next()?);
        Some(Pending {
            id: id.into(),
            code: code.into(),
            name: name.into(),
            key: unhex_key(key).ok()?,
            addr: addr.into(),
        })
    }

    /// The request showing `code`, as someone typed it.
    fn find(&self, code: &str) -> io::Result<Pending> {
        let code = parse_code(code).ok_or_else(|| bad("not a pairing code: type the 32 digits the Chumby shows"))?;
        self.pending()?.into_iter().find(|p| p.code == code).ok_or_else(|| bad("no Chumby is asking with that code"))
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

    /// Pairs the Chumby asking with `code`. A Chumby already paired under
    /// its name with another key is unpaired for it only if `replace`.
    pub fn approve(&self, code: &str, replace: bool) -> io::Result<String> {
        let p = self.find(code)?;
        let key = hex(&p.key);
        if let Some((_, old)) = self.pairs()?.into_iter().find(|(n, k)| n == &p.name && k != &key)
            && !replace
        {
            return Err(bad(&format!(
                "{} is already paired, with key {old}; to pair this one in its place, approve it with --replace",
                p.name
            )));
        }
        self.forget(&p.name)?;
        std::fs::write(self.dir.join("paired").join(key), format!("{}\n", p.name))?;
        std::fs::remove_file(self.dir.join("pending").join(&p.id))?;
        Ok(p.name)
    }

    pub fn refuse(&self, code: &str) -> io::Result<()> {
        let p = self.find(code)?;
        self.withdraw(&p.id)
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
        // Typed settings take only their own values.
        assert!(st.set("kitchen", "delivery", "usb").is_err());
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

    #[test]
    fn pairing() {
        let dir = std::env::temp_dir().join(format!("neochumbyd-pairing-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let st = State::open(dir.clone()).unwrap();
        let addr: SocketAddr = "192.0.2.7:4000".parse().unwrap();
        let code = |n: u8| format!("{n:02x}00-0000-0000-0000-0000-0000-0000-0000");
        let (k1, k2) = (Identity::generate().unwrap().public(), Identity::generate().unwrap().public());

        assert!(st.request("123456", "den", &k1, addr).is_err(), "a short code is no code");
        let id = st.request(&code(1), "den", &k1, addr).unwrap();
        assert!(valid_id(&id));
        assert_eq!(st.decision(&id, &k1), None);
        // Typed in capitals and without dashes, it is the same code.
        assert!(st.approve(&code(9), false).is_err(), "no one asks with it");
        assert_eq!(st.approve(&code(1).to_uppercase().replace('-', ""), false).unwrap(), "den");
        assert_eq!(st.decision(&id, &k1), Some(true));

        // Another key under the same name replaces it only when asked to.
        st.request(&code(2), "den", &k2, addr).unwrap();
        let e = st.approve(&code(2), false).unwrap_err().to_string();
        assert!(e.contains("--replace") && e.contains(&hex(&k1)), "{e}");
        assert_eq!(st.paired(&k1).as_deref(), Some("den"));
        st.approve(&code(2), true).unwrap();
        assert_eq!((st.paired(&k1), st.paired(&k2).as_deref()), (None, Some("den")));

        // Refused, and then gone.
        let id = st.request(&code(3), "attic", &k1, addr).unwrap();
        st.refuse(&code(3)).unwrap();
        assert_eq!(st.decision(&id, &k1), Some(false));

        // At most MAX_PENDING wait, and none for longer than PENDING_TTL.
        for n in 0..MAX_PENDING as u8 {
            st.request(&code(10 + n), "attic", &k1, addr).unwrap();
        }
        assert!(st.request(&code(99), "attic", &k1, addr).is_err());
        let old = std::time::SystemTime::now() - PENDING_TTL - Duration::from_secs(1);
        let first = st.pending().unwrap().remove(0);
        std::fs::File::options()
            .write(true)
            .open(dir.join("pending").join(&first.id))
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(st.pending().unwrap().len(), MAX_PENDING - 1);
        st.request(&code(99), "attic", &k1, addr).unwrap();
        st.clear_pending().unwrap();
        assert!(st.pending().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
