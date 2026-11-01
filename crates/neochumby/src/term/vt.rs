//! The kernel's console on the screen: fbcon's virtual terminal, tty1.
//!
//! A text-mode app's terminal is a pseudo-terminal of its own, and [`relay`]
//! passes what it writes to the console as it
//! comes, for the kernel to draw: the app never holds the console, so it
//! can't take it as its controlling terminal, switch it or change its
//! font. Watching the output on its way is how the session knows what the
//! program asked of the mouse, so a touch reaches it. What the console answers (cursor reports), and what a USB
//! keyboard types on it, go back to the program.
//!
//! The console is set up with BusyBox, which the dist image has: `stty`
//! for raw mode, and `loadfont` (KDFONTOP and the font's Unicode table)
//! for the font, which fbcon takes up to 32x32: a larger one of an app's own
//! isn't taken (term::serve uses the default). The colours go in its palette with `ESC ] P`. The
//! Linux console has 16 colours and draws its background and text in
//! colours 0 and 7, so a scheme's background and foreground take those
//! two there. It has no colour for the cursor, which it draws in reverse
//! video, so a scheme's cursor colour isn't used here.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::process::{Command, Stdio};

use neochumby_sys::pty;

use super::colors::Scheme;
use super::font::Font;
use super::mouse::Mouse;
use super::{FRAME, Options, Touches};
use crate::input::Touchscreen;

/// The console on the screen.
pub const TTY: &str = "/dev/tty1";
/// BusyBox, whose applets set the console up.
const BUSYBOX: &str = "/bin/busybox";
/// The largest glyph fbcon takes.
const MAX_SIDE: usize = 32;

/// Whether the kernel has a console on the screen: a virtual terminal
/// bound to fbcon, the framebuffer console.
pub fn present() -> bool {
    let fbcon = std::fs::read_dir("/sys/class/vtconsole").into_iter().flatten().flatten().any(|e| {
        let read = |f: &str| std::fs::read_to_string(e.path().join(f)).unwrap_or_default();
        read("name").contains("frame buffer") && read("bind").trim() == "1"
    });
    fbcon && std::fs::metadata(TTY).is_ok_and(|m| m.file_type().is_char_device())
}

/// Whether the console can take `font`.
pub fn fits(font: &Font) -> bool {
    font.width <= MAX_SIDE && font.height <= MAX_SIDE
}

/// `ESC ] P` for each of the 16 colours, the background in 0 and the
/// foreground in 7.
pub fn palette(s: &Scheme) -> Vec<u8> {
    let mut out = Vec::new();
    for (n, c) in s.palette.iter().enumerate() {
        let [r, g, b] = match n {
            0 => s.background,
            7 => s.foreground,
            _ => *c,
        };
        out.extend_from_slice(format!("\x1b]P{n:x}{r:02x}{g:02x}{b:02x}").as_bytes());
    }
    out
}

/// Hides the console's cursor, so that it doesn't blink over what an app
/// draws on the framebuffer. Nothing where there is no console.
pub fn quiet() {
    if present()
        && let Ok(mut tty) = OpenOptions::new().write(true).open(TTY)
    {
        let _ = tty.write_all(b"\x1b[0m\x1b[?25l");
    }
}

/// Runs a BusyBox applet, with `input` on its stdin; what went wrong.
fn busybox(args: &[&str], input: &[u8]) -> io::Result<()> {
    let mut child =
        Command::new(BUSYBOX).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input)?;
    }
    let out = child.wait_with_output()?;
    match out.status.success() {
        true => Ok(()),
        false => Err(io::Error::other(format!(
            "busybox {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

/// Puts the console in raw mode, with `opts`' font and colours, cleared.
/// A font it can't load is logged, and the console's own is kept.
fn prepare(tty: &mut File, opts: &Options) -> io::Result<()> {
    busybox(&["stty", "-F", TTY, "raw", "-echo"], b"")?;
    // A reset first: whatever the last program left (its colours, its
    // scrolling region) goes. Then the palette, before anything is drawn
    // in it: fbcon on a true-colour screen doesn't redraw for a new one.
    tty.write_all(b"\x1bc")?;
    tty.write_all(&palette(&opts.scheme))?;
    // On the foreground console, tty1 (loadfont finds it as /dev/tty0).
    if let Err(e) = busybox(&["loadfont"], opts.font.data()) {
        eprintln!("neochumby: terminal: {e}; keeping the console's font");
    }
    // The cursor too, as ncinit hides it during a boot.
    tty.write_all(b"\x1b[0m\x1b[?25h\x1b[H\x1b[2J")
}

/// What of the program's output the relay must act on.
#[derive(Default)]
struct Watch {
    mouse: Mouse,
    /// The program reset the console (RIS, or `ESC ] R`), and with it the
    /// palette: it goes in again.
    reset: bool,
}

impl vte::Perform for Watch {
    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, action: char) {
        if intermediates == b"?" && matches!(action, 'h' | 'l') {
            for p in params.iter() {
                self.mouse.set(p[0], action == 'h');
            }
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if intermediates.is_empty() && byte == b'c' {
            self.mouse = Mouse::default();
            self.reset = true;
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.first().is_some_and(|p| p.starts_with(b"R")) {
            self.reset = true;
        }
    }
}

/// Passes the terminal whose master end is `master` to the console, and
/// touches and the console's input to it, until it closes. The console is
/// left with its cursor hidden, for whatever draws on the screen next.
pub fn relay(mut master: File, opts: &Options) -> io::Result<()> {
    let mut tty = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(TTY)?;
    prepare(&mut tty, opts)?;
    let mut touch = Touchscreen::open();
    let mut touches = Touches::new(&opts.font);
    let mut watch = Watch::default();
    let mut parser = vte::Parser::new();
    let mut buf = [0u8; 4096];
    let result = loop {
        let mut open = true;
        match pty::readable(&master, FRAME.as_millis() as u16) {
            Ok(true) => match master.read(&mut buf) {
                Ok(n @ 1..) => {
                    parser.advance(&mut watch, &buf[..n]);
                    if let Err(e) = write_all(&mut tty, &buf[..n]) {
                        break Err(e);
                    }
                    if std::mem::take(&mut watch.reset) {
                        let _ = write_all(&mut tty, &palette(&opts.scheme));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // Closed: EIO on Linux.
                _ => open = false,
            },
            Ok(false) => {}
            Err(e) => break Err(e),
        }
        let mut typed = Vec::new();
        // The console's answers, and a keyboard's keys.
        while let Ok(n @ 1..) = tty.read(&mut buf) {
            typed.extend_from_slice(&buf[..n]);
        }
        for t in touch.poll() {
            typed.extend(touches.bytes(t, &watch.mouse, &opts.tap));
        }
        if !typed.is_empty() && open {
            let _ = master.write_all(&typed);
        }
        if !open {
            break Ok(());
        }
    };
    let _ = write_all(&mut tty, b"\x1b[0m\x1b[?25l");
    result
}

/// Writes all of `data` to the console, opened non-blocking, waiting while
/// it is busy drawing.
fn write_all(tty: &mut File, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        match tty.write(data) {
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => crate::sleep_ms(5),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_palette_takes_the_background_and_foreground() {
        let s = Scheme::default();
        let p = String::from_utf8(palette(&s)).unwrap();
        assert!(p.starts_with("\x1b]P0010409\x1b]P1ff7b72"), "{p:?}");
        assert!(p.contains("\x1b]P7e6edf3\x1b]P86e7681") && p.ends_with("\x1b]Pfffffff"), "{p:?}");
        assert_eq!(p.matches("\x1b]P").count(), 16);
    }

    #[test]
    fn the_relay_follows_the_mouse_and_resets() {
        let mut w = Watch::default();
        let mut p = vte::Parser::new();
        p.advance(&mut w, b"\x1b[?1000h\x1b[?1006h");
        assert_eq!((w.mouse.mode, w.mouse.sgr, w.reset), (1000, true, false));
        p.advance(&mut w, b"\x1bc");
        assert_eq!((w.mouse, w.reset), (Mouse::default(), true));
    }

    #[test]
    fn fbcon_takes_fonts_up_to_32x32() {
        assert!(fits(&Font::builtin("spleen-16x32").unwrap()));
        // PSF 2: magic, version, header's size, flags, glyphs, a glyph's
        // size, height and width.
        let mut psf = vec![0x72, 0xb5, 0x4a, 0x86];
        for word in [0u32, 32, 0, 1, 512, 64, 64] {
            psf.extend_from_slice(&word.to_le_bytes());
        }
        psf.resize(32 + 512, 0);
        assert!(!fits(&Font::parse(&psf).unwrap()));
    }
}
