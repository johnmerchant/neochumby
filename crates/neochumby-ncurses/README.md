# neochumby-ncurses

**Experimental and unstable.** Any release may break anything, a 0.0.x
release included. This crate is built for
[neochumby](https://github.com/johnmerchant/neochumby), a new software
stack for the original Chumby Classic, not as a general-purpose library.
Its 0.0.1 is on crates.io, but it isn't published any more: the dist
images ship ncurses themselves.

ncurses, built from source (its own configure and make, in build.rs) as
a static libncursesw and libpanelw, for neochumby apps whose C programs
use curses. A crate that depends on it links them, and its build script
finds the headers in `DEP_NCURSESW_INCLUDE`. It knows two terminals,
`linux` (what neochumby's terminal is) and xterm-256color, compiled in:
there is no terminfo database on the Chumby.

With `NEOCHUMBY_NCURSES=system` it builds nothing, and links a shared
libncursesw and libpanelw instead, which `pkg-config` finds (in the
sysroot `PKG_CONFIG_SYSROOT_DIR` and `PKG_CONFIG_LIBDIR` name). neochumby's
`tools/tree` does so: the same ncurses,
configured the same way, shared, and carried in the tree's `lib/`
(`tools/linux-libs`).

## Building

The build needs `sh` and `make`, and a C compiler for the host as well as
the target (it builds ncurses' `tic` for the host first). For the Chumby,
neochumby uses zig (`zig cc`, through the repository's `tools/zigcc`).

## Licence

This crate is MIT. ncurses' sources, packaged in `third_party/ncurses`,
are under its X11-style licence (`third_party/ncurses/COPYING`); its
build files `install-sh` (X11), `configure` (FSFUL) and `config.guess`
and `config.sub` (GPL-3.0-or-later with the Autoconf exception) carry
their own notices.
