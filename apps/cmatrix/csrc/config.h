/* cmatrix's config.h, which its configure would write, for static musl on
 * Linux with neochumby-ncurses (ncursesw): build.rs builds
 * third_party/cmatrix with it. */

#define HAVE_NCURSES_H 1
#define HAVE_SYS_IOCTL_H 1
#define HAVE_UNISTD_H 1
#define HAVE_TERMIOS_H 1
#define HAVE_USE_DEFAULT_COLORS 1
#define HAVE_RESIZETERM 1
#define HAVE_WRESIZE 1
