//! A Chumby's settings, as its owner sets them on its neochumbyd
//! (`neochumbyd set NAME KEY=VALUE`) and as [`Msg::Config`](crate::Msg::Config)
//! carries them: key/value pairs on the wire and in neochumbyd's files,
//! [`Settings`] in code.
//!
//! The keys the session acts on have typed fields. Any other key, and a
//! known key whose value isn't one of its own, is kept as a pair in
//! [`Settings::other`] and sent on as it came, so a newer neochumbyd's
//! setting reaches the Chumby even through a build that doesn't know it.

use std::fmt;

/// The keys with fields of their own.
pub mod key {
    /// The app the session opens first.
    pub const START: &str = "start";
    /// The default player (an app whose manifest says `player = true`).
    pub const PLAYER: &str = "player";
    /// The app whose transition plays between apps (one whose manifest
    /// names a `transition`).
    pub const TRANSITION: &str = "transition";
    /// How the apps reach the Chumby: [`Delivery`](super::Delivery).
    pub const DELIVERY: &str = "delivery";
    /// Taps and button presses from neochumbyd (crate::dev): `on` or `off`.
    pub const DEV: &str = "dev";
    /// `frontend_<app>=<frontend>`: the owner approves an app as `<app>`'s
    /// frontend ([`frontend`](super::frontend)).
    pub const FRONTEND: &str = "frontend_";
}

/// How the apps reach the Chumby (`delivery=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Delivery {
    /// Run from neochumbyd's tree over NFS: the default.
    #[default]
    Nfs,
    /// Run from a copy on the Chumby's stick, kept in step with the tree
    /// (neochumby-sync).
    Sync,
}

impl Delivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Delivery::Nfs => "nfs",
            Delivery::Sync => "sync",
        }
    }

    pub fn parse(s: &str) -> Option<Delivery> {
        match s {
            "nfs" => Some(Delivery::Nfs),
            "sync" => Some(Delivery::Sync),
            _ => None,
        }
    }
}

impl fmt::Display for Delivery {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `dev=`'s values.
const ON: &str = "on";
const OFF: &str = "off";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    /// `start`: the app the session opens first, if set. Trusted only as
    /// far as the tree goes: the session checks the app is there.
    pub start: Option<String>,
    /// `player`: the default player, if set; checked like `start`.
    pub player: Option<String>,
    /// `transition`: the app animating switches between apps, if set;
    /// checked like `start`.
    pub transition: Option<String>,
    /// `delivery`, if set; unset is [`Delivery::Nfs`].
    pub delivery: Option<Delivery>,
    /// `dev=on`: the Chumby takes taps and button presses from its
    /// neochumbyd. Off unless set (crate::dev).
    pub dev: bool,
    /// Every other pair, in the order it came.
    pub other: Vec<(String, String)>,
}

impl Settings {
    /// From the wire's (or a file's) pairs. Pairs that aren't
    /// [`valid`] are dropped; of a key given twice, the first counts, as
    /// it always has.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Settings {
        let mut s = Settings::default();
        let mut seen: Vec<&'static str> = Vec::new();
        for (k, v) in pairs {
            if !valid(&k, &v) {
                continue;
            }
            let known =
                [key::START, key::PLAYER, key::TRANSITION, key::DELIVERY, key::DEV].into_iter().find(|n| *n == k);
            // A known key's first value is taken if it is one of its own.
            let took = match known {
                Some(n) if !seen.contains(&n) => {
                    seen.push(n);
                    match (n, Delivery::parse(&v)) {
                        (key::START, _) if crate::valid_name(&v) => {
                            s.start = Some(v.clone());
                            true
                        }
                        (key::PLAYER, _) if crate::valid_name(&v) => {
                            s.player = Some(v.clone());
                            true
                        }
                        (key::TRANSITION, _) if crate::valid_name(&v) => {
                            s.transition = Some(v.clone());
                            true
                        }
                        (key::DELIVERY, Some(d)) => {
                            s.delivery = Some(d);
                            true
                        }
                        (key::DEV, _) if v == ON || v == OFF => {
                            s.dev = v == ON;
                            true
                        }
                        _ => false,
                    }
                }
                _ => false,
            };
            if !took {
                s.other.push((k, v));
            }
        }
        s
    }

    /// The pairs for the wire: the typed fields that are set (`dev` only
    /// when on), then [`Settings::other`].
    pub fn to_pairs(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut put = |k: &str, v: &str| out.push((k.to_string(), v.to_string()));
        if let Some(v) = &self.start {
            put(key::START, v);
        }
        if let Some(v) = &self.player {
            put(key::PLAYER, v);
        }
        if let Some(v) = &self.transition {
            put(key::TRANSITION, v);
        }
        if let Some(d) = self.delivery {
            put(key::DELIVERY, d.as_str());
        }
        if self.dev {
            put(key::DEV, ON);
        }
        out.extend(self.other.iter().cloned());
        out
    }

    /// Whether `key` may be set (or removed).
    pub fn check_key(key: &str) -> Result<(), String> {
        match valid(key, "x") {
            true => Ok(()),
            false => Err("bad setting: keys are 1-32 of a-z and _".into()),
        }
    }

    /// Whether `key=value` may be set: a [`valid`] pair, and for a key
    /// with a field, a value of its own. Else why not.
    pub fn check(key: &str, value: &str) -> Result<(), String> {
        Settings::check_key(key)?;
        if !valid(key, value) {
            return Err("bad setting: values are 1-128 of A-Z a-z 0-9 . : / _ -".into());
        }
        match key {
            key::START | key::PLAYER | key::TRANSITION if !crate::valid_name(value) => {
                Err(format!("{key} names an app: 1-32 of a-z, 0-9, _ and -"))
            }
            key::DELIVERY if Delivery::parse(value).is_none() => Err(format!("{key} is nfs or sync")),
            key::DEV if value != ON && value != OFF => Err(format!("{key} is {ON} or {OFF}")),
            _ if key.starts_with(key::FRONTEND) && !crate::valid_name(value) => {
                Err(format!("{key} names an app: 1-32 of a-z, 0-9, _ and -"))
            }
            _ => Ok(()),
        }
    }
}

/// The key that approves a frontend for `app` (`frontend_airplay`): `app`
/// with `-` written `_`, as keys have no `-`. The frontend's own manifest
/// names the app exactly (`frontend = "airplay"`).
pub fn frontend(app: &str) -> String {
    format!("{}{}", key::FRONTEND, app.replace('-', "_"))
}

/// A pair a Chumby keeps: a short lower-case key, and a value of the
/// characters its /psp files accept.
pub fn valid(key: &str, value: &str) -> bool {
    (1..=32).contains(&key.len())
        && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        && (1..=128).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b".:/_-".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn typed_fields_and_the_rest() {
        let s = Settings::from_pairs(pairs(&[
            ("start", "wx"),
            ("delivery", "sync"),
            ("dev", "on"),
            ("player", "somafm"),
            ("transition", "transitions"),
            ("theme", "dark"),
        ]));
        assert_eq!(s.start.as_deref(), Some("wx"));
        assert_eq!(s.player.as_deref(), Some("somafm"));
        assert_eq!(s.transition.as_deref(), Some("transitions"));
        assert_eq!(s.delivery, Some(Delivery::Sync));
        assert!(s.dev);
        assert_eq!(s.other, pairs(&[("theme", "dark")]));
        assert_eq!(
            s.to_pairs(),
            pairs(&[
                ("start", "wx"),
                ("player", "somafm"),
                ("transition", "transitions"),
                ("delivery", "sync"),
                ("dev", "on"),
                ("theme", "dark")
            ])
        );
        assert_eq!(Settings::from_pairs(s.to_pairs()), s);
        assert_eq!(Settings::from_pairs(vec![]), Settings::default());
    }

    #[test]
    fn nothing_is_lost() {
        // Values a field can't hold, and a second of a key, stay as pairs
        // and go on to the Chumby, which reads the first.
        let s = Settings::from_pairs(pairs(&[("delivery", "usb"), ("dev", "yes"), ("start", "home"), ("start", "wx")]));
        assert_eq!(s.delivery, None);
        assert!(!s.dev);
        assert_eq!(s.start.as_deref(), Some("home"));
        assert_eq!(s.other, pairs(&[("delivery", "usb"), ("dev", "yes"), ("start", "wx")]));
        assert_eq!(Settings::from_pairs(s.to_pairs()), s);
        // dev=off is the default, so it needs no pair.
        let off = Settings::from_pairs(pairs(&[("dev", "off")]));
        assert_eq!(off, Settings::default());
        // Pairs no Chumby keeps are dropped.
        let s = Settings::from_pairs(pairs(&[("Start", "wx"), ("start", "a b"), ("proto.version", "1"), ("x", "")]));
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn what_may_be_set() {
        let dev = |v: &str| Settings::from_pairs(pairs(&[("start", "wx"), ("dev", v)])).dev;
        assert!(dev("on"));
        assert!(!dev("off") && !dev("On") && !dev("yes"));
        assert!(!Settings::from_pairs(pairs(&[("develop", "on")])).dev);
        assert!(Settings::check("start", "wx").is_ok());
        assert!(Settings::check("start", "Wx").is_err());
        assert!(Settings::check("player", "../x").is_err());
        assert!(Settings::check("transition", "../x").is_err());
        assert!(Settings::check("transition", "transitions").is_ok());
        assert!(Settings::check("delivery", "sync").is_ok());
        assert!(Settings::check("delivery", "nfs").is_ok());
        assert!(Settings::check("delivery", "usb").is_err());
        assert!(Settings::check("dev", "on").is_ok());
        assert!(Settings::check("dev", "off").is_ok());
        assert!(Settings::check("dev", "On").is_err());
        assert!(Settings::check("theme", "dark").is_ok());
        assert!(Settings::check("frontend_airplay", "nowplaying").is_ok());
        assert!(Settings::check("frontend_airplay", "Now:Playing").is_err());
        assert_eq!(frontend("my-radio"), "frontend_my_radio");
        assert!(Settings::check("Home", "wx").is_err());
        assert!(Settings::check("theme", "a\nb=c").is_err());
        assert!(Settings::check("proto.version", "2").is_err(), "the protocol's own key");
    }
}
