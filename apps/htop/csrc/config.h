/* htop's config.h, which its configure would write, for static musl on
 * Linux with neochumby-ncurses (ncursesw) and none of the optional
 * libraries: build.rs builds third_party/htop with it. */

#define PACKAGE "htop"
#define PACKAGE_NAME "htop"
#define PACKAGE_TARNAME "htop"
#define PACKAGE_VERSION HTOP_VERSION
#define VERSION HTOP_VERSION
#define PACKAGE_STRING "htop " HTOP_VERSION
#define PACKAGE_BUGREPORT "htop@groups.io"
#define PACKAGE_URL "https://htop.dev/"
#define COPYRIGHT "(C) 2004-2019 Hisham Muhammad. (C) 2020-2026 htop dev team."

#define HTOP_LINUX 1
#define BUILD_STATIC 1
/* $HOME/.config/htop/htoprc, unless HTOPRC names one. */
#define CONFIGDIR "/.config"
#define SYSCONFDIR "/etc"
#define PROCDIR "/proc"

#define _XOPEN_SOURCE_EXTENDED 1
#define NCURSES_ENABLE_STDBOOL_H 1
#define HAVE_LIBNCURSESW 1
#define HAVE_CURSES_H 1
#define HAVE_TERM_H 1
#define HAVE_SET_ESCDELAY 1
#define HAVE_GETMOUSE 1

#define HAVE_STDLIB_H 1
#define HAVE_STRING_H 1
#define HAVE_STRINGS_H 1
#define HAVE_SYS_PARAM_H 1
#define HAVE_SYS_TIME_H 1
#define HAVE_SYS_UTSNAME_H 1
#define HAVE_UNISTD_H 1
#define MAJOR_IN_SYSMACROS 1

#define HAVE_OPENAT 1
#define HAVE_FSTATAT 1
#define HAVE_FACCESSAT 1
#define HAVE_READLINKAT 1
#define HAVE_MEMFD_CREATE 1
#define HAVE_CLOCK_GETTIME 1
#define HAVE_SCHED_GETSCHEDULER 1
#define HAVE_SCHED_SETSCHEDULER 1
#define HAVE_STRNLEN 1
#define HAVE_STRCHRNUL 1
#define HAVE_AFFINITY 1

#define HAVE_ATTR_ALLOC_SIZE 1
#define HAVE_ATTR_NONNULL 1
#define HAVE_ATTR_RETNONNULL 1
#define HAVE_BUILTIN_CTZ 1
