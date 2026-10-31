//! Linux 2.6.16 EABI syscall numbers and the small safe helpers the loader
//! needs.  The raw syscalls themselves live in `raw`.

use crate::raw::{self, Mem};

// ARM private syscall numbers, 2.6.16 EABI.
pub(crate) const SYS_EXIT: usize = 1;
pub(crate) const SYS_READ: usize = 3;
pub(crate) const SYS_WRITE: usize = 4;
pub(crate) const SYS_OPEN: usize = 5;
pub(crate) const SYS_CLOSE: usize = 6;
pub(crate) const SYS_CACHEFLUSH: usize = 0x000f_0002;
pub(crate) const SYS_UNAME: usize = 122;
pub(crate) const SYS_GETUID32: usize = 199;
pub(crate) const SYS_GETGID32: usize = 200;
pub(crate) const SYS_SETUID32: usize = 213;
pub(crate) const SYS_SETGID32: usize = 214;
pub(crate) const SYS_SOCKET: usize = 281;
pub(crate) const SYS_SENDMSG: usize = 296;
pub(crate) const SYS_MMAP2: usize = 192;
pub(crate) const SYS_CAPGET: usize = 184;

pub(crate) const AF_INET: usize = 2;
pub(crate) const SOCK_DGRAM: usize = 2;
pub(crate) const MSG_PROBE: usize = 0x10;
pub(crate) const MSG_MORE: usize = 0x8000;

pub(crate) const PROT_READ: usize = 1;
pub(crate) const PROT_WRITE: usize = 2;
pub(crate) const MAP_SHARED: usize = 1;
pub(crate) const MAP_FIXED: usize = 0x10;

pub(crate) const O_RDWR: usize = 2;

pub(crate) fn write(fd: usize, buf: &[u8]) {
    raw::write(fd, buf);
}

pub(crate) fn print(s: &[u8]) {
    write(1, s);
}

pub(crate) fn print_str(s: &str) {
    print(s.as_bytes());
}

pub(crate) fn print_hex(v: u32) {
    let mut b = [0u8; 11];
    b[0] = b'0';
    b[1] = b'x';
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for i in 0..8 {
        b[2 + i] = HEX[((v >> (28 - 4 * i)) & 15) as usize];
    }
    b[10] = b'\n';
    print(&b);
}

pub(crate) fn print_dec(mut v: u32) {
    let mut b = [0u8; 12];
    let mut i = 11;
    b[i] = b'\n';
    if v == 0 {
        i -= 1;
        b[i] = b'0';
    }
    while v != 0 {
        i -= 1;
        b[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    print(&b[i..]);
}

pub(crate) fn str_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

pub(crate) fn parse_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    let mut i = 0;
    let mut base = 10u32;
    if s.len() > 2 && s[0] == b'0' && (s[1] == b'x' || s[1] == b'X') {
        base = 16;
        i = 2;
    }
    if i >= s.len() {
        return None;
    }
    while i < s.len() {
        let c = s[i];
        let d = match c {
            b'0'..=b'9' => (c - b'0') as u32,
            b'a'..=b'f' => (c - b'a' + 10) as u32,
            b'A'..=b'F' => (c - b'A' + 10) as u32,
            _ => return None,
        };
        if d >= base {
            return None;
        }
        v = v.checked_mul(base)?.checked_add(d)?;
        i += 1;
    }
    Some(v)
}

/// Read `path` into `mem`, at most `max` bytes.  The file is opened
/// read-only and closed before returning.
pub(crate) fn read_file(path: &[u8], mem: &Mem, max: u32) -> Option<u32> {
    if max > mem.len() {
        return None;
    }
    print_str("sideloader: opening file\n");
    let fd = raw::open(path, 0);
    if fd < 0 {
        print_str("sideloader: open failed\n");
        return None;
    }
    let mut got: u32 = 0;
    let mut report: u32 = 0;
    // A single read of several MiB into the /dev/mem mapping hangs on the
    // stock 2.6.16 kernel; read in bounded chunks instead.
    const CHUNK: u32 = 0x10000;
    loop {
        if got >= max {
            raw::close(fd as usize);
            return None;
        }
        let want = if max - got < CHUNK { max - got } else { CHUNK };
        let n = raw::read(fd as usize, mem, got, want);
        if n < 0 {
            print_str("sideloader: read failed\n");
            raw::close(fd as usize);
            return None;
        }
        if n == 0 {
            break;
        }
        got += n as u32;
        if got >= report {
            print_str("sideloader: read ");
            print_dec(got);
            print_str(" bytes\n");
            report = got + 0x0010_0000;
        }
    }
    print_str("sideloader: file read done\n");
    raw::close(fd as usize);
    Some(got)
}
