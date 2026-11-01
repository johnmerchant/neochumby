//! The line on home's screen that says how the Chumby is connected. It only
//! looks: nothing here changes the network, and a tap on it does nothing.
//!
//! The network's name comes from /psp/network_config, of which this reads
//! the `ssid` alone (the file holds the key too). Whether the interface is
//! up, and how strong the signal is, from the kernel. The address is what a
//! UDP socket would send from, which needs no unsafe code and sends nothing.

use std::fs;
use std::net::UdpSocket;

/// A documentation address (TEST-NET-1): only the route to it matters.
const SOMEWHERE: &str = "192.0.2.1:9";

/// What is known of the connection, as read once.
#[derive(Default, PartialEq, Debug)]
pub struct Status {
    pub wired: bool,
    pub up: bool,
    pub ssid: Option<String>,
    pub address: Option<String>,
    pub signal_dbm: Option<i32>,
}

impl Status {
    pub fn read() -> Status {
        let up = |i: &str| fs::read_to_string(format!("/sys/class/net/{i}/operstate")).is_ok_and(|s| s.trim() == "up");
        let (wifi, wired) = (up("wlan0"), up("eth0"));
        let config = fs::read_to_string("/psp/network_config").unwrap_or_default();
        Status {
            wired: wired && !wifi,
            up: wifi || wired,
            ssid: ssid(&config),
            address: address(),
            signal_dbm: fs::read_to_string("/proc/net/wireless").ok().and_then(|w| signal(&w)),
        }
    }

    /// The line, as short as the screen's 40 columns want it.
    pub fn line(&self) -> String {
        let name = self.ssid.as_deref();
        match (self.up, self.wired, &self.address) {
            (false, _, _) => match name {
                Some(n) => format!("joining {n}"),
                None => "no network".to_owned(),
            },
            (true, true, Some(a)) => format!("wired {a}"),
            (true, true, None) => "wired, getting an address".to_owned(),
            (true, false, None) => format!("{}, getting an address", name.unwrap_or("Wi-Fi")),
            (true, false, Some(a)) => {
                let mut l = format!("{} {a}", name.unwrap_or("Wi-Fi"));
                if let Some(s) = self.signal_dbm {
                    l.push_str(&format!(" {s}dBm"));
                }
                l
            }
        }
    }
}

/// The `ssid` attribute, printable: it goes on the terminal, where a control
/// character would be a command. None if there's none.
fn ssid(config: &str) -> Option<String> {
    let rest = &config[config.find(" ssid=\"")? + 7..];
    let value = &rest[..rest.find('"')?];
    let value = value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    let clean: String = value.chars().map(|c| if c.is_control() { '?' } else { c }).collect();
    (!clean.is_empty()).then_some(clean)
}

fn address() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect(SOMEWHERE).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then(|| ip.to_string())
}

/// wlan0's signal level in /proc/net/wireless: `wlan0: 0000 54. -56. -256 ...`.
/// Kernels without wireless extensions have no such file: no signal, then.
fn signal(wireless: &str) -> Option<i32> {
    let line = wireless.lines().find(|l| l.trim_start().starts_with("wlan0:"))?;
    let level = line.split_whitespace().nth(3)?;
    level.trim_end_matches('.').parse().ok().filter(|l| *l < 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STOCK: &str =
        r#"<configuration gateway="" key="secret" auth="WPA2PSK" type="wlan" ssid="Packets-2G" allocation="dhcp" />"#;

    #[test]
    fn the_name_and_never_the_key() {
        assert_eq!(ssid(STOCK).as_deref(), Some("Packets-2G"));
        assert_eq!(ssid(r#"<configuration key=" ssid=&quot;" />"#), None);
        assert_eq!(ssid(""), None);
        assert_eq!(ssid(r#"<configuration ssid="" />"#), None);
    }

    #[test]
    fn a_name_cant_command_the_terminal() {
        assert_eq!(ssid("<configuration ssid=\"a\u{1b}[2Jb\" />").as_deref(), Some("a?[2Jb"));
        assert_eq!(ssid(r#"<configuration ssid="R&amp;D" />"#).as_deref(), Some("R&D"));
    }

    #[test]
    fn the_signal_from_procfs() {
        let w =
            "Inter-| sta-|   Quality\n face | status | link level noise\n wlan0: 0000   54.  -56.  -256 0 0 0 0 0 0\n";
        assert_eq!(signal(w), Some(-56));
        assert_eq!(signal("Inter-|\n face |\n"), None);
        assert_eq!(signal(" wlan0: 0000 0 0 0"), None);
    }

    #[test]
    fn each_state_says_so() {
        let mut s = Status::default();
        assert_eq!(s.line(), "no network");
        s.ssid = Some("Packets-2G".into());
        assert_eq!(s.line(), "joining Packets-2G");
        s.up = true;
        assert_eq!(s.line(), "Packets-2G, getting an address");
        s.address = Some("10.20.40.18".into());
        assert_eq!(s.line(), "Packets-2G 10.20.40.18");
        s.signal_dbm = Some(-56);
        assert_eq!(s.line(), "Packets-2G 10.20.40.18 -56dBm");
        s.wired = true;
        assert_eq!(s.line(), "wired 10.20.40.18");
    }
}
