//! What neochumbyd keeps, in its state directory:
//!
//!   identity          its private key (0600; the daemon's alone)
//!   paired/<key>      a paired Chumby: file named for its public key (hex),
//!                     holding its name
//!   pending/<code>    a Chumby asking to pair: "name key address"
//!
//! The daemon writes pending/; `neochumbyd approve <code>` (whoever is in
//! the daemon's group) turns one into paired/, which the daemon, waiting on
//! the Chumby's connection, sees. Plain files: no socket between the two.

use std::io;
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use neochumby_proto::{Identity, PublicKey, hex, unhex_key, valid_name};

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
}
