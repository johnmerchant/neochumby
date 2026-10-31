//! htop on the Chumby, on neochumby's terminal, with the touchscreen as its
//! mouse: a tap on a process selects it, on a column's heading sorts by it,
//! on the function keys along the bottom presses them, F10 quitting. htop
//! (C, GPL-2.0-or-later, third_party/htop) is linked in, its main() as
//! htop_main (build.rs), with neochumby-ncurses' curses, and the binary runs
//! itself on the terminal:
//!
//!   neochumby-htop               on the screen
//!   neochumby-htop htop [ARG..]  htop itself, on this terminal

// FFI to htop's main, so allowed unsafe code, as the games are.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int};
use std::process::Command;

// For its ncurses, which htop's objects need.
use neochumby_ncurses as _;

unsafe extern "C" {
    fn htop_main(argc: c_int, argv: *mut *const c_char) -> c_int;
}

fn htop(args: impl Iterator<Item = String>) -> ! {
    let args: Vec<CString> =
        std::iter::once("htop".to_string()).chain(args).map(|a| CString::new(a).unwrap_or_default()).collect();
    let mut argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    // SAFETY: htop's main(), renamed (build.rs), takes argc and argv as
    // the C runtime passes them: argc NUL-terminated strings, then a null.
    // It keeps pointers into them while it runs (getopt's optarg), and may
    // reorder the array (getopt_long permutes it), so argv is passed
    // mutable; it never writes the strings. args and argv outlive the
    // call, after which the process exits. It is called once, on the main
    // thread, before any other of its C runs, as a real main() would be.
    let code = unsafe { htop_main(args.len() as c_int, argv.as_mut_ptr()) };
    std::process::exit(code)
}

fn main() {
    neochumby::debug::install("neochumby-htop");
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("htop") {
        htop(args);
    }
    let Some(exe) = neochumby::apps::exe() else {
        eprintln!("neochumby-htop: can't find myself");
        std::process::exit(1);
    };
    let mut cmd = Command::new(exe);
    // UTF-8, for htop's tree and meters in line drawing (musl's C locale is
    // ASCII).
    cmd.arg("htop").env("LANG", "C.UTF-8");
    match cmd.status() {
        Ok(s) if !s.success() => eprintln!("neochumby-htop: htop: {s}"),
        Ok(_) => {}
        Err(e) => {
            eprintln!("neochumby-htop: {e}");
            std::process::exit(1);
        }
    }
}
