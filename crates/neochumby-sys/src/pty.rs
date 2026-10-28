//! Pseudo-terminals: a pair of files, of which a program takes the far end
//! (the slave) as its terminal, and whoever holds the near end (the master)
//! reads what it writes and types to it.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};

/// A new pseudo-terminal of `cols` by `rows` characters: (master, slave).
/// Both are close-on-exec, so neither leaks into programs started later;
/// one given to a child as its stdin, stdout or stderr is dup'd for it.
/// Needs devpts (/dev/pts) mounted.
pub fn open(cols: u16, rows: u16) -> io::Result<(File, File)> {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    // Linux declares the termios and window size const, macOS mutable:
    // a mutable pointer passes for both, and neither writes through it.
    let size = std::ptr::from_ref(&size).cast_mut();
    // SAFETY: openpty(3) writes the two descriptors through the pointers,
    // which point to our two ints; the name and termios may be null, and
    // the window size is only read, from our struct.
    let r = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), size) };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openpty succeeded, so both are open descriptors that nothing
    // else owns: each File takes sole ownership of one.
    let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
    cloexec(master.as_raw_fd())?;
    cloexec(slave.as_raw_fd())?;
    Ok((master, slave))
}

/// Waits up to `ms` milliseconds for `master` to have something to read:
/// output, or the slave's last copy closing (a read then fails, EIO on
/// Linux). poll(2), not a thread: std's blocking primitives wait on futex
/// operations newer than the Chumby's kernel.
pub fn readable(master: &File, ms: u16) -> io::Result<bool> {
    let mut fd = libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: poll(2) reads and writes the one pollfd we pass, for the
    // count we give; the descriptor is open, owned by `master`.
    match unsafe { libc::poll(&mut fd, 1, ms.into()) } {
        -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => Ok(false),
        -1 => Err(io::Error::last_os_error()),
        n => Ok(n > 0),
    }
}

fn cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl(2) with F_SETFD and an int flag touches no memory of
    // ours; `fd` is open, owned by a File the caller holds.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::process::{Command, Stdio};

    use super::*;

    /// What a program prints on `slave`, read from the master until the
    /// slave's last copy closes (EIO on Linux, EOF elsewhere).
    fn run(cmd: &mut Command) -> String {
        let (mut master, slave) = open(64, 30).unwrap();
        let mut child = cmd
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave))
            .spawn()
            .unwrap();
        // The Command still holds the slave's copies: drop them.
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        while let Ok(n @ 1..) = master.read(&mut buf) {
            out.extend_from_slice(&buf[..n]);
        }
        child.wait().unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_program_sees_a_terminal_of_the_size_given() {
        // `stty size` reads the size of the terminal on its stdin: rows, then
        // columns. The terminal turns its "\n" into "\r\n".
        assert_eq!(run(Command::new("stty").arg("size")), "30 64\r\n");
    }

    #[test]
    fn its_output_reaches_the_master() {
        assert_eq!(run(Command::new("sh").args(["-c", "test -t 1 && echo tty"])), "tty\r\n");
    }
}
