//! ncurses (C, X11 licence, third_party/ncurses), built for the target as a
//! static libncursesw (build.rs), for apps whose C programs use curses: a
//! crate that depends on this one links it, and its build script finds the
//! headers in `DEP_NCURSESW_INCLUDE`. It knows one terminal, xterm-256color,
//! which is what neochumby's terminal (`neochumby::term`) says it is; there
//! is no terminfo database on the Chumby.
//!
//! **Experimental and unstable**: any release may break anything. Built
//! for [neochumby](https://github.com/johnmerchant/neochumby), and
//! published to start an SDK for it.
//!
//! Nothing in Rust: what links C against it declares the functions it
//! calls.
