//! The wx-feed daemon: the my-channels web page (python/configweb.py),
//! which neochumbyd runs once per server. Phones reach it from the QR code
//! in the Chumby's Settings; it edits the line-up and each app's options,
//! and, behind an admin login, pairs Chumbys and sets each one's shell and
//! delivery. This only starts the Python, in its place (exec), so the page
//! is this process and neochumbyd's signals reach it.
//!
//!   NEOCHUMBY_WX_WEB   the address to listen on, default 0.0.0.0:8080
//!
//! It shares the app's state directory with the companion (config.json,
//! which the renderer reads), and previews the renders (NEOCHUMBY_WX_OUT,
//! else neochumbyd's overlay). What the page needs from neochumbyd
//! (NEOCHUMBY_ADMIN, its admin socket, else the neochumbyd command) is
//! read by the Python (neochumbyd_admin.py).

mod py;

use std::io;
use std::os::unix::process::CommandExt;

/// "host:port" (or "[v6]:port") -> (host, port).
fn listen(addr: &str) -> Option<(String, u16)> {
    let (host, port) = addr.trim().rsplit_once(':')?;
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    Some((if host.is_empty() { "0.0.0.0" } else { host }.to_string(), port.parse().ok()?))
}

fn run() -> io::Error {
    let addr = std::env::var("NEOCHUMBY_WX_WEB").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let Some((host, port)) = listen(&addr) else {
        return io::Error::other(format!("NEOCHUMBY_WX_WEB: {addr:?} is not host:port"));
    };
    let state = py::state_dir();
    let mut cmd = match py::command("configweb.py") {
        Ok(c) => c,
        Err(e) => return e,
    };
    cmd.arg("--bind")
        .arg(host)
        .arg("--port")
        .arg(port.to_string())
        .arg("--config")
        .arg(state.join("config.json"))
        .arg("--out")
        .arg(py::out_template(std::env::var("NEOCHUMBY_WX_OUT").ok().as_deref(), &state))
        .exec()
}

fn main() {
    let e = run();
    eprintln!("neochumby-wx-feed-web: {e}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::listen;

    #[test]
    fn parses_the_address() {
        assert_eq!(listen("0.0.0.0:8080"), Some(("0.0.0.0".into(), 8080)));
        assert_eq!(listen("[::]:80"), Some(("::".into(), 80)));
        assert_eq!(listen(":9000"), Some(("0.0.0.0".into(), 9000)));
        assert_eq!(listen("localhost"), None);
        assert_eq!(listen("host:http"), None);
    }
}
