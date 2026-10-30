/* The calls procps makes that the Chumby's Linux 2.6.16 doesn't have on
 * ARM: openat, fstatat and readlinkat (322 and on, which ARM got in
 * 2.6.17) and pselect6 (2.6.19). Without them top exits with 47 at once,
 * as it can't read /proc/self. build.rs includes this before each of top's
 * files, so that they call compat.c's functions in their place, which make
 * the real call and, where the kernel says ENOSYS, the same with the calls
 * 2.6.16 has.
 *
 * A macro on the command line wouldn't do: musl's headers send pselect and
 * fstatat to their 64-bit-time symbols, and a renamed declaration takes
 * the redirection with it. Here the headers come first, then the macros,
 * so only the calls are renamed. compat.c is C, built against the same
 * headers, so the structures it hands musl are laid out as musl's are. */

#include <fcntl.h>
#include <signal.h>
#include <sys/select.h>
#include <sys/stat.h>
#include <unistd.h>

int top_openat(int dirfd, const char *path, int flags, ...);
int top_fstatat(int dirfd, const char *restrict path, struct stat *restrict st, int flag);
ssize_t top_readlinkat(int dirfd, const char *restrict path, char *restrict buf, size_t size);
int top_pselect(int n, fd_set *restrict r, fd_set *restrict w, fd_set *restrict e, const struct timespec *restrict ts,
                const sigset_t *restrict mask);

#define openat top_openat
#define fstatat top_fstatat
#define readlinkat top_readlinkat
#define pselect top_pselect
