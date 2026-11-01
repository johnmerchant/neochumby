//! The Chumby's network, as its stock software left it in /psp/network_config:
//! one XML element, handed over at the sideload (neochumby-handoff).
//!
//! ```xml
//! <configuration type="wlan" ssid="..." auth="WPA2PSK" encryption="AES"
//!   key="..." encoding="ascii" allocation="dhcp" ip="" netmask="" gateway=""
//!   nameserver1="" nameserver2="" hwaddr="" />
//! ```

/// How the Chumby joins the network.
#[derive(Debug, PartialEq, Eq)]
pub struct Config {
    pub wireless: bool,
    pub ssid: String,
    pub security: Security,
    pub address: Address,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Security {
    Open,
    /// WPA or WPA2 with a key: a passphrase, or 64 hexadecimal digits.
    Psk {
        key: String,
        hex: bool,
    },
    /// WEP's key: 5 or 13 characters, or 10 or 26 hexadecimal digits.
    Wep {
        key: String,
        hex: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Address {
    Dhcp,
    Static { ip: String, netmask: String, gateway: String, nameservers: Vec<String> },
}

/// `name`'s value in the element `text`, with XML's escapes undone.
fn attribute(text: &str, name: &str) -> Option<String> {
    let mut rest = text;
    loop {
        let at = rest.find('=')?;
        let key = rest[..at].rsplit(|c: char| c.is_whitespace() || c == '<').next()?;
        let after = rest[at + 1..].trim_start();
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let end = after[1..].find(quote)?;
        if key == name {
            return Some(unescape(&after[1..1 + end]));
        }
        rest = &after[end + 2..];
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// What a static address must look like, as it goes on a command line.
fn is_address(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'.' || b == b':')
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        let get = |name: &str| attribute(text, name).unwrap_or_default();
        if !text.contains("<configuration") {
            return Err("no <configuration> element".into());
        }
        let wireless = get("type") != "lan";
        let ssid = get("ssid");
        if wireless && ssid.is_empty() {
            return Err("no ssid".into());
        }
        let key = get("key");
        let hex = get("encoding") == "hex";
        let security = match get("auth").as_str() {
            "OPEN" | "" if get("encryption") != "WEP" => Security::Open,
            "OPEN" | "" | "WEPAUTO" | "SHARED" => Security::Wep { key, hex },
            "WPAPSK" | "WPA2PSK" | "WPAAUTOPSK" => Security::Psk { key, hex },
            other => return Err(format!("auth {other}: not supported")),
        };
        let address = if get("allocation") == "static" {
            let (ip, netmask, gateway) = (get("ip"), get("netmask"), get("gateway"));
            let nameservers: Vec<String> =
                [get("nameserver1"), get("nameserver2")].into_iter().filter(|n| !n.is_empty()).collect();
            if ![&ip, &netmask].into_iter().all(|a| is_address(a))
                || !(gateway.is_empty() || is_address(&gateway))
                || !nameservers.iter().all(|n| is_address(n))
            {
                return Err("a static address that isn't one".into());
            }
            Address::Static { ip, netmask, gateway, nameservers }
        } else {
            Address::Dhcp
        };
        Ok(Config { wireless, ssid, security, address })
    }

    /// wpa_supplicant's configuration. Refuses what its quoted strings
    /// can't hold, rather than let a value write another line.
    pub fn wpa_conf(&self) -> Result<String, String> {
        let quoted = |what: &str, s: &str| {
            if s.chars().any(|c| c == '"' || c.is_control()) {
                Err(format!("{what} has a quote or a control character"))
            } else {
                Ok(format!("\"{s}\""))
            }
        };
        let hexed = |what: &str, s: &str| {
            if s.bytes().all(|b| b.is_ascii_hexdigit()) {
                Ok(s.to_owned())
            } else {
                Err(format!("{what} isn't hexadecimal"))
            }
        };
        let mut c = format!("network={{\n\tssid={}\n\tscan_ssid=1\n", quoted("ssid", &self.ssid)?);
        match &self.security {
            Security::Open => c.push_str("\tkey_mgmt=NONE\n"),
            Security::Psk { key, hex } => {
                let psk = if *hex { hexed("key", key)? } else { quoted("key", key)? };
                c.push_str(&format!("\tkey_mgmt=WPA-PSK\n\tpsk={psk}\n"));
            }
            Security::Wep { key, hex } => {
                let k = if *hex { hexed("key", key)? } else { quoted("key", key)? };
                c.push_str(&format!("\tkey_mgmt=NONE\n\twep_key0={k}\n\twep_tx_keyidx=0\n"));
            }
        }
        c.push_str("}\n");
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STOCK: &str = r#"<configuration gateway="" ip="" nameserver1="" encryption="AES" key="se&amp;cret pass" hwaddr="F6:92:BF:C5:C1:7A" nameserver2="" auth="WPA2PSK" netmask="" type="wlan" ssid="Packets-2G" allocation="dhcp" encoding="ascii" />"#;

    #[test]
    fn stocks_own_file() {
        let c = Config::parse(STOCK).unwrap();
        assert!(c.wireless);
        assert_eq!(c.ssid, "Packets-2G");
        assert_eq!(c.security, Security::Psk { key: "se&cret pass".into(), hex: false });
        assert_eq!(c.address, Address::Dhcp);
        assert_eq!(
            c.wpa_conf().unwrap(),
            "network={\n\tssid=\"Packets-2G\"\n\tscan_ssid=1\n\tkey_mgmt=WPA-PSK\n\tpsk=\"se&cret pass\"\n}\n"
        );
    }

    #[test]
    fn a_name_inside_a_value_isnt_an_attribute() {
        let c = Config::parse(r#"<configuration type="wlan" ssid="key=&quot;x" auth="OPEN" key="" />"#);
        assert_eq!(c.unwrap().ssid, "key=\"x");
    }

    #[test]
    fn a_value_cant_write_another_line() {
        let c = Config::parse("<configuration type=\"wlan\" ssid=\"a\nb\" auth=\"OPEN\" />").unwrap();
        assert!(c.wpa_conf().is_err());
        let c = Config::parse(r#"<configuration type="wlan" ssid="a&quot;}" auth="OPEN" />"#).unwrap();
        assert!(c.wpa_conf().is_err());
    }

    #[test]
    fn open_wep_static_and_wired() {
        let open = Config::parse(r#"<configuration type="wlan" ssid="x" auth="OPEN" encryption="NONE" />"#).unwrap();
        assert_eq!(open.security, Security::Open);
        let wep = Config::parse(
            r#"<configuration type="wlan" ssid="x" auth="WEPAUTO" encryption="WEP" key="0011223344" encoding="hex" />"#,
        )
        .unwrap();
        assert!(wep.wpa_conf().unwrap().contains("wep_key0=0011223344"));
        let wired = Config::parse(r#"<configuration type="lan" allocation="static" ip="10.0.0.2" netmask="255.0.0.0" gateway="10.0.0.1" nameserver1="10.0.0.1" nameserver2="" />"#).unwrap();
        assert!(!wired.wireless);
        assert_eq!(
            wired.address,
            Address::Static {
                ip: "10.0.0.2".into(),
                netmask: "255.0.0.0".into(),
                gateway: "10.0.0.1".into(),
                nameservers: vec!["10.0.0.1".into()]
            }
        );
    }

    #[test]
    fn refusals() {
        assert!(Config::parse("").is_err());
        assert!(Config::parse(r#"<configuration type="wlan" />"#).is_err());
        assert!(Config::parse(r#"<configuration type="wlan" ssid="x" auth="WPA2EAP" />"#).is_err());
        assert!(
            Config::parse(
                r#"<configuration type="lan" allocation="static" ip="1.1.1.1; reboot" netmask="255.0.0.0" />"#
            )
            .is_err()
        );
    }
}
