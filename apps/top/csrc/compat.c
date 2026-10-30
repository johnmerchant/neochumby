/* compat.h's functions: each makes the real call and, where the kernel
 * says ENOSYS (the Chumby's 2.6.16), does the same with the calls it has.
 * A directory descriptor becomes its /proc/self/fd link, which top can
 * follow as it has /proc (`system`), and pselect becomes poll between two
 * changes of the signal mask. */

/* build.rs includes compat.h here too: the real calls, not ours. */
#undef openat
#undef fstatat
#undef readlinkat
#undef pselect

#include <errno.h>
#include <limits.h>
#include <poll.h>
#include <stdarg.h>
#include <stdio.h>

/* path, as seen from the directory dirfd, in buf; 0, or -1 if too long. */
static int at(int dirfd, const char *path, char *buf, size_t size) {
    int n;

    if (dirfd == AT_FDCWD || path[0] == '/')
        n = snprintf(buf, size, "%s", path);
    else
        n = snprintf(buf, size, "/proc/self/fd/%d/%s", dirfd, path);
    if (n < 0 || (size_t)n >= size) {
        errno = ENAMETOOLONG;
        return -1;
    }
    return 0;
}

int top_openat(int dirfd, const char *path, int flags, ...) {
    char p[PATH_MAX];
    mode_t mode = 0;
    int fd;

    if (flags & O_CREAT) {
        va_list ap;
        va_start(ap, flags);
        mode = va_arg(ap, mode_t);
        va_end(ap);
    }
    fd = openat(dirfd, path, flags, mode);
    if (fd >= 0 || errno != ENOSYS || at(dirfd, path, p, sizeof p))
        return fd;
    return open(p, flags, mode);
}

int top_fstatat(int dirfd, const char *restrict path, struct stat *restrict st, int flag) {
    char p[PATH_MAX];
    int r = fstatat(dirfd, path, st, flag);

    if (r == 0 || errno != ENOSYS || at(dirfd, path, p, sizeof p))
        return r;
    return flag & AT_SYMLINK_NOFOLLOW ? lstat(p, st) : stat(p, st);
}

ssize_t top_readlinkat(int dirfd, const char *restrict path, char *restrict buf, size_t size) {
    char p[PATH_MAX];
    ssize_t r = readlinkat(dirfd, path, buf, size);

    if (r >= 0 || errno != ENOSYS || at(dirfd, path, p, sizeof p))
        return r;
    return readlink(p, buf, size);
}

/* A signal that comes between the change of mask and poll waits for the
 * next key or the timeout, where pselect would end at once: to top, a
 * screen refreshed a few seconds late. musl's select is pselect6 too. */
int top_pselect(int n, fd_set *restrict r, fd_set *restrict w, fd_set *restrict e, const struct timespec *restrict ts,
                const sigset_t *restrict mask) {
    struct pollfd fds[FD_SETSIZE];
    sigset_t old;
    int i, k = 0, ms = -1, masked = 0, rc, ready = 0;

    rc = pselect(n, r, w, e, ts, mask);
    if (rc >= 0 || errno != ENOSYS)
        return rc;
    if (n > FD_SETSIZE)
        n = FD_SETSIZE;
    for (i = 0; i < n; i++) {
        short ev = (r && FD_ISSET(i, r) ? POLLIN : 0) | (w && FD_ISSET(i, w) ? POLLOUT : 0) |
                   (e && FD_ISSET(i, e) ? POLLPRI : 0);
        if (ev) {
            fds[k].fd = i;
            fds[k].events = ev;
            fds[k].revents = 0;
            k++;
        }
    }
    if (ts) {
        long long t = (long long)ts->tv_sec * 1000 + (ts->tv_nsec + 999999) / 1000000;
        ms = t > INT_MAX ? INT_MAX : (int)t;
    }
    if (mask)
        masked = sigprocmask(SIG_SETMASK, mask, &old) == 0;
    rc = poll(fds, k, ms);
    if (masked) {
        int err = errno;
        sigprocmask(SIG_SETMASK, &old, NULL);
        errno = err;
    }
    if (rc < 0)
        return rc;
    if (r)
        FD_ZERO(r);
    if (w)
        FD_ZERO(w);
    if (e)
        FD_ZERO(e);
    /* As select counts: each descriptor once for each set it is ready in. */
    for (i = 0; i < k; i++) {
        short rev = fds[i].revents;
        if (r && rev & (POLLIN | POLLHUP | POLLERR)) {
            FD_SET(fds[i].fd, r);
            ready++;
        }
        if (w && rev & (POLLOUT | POLLERR)) {
            FD_SET(fds[i].fd, w);
            ready++;
        }
        if (e && rev & POLLPRI) {
            FD_SET(fds[i].fd, e);
            ready++;
        }
    }
    return ready;
}
