//! Minimal Linux 2.6.16 EABI syscalls and the small string/printing helpers
//! the loader needs.

// ARM private syscall numbers, 2.6.16 EABI.
pub(crate) const SYS_EXIT: usize = 1;
pub(crate) const SYS_READ: usize = 3;
pub(crate) const SYS_WRITE: usize = 4;
pub(crate) const SYS_OPEN: usize = 5;
pub(crate) const SYS_CLOSE: usize = 6;
pub(crate) const SYS_CACHEFLUSH: usize = 0x0f00_02;
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

#[inline(always)]
pub(crate) unsafe fn syscall6(n: usize, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> isize {
    let mut r0 = a;
    core::arch::asm!(
        "svc 0",
        in("r7") n,
        inout("r0") r0,
        in("r1") b,
        in("r2") c,
        in("r3") d,
        in("r4") e,
        in("r5") f,
        options(nostack)
    );
    r0 as isize
}

#[inline(always)]
pub(crate) unsafe fn syscall3(n: usize, a: usize, b: usize, c: usize) -> isize {
    syscall6(n, a, b, c, 0, 0, 0)
}

pub(crate) unsafe fn write(fd: usize, buf: &[u8]) {
    syscall3(SYS_WRITE, fd, buf.as_ptr() as usize, buf.len());
}

pub(crate) unsafe fn print(s: &[u8]) {
    write(1, s);
}

pub(crate) unsafe fn print_str(s: &str) {
    print(s.as_bytes());
}

pub(crate) unsafe fn print_hex(v: u32) {
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

pub(crate) unsafe fn print_dec(mut v: u32) {
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

pub(crate) unsafe fn cstr_len(p: *const u8) -> usize {
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    n
}

pub(crate) unsafe fn cstr(p: *const u8) -> &'static [u8] {
    core::slice::from_raw_parts(p, cstr_len(p))
}

pub(crate) unsafe fn str_eq(a: &[u8], b: &[u8]) -> bool {
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

pub(crate) unsafe fn parse_u32(s: &[u8]) -> Option<u32> {
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

/// Read `path` into `dst`, at most `max` bytes.  The file is opened read-only
/// and closed before returning.
pub(crate) unsafe fn read_file(path: &[u8], dst: u32, max: u32) -> Option<u32> {
    let mut p = [0u8; 256];
    if path.len() + 1 > p.len() {
        return None;
    }
    let mut i = 0;
    while i < path.len() {
        p[i] = path[i];
        i += 1;
    }
    p[path.len()] = 0;
    let fd = syscall3(SYS_OPEN, p.as_ptr() as usize, 0, 0);
    if fd < 0 {
        return None;
    }
    let mut got: u32 = 0;
    loop {
        if got >= max {
            syscall3(SYS_CLOSE, fd as usize, 0, 0);
            return None;
        }
        let n = syscall3(SYS_READ, fd as usize, (dst + got) as usize, (max - got) as usize);
        if n < 0 {
            syscall3(SYS_CLOSE, fd as usize, 0, 0);
            return None;
        }
        if n == 0 {
            break;
        }
        got += n as u32;
    }
    syscall3(SYS_CLOSE, fd as usize, 0, 0);
    Some(got)
}
