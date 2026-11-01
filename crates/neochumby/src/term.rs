//! A terminal on the screen, for programs written for one: a
//! pseudo-terminal (neochumby_sys::pty) the program writes to, shown on the
//! screen in a console font (font.rs: Spleen, 5x8 by default, 64 by 30
//! characters) and a colour scheme (colors.rs: GitHub Dark by default).
//!
//! It is shown on the kernel's console ([`vt`]): the font and the colours are
//! loaded there, and the program's output is passed to it as it comes. `TERM`
//! is `linux`, which neochumby-ncurses has compiled in.
//!
//! There is no keyboard. The touchscreen is the mouse, for a program that
//! asks for one (xterm's mouse reporting, as curses' mousemask does): a
//! touch presses its left button on the character under it, reported in
//! the encoding the program asked for (SGR's, 1006, or X10's). To one that
//! hasn't asked, a tap can type something ([`Options::tap`]), such as the
//! key that quits it. CONTRIBUTING.md has the encoding.
//!
//! The session runs a text-mode app on a terminal of its own ([`serve`]),
//! so that the app needs no access to the screen. [`run`] is for an app
//! with the framebuffer that wants a terminal for a program of its own.

pub mod colors;
pub mod font;
mod mouse;
pub mod vt;

use std::fs::File;
use std::io;
use std::process::{Command, ExitStatus};
use std::time::Duration;

use neochumby_sys::pty;

pub use colors::Scheme;
pub use font::Font;
use mouse::{Button, Mouse};

use crate::fb;
use crate::input::Touch;

/// How often the touchscreen is read while no output arrives.
const FRAME: Duration = Duration::from_millis(40);

/// What the terminal's programs are told it is.
pub const TERM: &str = "linux";

/// How a terminal looks, and what a tap types.
#[derive(Clone)]
pub struct Options {
    pub font: Font,
    pub scheme: Scheme,
    /// What a tap types while the program hasn't asked for the mouse:
    /// `b"q"` quits top. Nothing, if empty.
    pub tap: Vec<u8>,
}

impl Default for Options {
    fn default() -> Options {
        let font = Font::builtin(font::DEFAULT).or_else(|| Font::parse(font::BUILTIN[0].1).ok());
        Options { font: font.expect("the built-in fonts parse"), scheme: Scheme::default(), tap: Vec::new() }
    }
}

/// The terminal's size in characters, columns then rows, in `font`: as many
/// as fit on the screen.
pub fn size(font: &Font) -> (u16, u16) {
    ((fb::W / font.width).max(1) as u16, (fb::H / font.height).max(1) as u16)
}

/// Runs `cmd` on the terminal, the whole screen, until the terminal closes:
/// when the program and anything it started that kept it have exited. Its
/// output stays on the screen. The app needs the framebuffer, the
/// touchscreen and `terminal` in its access.
pub fn run(cmd: Command) -> io::Result<ExitStatus> {
    run_tapping(cmd, b"")
}

/// [`run`], where a tap types `keys` while the program hasn't asked for
/// the mouse: `b"q"` quits top.
pub fn run_tapping(mut cmd: Command, keys: &[u8]) -> io::Result<ExitStatus> {
    let opts = Options { tap: keys.to_vec(), ..Options::default() };
    let (cols, rows) = size(&opts.font);
    let (master, slave) = pty::open(cols, rows)?;
    cmd.stdin(slave.try_clone()?).stdout(slave.try_clone()?).stderr(slave).env("TERM", TERM);
    let mut child = cmd.spawn()?;
    // With the Command go our copies of the slave: the terminal closes when
    // the program's do.
    drop(cmd);
    serve(master, &opts)?;
    child.wait()
}

/// Shows the terminal whose master end is `master` on the kernel's console
/// until it closes. A font the console can't take is logged, and the default
/// used; the program's terminal then keeps the size it was given. The session
/// runs it, as `neochumby terminal`, for each text-mode app.
pub fn serve(master: File, opts: &Options) -> io::Result<()> {
    if vt::fits(&opts.font) {
        return vt::relay(master, opts);
    }
    eprintln!(
        "neochumby: terminal: a font of {}x{} doesn't fit the kernel's console; using {}",
        opts.font.width,
        opts.font.height,
        font::DEFAULT
    );
    vt::relay(master, &Options { font: Options::default().font, ..opts.clone() })
}

/// Touches, as a program on a terminal is to get them.
struct Touches {
    /// The cell a touch is down on.
    down: Option<(usize, usize)>,
    cell: (usize, usize),
    size: (usize, usize),
}

impl Touches {
    fn new(font: &Font) -> Touches {
        let (cols, rows) = size(font);
        Touches { down: None, cell: (font.width, font.height), size: (cols.into(), rows.into()) }
    }

    /// What the program gets for `t`, given what it asked of the mouse:
    /// mouse reports, or `tap` as a tap ends if it asked for none.
    fn bytes(&mut self, t: Touch, mouse: &Mouse, tap: &[u8]) -> Vec<u8> {
        let (button, at) = match (t, self.down) {
            (Touch::Down { x, y, .. }, was) => {
                let at = (
                    (x.max(0) as usize / self.cell.0).min(self.size.0 - 1),
                    (y.max(0) as usize / self.cell.1).min(self.size.1 - 1),
                );
                self.down = Some(at);
                match was {
                    None => (Button::Press, at),
                    Some(was) if was != at => (Button::Drag, at),
                    Some(_) => return Vec::new(),
                }
            }
            (Touch::Up, Some(at)) => {
                self.down = None;
                (Button::Release, at)
            }
            (Touch::Up, None) => return Vec::new(),
        };
        match mouse.report(button, at.0, at.1) {
            Some(report) => report,
            None if mouse.mode == 0 && button == Button::Release => tap.to_vec(),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tap_types_until_the_mouse_is_asked_for() {
        let font = Font::builtin("spleen-8x16").unwrap();
        let mut t = Touches::new(&font);
        let mut mouse = Mouse::default();
        let down = |x, y| Touch::Down { x, y, raw: (0, 0) };
        assert!(t.bytes(down(20, 40), &mouse, b"q").is_empty());
        assert_eq!(t.bytes(Touch::Up, &mouse, b"q"), b"q");
        mouse.set(1000, true);
        mouse.set(1006, true);
        // Cell (2, 2), counted from 1 as 3;3.
        assert_eq!(t.bytes(down(20, 40), &mouse, b"q"), b"\x1b[<0;3;3M");
        // Moving within the cell is nothing; to another, a drag, which
        // 1000 doesn't report.
        assert!(t.bytes(down(23, 41), &mouse, b"q").is_empty());
        assert!(t.bytes(down(319, 239), &mouse, b"q").is_empty());
        assert_eq!(t.bytes(Touch::Up, &mouse, b"q"), b"\x1b[<0;40;15m");
        assert!(t.bytes(Touch::Up, &mouse, b"q").is_empty());
    }

    #[test]
    fn the_size_is_the_screens_in_the_font() {
        assert_eq!(size(&Options::default().font), (64, 30));
        assert_eq!(size(&Font::builtin("spleen-6x12").unwrap()), (53, 20));
    }
}
