//! A text-mode app's terminal (neochumby::term): a pseudo-terminal whose
//! far end is the app's stdin and stdout, and whose near end a helper, this
//! binary again as `neochumby terminal`, shows on the screen and types
//! touches into, as root, until the app and whatever it started have
//! closed it. The app itself never has the screen or the touchscreen.
//!
//! Its look: the font the Chumby's settings name (`font=`, for every app),
//! else the one the app's manifest names, else Spleen 5x8; and the colours
//! the settings name (`colors=`, neochumby::term::colors: built in, the
//! owner's in the tree's themes/, or the tree's collection), else GitHub
//! Dark. A name that doesn't resolve is logged, and the default used.

use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use neochumby::apps::App;
use neochumby::term::{self, Font, Options, Scheme, colors, font};
use neochumby_sys::pty;

/// How long the helper has to go once the app has: it sees the terminal
/// close at once.
const LINGER: Duration = Duration::from_secs(1);

/// The owner's choices, from the Chumby's settings or its conf.
#[derive(Clone, Debug, Default)]
pub struct Look {
    pub font: Option<String>,
    pub colors: Option<String>,
}

/// The helper showing an app's terminal.
pub struct Terminal {
    helper: Child,
}

/// The font for `app`: the owner's, else its own, else the default.
fn font_for(app: &App, look: &Look) -> (String, Font) {
    for name in [look.font.as_deref(), app.font.as_deref()].into_iter().flatten() {
        match Font::named(name, &app.dir) {
            Ok(f) if neochumby::term::vt::fits(&f) => return (name.to_string(), f),
            Ok(f) => eprintln!(
                "neochumby: {}: font {name}: {}x{} doesn't fit the kernel's console; using {}",
                app.name,
                f.width,
                f.height,
                font::DEFAULT
            ),
            Err(e) => eprintln!("neochumby: {}: font {name}: {e}; using {}", app.name, font::DEFAULT),
        }
    }
    (font::DEFAULT.into(), Options::default().font)
}

/// A terminal for `app`, its helper started: the far end, for the app.
pub fn open(app: &App, root: &Path, look: &Look) -> io::Result<(Terminal, File)> {
    let (font, f) = font_for(app, look);
    let (cols, rows) = term::size(&f);
    let (master, slave) = pty::open(cols, rows)?;
    let colors = look.colors.clone().unwrap_or_else(|| colors::DEFAULT.into());
    let tap: String = app.tap.iter().map(|b| format!("{b:02x}")).collect();
    let exe = std::env::current_exe()?;
    eprintln!("neochumby: {}: a terminal of {cols}x{rows} in {font}, colours {colors}", app.name);
    let helper = Command::new(exe)
        .arg("terminal")
        .arg(&font)
        .arg(&app.dir)
        .arg(&colors)
        .arg(if tap.is_empty() { "-" } else { &tap })
        .env("NEOCHUMBY_ROOT", root)
        .stdin(master)
        .stdout(Stdio::null())
        .spawn()?;
    Ok((Terminal { helper }, slave))
}

impl Terminal {
    /// Once the app has gone with whatever it started: waits for the
    /// helper, which sees the terminal close, for up to [`LINGER`], then
    /// ends it.
    pub fn finish(mut self) {
        let deadline = Instant::now() + LINGER;
        while matches!(self.helper.try_wait(), Ok(None)) && Instant::now() < deadline {
            neochumby::sleep_ms(20);
        }
        if let Ok(None) = self.helper.try_wait() {
            eprintln!("neochumby: the terminal outlived its app; ending it");
            let _ = self.helper.kill();
        }
        let _ = self.helper.wait();
    }
}

/// `neochumby terminal FONT DIR COLORS TAP`: shows the terminal whose near
/// end is stdin until it closes, in FONT (a built-in, or a file in the
/// app's directory DIR) and COLORS (neochumby::term::colors, with the tree
/// at NEOCHUMBY_ROOT), with TAP, hex, typed for a tap ("-": nothing).
pub fn serve(args: &[String]) -> ! {
    let [font, dir, colors, tap] = args else {
        eprintln!("usage: neochumby terminal FONT DIR COLORS TAP");
        std::process::exit(2)
    };
    let mut opts = Options::default();
    match Font::named(font, &PathBuf::from(dir)) {
        Ok(f) => opts.font = f,
        Err(e) => eprintln!("neochumby: terminal: font {font}: {e}"),
    }
    let (scheme, why) = Scheme::chosen(Some(colors), &neochumby::apps::root());
    if let Some(e) = why {
        eprintln!("neochumby: terminal: colours {colors}: {e}; using {}", colors::DEFAULT);
    }
    opts.scheme = scheme;
    opts.tap = (0..tap.len() / 2).filter_map(|i| u8::from_str_radix(tap.get(2 * i..2 * i + 2)?, 16).ok()).collect();
    let master = match io::stdin().as_fd().try_clone_to_owned() {
        Ok(fd) => File::from(fd),
        Err(e) => {
            eprintln!("neochumby: terminal: stdin: {e}");
            std::process::exit(1)
        }
    };
    if let Err(e) = term::serve(master, &opts) {
        eprintln!("neochumby: terminal: {e}");
        std::process::exit(1);
    }
    std::process::exit(0)
}
