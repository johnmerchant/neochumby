/* procps' config.h, which its configure would write, for top and its
 * library (libproc2) on static musl, without NLS, systemd or elogind:
 * build.rs builds third_party/procps with it. */

#define PACKAGE "procps-ng"
#define PACKAGE_NAME "procps-ng"
#define VERSION PROCPS_VERSION
#define PACKAGE_VERSION PROCPS_VERSION
#define PACKAGE_STRING "procps-ng " PROCPS_VERSION
#define PACKAGE_BUGREPORT "procps@freelists.org"

#define HAVE_ERR_H 1
#define HAVE_LANGINFO_H 1
#define HAVE_LOCALE_H 1
#define HAVE_PROGRAM_INVOCATION_SHORT_NAME 1
#define HAVE___PROGNAME 1
#define HAVE_STDIO_EXT_H 1
#define HAVE_UTMP_X 1
