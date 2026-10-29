//! neochumby-somafm: the Music screen (app.rs, screen.rs).
//!
//! On a host, `neochumby-somafm render FILES OUT [station N] [on] [volume
//! V] [night]` writes the screen as the Chumby would show it, a 320x240
//! RGB565 file, from the pictures in FILES (the crate's files/).

fn main() {
    #[cfg(target_arch = "arm")]
    std::process::exit(neochumby_somafm::device::app());
    #[cfg(not(target_arch = "arm"))]
    std::process::exit(host(&std::env::args().skip(1).collect::<Vec<_>>()));
}

#[cfg(not(target_arch = "arm"))]
fn host(args: &[String]) -> i32 {
    use neochumby_somafm::screen::{H, Screen, View, W};

    let [cmd, files, out, rest @ ..] = args else {
        eprintln!("usage: neochumby-somafm render FILES OUT [station N] [on] [volume V] [night]");
        return 2;
    };
    if cmd != "render" {
        eprintln!("neochumby-somafm: on a host, only `render`");
        return 2;
    }
    let mut v = View { station: 0, on: false, volume: neochumby_somafm::store::DEFAULT_VOLUME, night: false };
    let mut words = rest.iter();
    while let Some(w) = words.next() {
        let mut num = || words.next().and_then(|n| n.parse().ok());
        match w.as_str() {
            "station" => v.station = num().unwrap_or(0) as usize,
            "volume" => v.volume = num().unwrap_or(0).min(100),
            "on" => v.on = true,
            "night" => v.night = true,
            _ => {
                eprintln!("neochumby-somafm: {w}?");
                return 2;
            }
        }
    }
    let screen = match Screen::load(std::path::Path::new(files)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("neochumby-somafm: {e}");
            return 1;
        }
    };
    let mut px = vec![0u16; W * H];
    screen.draw(&v, &mut px);
    let bytes: Vec<u8> = px.iter().flat_map(|p| p.to_le_bytes()).collect();
    if let Err(e) = std::fs::write(out, bytes) {
        eprintln!("neochumby-somafm: {out}: {e}");
        return 1;
    }
    0
}
