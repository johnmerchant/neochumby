//! Raw ABI: the only module with `unsafe` code.
//!
//! This is a bare-metal `no_std` program.  It needs raw system calls, raw
//! memory access and inline assembly, none of which can be expressed in safe
//! Rust.  Everything unsafe lives here; the rest of the crate uses the safe
//! types and functions below.
//!
//! Every `unsafe` block has a `// SAFETY:` comment.  `Mem` is the only way
//! the rest of the crate touches memory: it bounds-checks offsets against a
//! mapping that `map_phys` or `page_zero` established.

#![allow(unsafe_code)]

use core::ffi::c_void;

use crate::layout::PAGE;
use crate::sys::{
    AF_INET, MAP_FIXED, MAP_SHARED, PROT_READ, PROT_WRITE, SOCK_DGRAM, SYS_CACHEFLUSH, SYS_CAPGET, SYS_CLOSE, SYS_EXIT,
    SYS_GETGID32, SYS_GETUID32, SYS_MMAP2, SYS_OPEN, SYS_READ, SYS_SENDMSG, SYS_SETGID32, SYS_SETUID32, SYS_SOCKET,
    SYS_UNAME, SYS_WRITE,
};

/// A mapped SDRAM region.
///
/// `Mem` is only built by `map_phys` (after the kernel confirms the mapping)
/// or `page_zero` (the ELF's writable page at virtual address 0).  Every
/// access checks its offset against `len`, so the rest of the crate cannot
/// walk off the mapping.
#[derive(Clone, Copy)]
pub(crate) struct Mem {
    va: u32,
    len: u32,
}

impl Mem {
    /// The page mapped at virtual address 0 by the ELF loader.
    pub(crate) fn page_zero() -> Mem {
        // link.ld puts a writable PT_LOAD at vaddr 0; the kernel maps it for
        // this ELF, which is the CVE-2009-2698 route.
        Mem { va: 0, len: PAGE }
    }

    pub(crate) fn len(&self) -> u32 {
        self.len
    }

    fn range(&self, offset: u32, len: u32) -> Option<u32> {
        let end = offset.checked_add(len)?;
        if end > self.len {
            return None;
        }
        self.va.checked_add(offset)
    }

    pub(crate) fn write_u32(&self, offset: u32, value: u32) {
        if let Some(addr) = self.range(offset, 4) {
            // SAFETY: `range` proved offset..offset+4 is inside the mapping,
            // and `Mem` is only built for a live mapping.
            unsafe { core::ptr::write_volatile(addr as *mut u32, value) }
        }
    }

    pub(crate) fn read_u32(&self, offset: u32) -> u32 {
        match self.range(offset, 4) {
            // SAFETY: `range` proved offset..offset+4 is inside the mapping.
            Some(addr) => unsafe { core::ptr::read_volatile(addr as *const u32) },
            None => 0,
        }
    }

    pub(crate) fn write_u8(&self, offset: u32, value: u8) {
        if let Some(addr) = self.range(offset, 1) {
            // SAFETY: `range` proved the byte is inside the mapping.
            unsafe { core::ptr::write_volatile(addr as *mut u8, value) }
        }
    }

    pub(crate) fn read_bytes(&self, offset: u32, dst: &mut [u8]) {
        if let Some(addr) = self.range(offset, dst.len() as u32) {
            let src = addr as *const u8;
            for (i, byte) in dst.iter_mut().enumerate() {
                // SAFETY: `range` proved the whole slice is inside the
                // mapping, so each byte is readable.
                *byte = unsafe { core::ptr::read_volatile(src.add(i)) };
            }
        }
    }

    pub(crate) fn write_bytes(&self, offset: u32, src: &[u8]) {
        if let Some(addr) = self.range(offset, src.len() as u32) {
            let dst = addr as *mut u8;
            for (i, byte) in src.iter().enumerate() {
                // SAFETY: `range` proved the whole slice is inside the
                // mapping, so each byte is writable.
                unsafe { core::ptr::write_volatile(dst.add(i), *byte) };
            }
        }
    }

    pub(crate) fn copy_from(&self, dst_offset: u32, src: &Mem, src_offset: u32, len: u32) {
        if let (Some(dst), Some(src)) = (self.range(dst_offset, len), src.range(src_offset, len)) {
            let d = dst as *mut u8;
            let s = src as *const u8;
            for i in 0..len as usize {
                // SAFETY: both ranges were checked; the two mappings may
                // overlap, so copy forward byte by byte rather than using
                // `copy_nonoverlapping`.
                unsafe {
                    core::ptr::write_volatile(d.add(i), core::ptr::read_volatile(s.add(i)));
                }
            }
        }
    }

    fn ptr_mut(&self, offset: u32, len: u32) -> Option<*mut u8> {
        self.range(offset, len).map(|addr| addr as *mut u8)
    }
}

// The compiler may emit calls to the C memory routines even in no_std.
// These are the C ABI entry points, so they keep the raw-pointer signatures.
// The program is single-threaded and each caller passes valid buffers.

#[no_mangle]
pub unsafe extern "C" fn memset(dst: *mut c_void, c: i32, n: usize) -> *mut c_void {
    let d = dst as *mut u8;
    let mut i = 0;
    while i < n {
        // SAFETY: the C contract requires dst to be valid for n bytes.
        unsafe { core::ptr::write_volatile(d.add(i), c as u8) };
        i += 1;
    }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let d = dst as *mut u8;
    let s = src as *const u8;
    let mut i = 0;
    while i < n {
        // SAFETY: the C contract requires src and dst to be valid for n
        // bytes and not to overlap.
        unsafe { core::ptr::write_volatile(d.add(i), core::ptr::read_volatile(s.add(i))) };
        i += 1;
    }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    if (dst as usize) <= (src as usize) {
        // SAFETY: the C contract requires src and dst to be valid for n
        // bytes; forward order is safe when dst <= src.
        unsafe { memcpy(dst, src, n) }
    } else {
        let d = dst as *mut u8;
        let s = src as *const u8;
        let mut i = n;
        while i > 0 {
            i -= 1;
            // SAFETY: the C contract requires src and dst to be valid for n
            // bytes; backward order is safe when dst > src.
            unsafe { core::ptr::write_volatile(d.add(i), core::ptr::read_volatile(s.add(i))) };
        }
        dst
    }
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    let x = a as *const u8;
    let y = b as *const u8;
    let mut i = 0;
    while i < n {
        // SAFETY: the C contract requires both buffers to be valid for n
        // bytes.
        let xb = unsafe { core::ptr::read_volatile(x.add(i)) };
        // SAFETY: as above.
        let yb = unsafe { core::ptr::read_volatile(y.add(i)) };
        if xb != yb {
            return xb as i32 - yb as i32;
        }
        i += 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn strlen(s: *const u8) -> usize {
    let p = s;
    let mut n = 0;
    loop {
        // SAFETY: the C contract requires s to point to a NUL-terminated
        // string.
        if unsafe { core::ptr::read_volatile(p.add(n)) } == 0 {
            break;
        }
        n += 1;
    }
    n
}

/// The initial process arguments, read from the stack pointer `_start` got.
pub(crate) struct Args {
    argc: usize,
    argv: *const *const u8,
    next: usize,
}

impl Args {
    /// # Safety
    /// `sp` must be the initial stack pointer passed to `_start` by the
    /// kernel: `argc` at `sp[0]`, `argv` starting at `sp[1]`, and each
    /// `argv[i]` a NUL-terminated string.
    pub(crate) unsafe fn new(sp: *const u32) -> Args {
        // SAFETY: the caller guarantees the initial stack layout.
        unsafe { Args { argc: *sp as usize, argv: sp.add(1) as *const *const u8, next: 1 } }
    }

    pub(crate) fn next(&mut self) -> Option<&'static [u8]> {
        if self.next >= self.argc {
            return None;
        }
        // SAFETY: argv[next] is one of the NUL-terminated strings the
        // kernel placed on the initial stack.
        let p = unsafe { *self.argv.add(self.next) };
        self.next += 1;
        // SAFETY: p points to a NUL-terminated string that lives for the
        // process's lifetime.
        Some(unsafe { cstr(p) })
    }
}

unsafe fn cstr(p: *const u8) -> &'static [u8] {
    let mut n = 0;
    loop {
        // SAFETY: the caller guarantees p is NUL-terminated.
        if unsafe { core::ptr::read_volatile(p.add(n)) } == 0 {
            break;
        }
        n += 1;
    }
    // SAFETY: the string is at least n bytes, and it lives for the life of
    // the process.
    unsafe { core::slice::from_raw_parts(p, n) }
}

#[inline(always)]
unsafe fn syscall6(n: usize, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> isize {
    let mut r0 = a;
    // SAFETY: `svc 0` with the Linux 2.6.16 EABI register convention.  The
    // caller passes arguments valid for the requested syscall.
    unsafe {
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
    }
    r0 as isize
}

#[inline(always)]
unsafe fn syscall3(n: usize, a: usize, b: usize, c: usize) -> isize {
    // SAFETY: forwards to syscall6 with zero for the unused arguments.
    unsafe { syscall6(n, a, b, c, 0, 0, 0) }
}

pub(crate) fn write(fd: usize, buf: &[u8]) -> isize {
    // SAFETY: write(2) reads the buffer for the duration of the call and
    // does not retain the pointer.
    unsafe { syscall3(SYS_WRITE, fd, buf.as_ptr() as usize, buf.len()) }
}

pub(crate) fn read(fd: usize, mem: &Mem, offset: u32, len: u32) -> isize {
    match mem.ptr_mut(offset, len) {
        // SAFETY: the pointer is inside the mapping and valid for len bytes;
        // read(2) writes at most len bytes and does not retain the pointer.
        Some(p) => unsafe { syscall3(SYS_READ, fd, p as usize, len as usize) },
        None => -1,
    }
}

pub(crate) fn open(path: &[u8], flags: usize) -> isize {
    let mut p = [0u8; 256];
    if path.len() + 1 > p.len() {
        return -1;
    }
    p[..path.len()].copy_from_slice(path);
    p[path.len()] = 0;
    // SAFETY: p is NUL-terminated and lives for the duration of the call;
    // open(2) does not retain it.
    unsafe { syscall3(SYS_OPEN, p.as_ptr() as usize, flags, 0) }
}

pub(crate) fn close(fd: usize) -> isize {
    // SAFETY: close(2) has no pointer arguments.
    unsafe { syscall3(SYS_CLOSE, fd, 0, 0) }
}

pub(crate) fn getuid() -> u32 {
    // SAFETY: getuid32 has no pointer arguments.
    unsafe { syscall3(SYS_GETUID32, 0, 0, 0) as u32 }
}

pub(crate) fn getgid() -> u32 {
    // SAFETY: getgid32 has no pointer arguments.
    unsafe { syscall3(SYS_GETGID32, 0, 0, 0) as u32 }
}

pub(crate) fn setuid(uid: u32) -> isize {
    // SAFETY: setuid32 has no pointer arguments.
    unsafe { syscall3(SYS_SETUID32, uid as usize, 0, 0) }
}

pub(crate) fn setgid(gid: u32) -> isize {
    // SAFETY: setgid32 has no pointer arguments.
    unsafe { syscall3(SYS_SETGID32, gid as usize, 0, 0) }
}

pub(crate) fn uname(buf: &mut [u8; 390]) -> isize {
    // SAFETY: the kernel writes `struct utsname` (390 bytes on this ABI)
    // into the buffer, which is valid for that many bytes.
    unsafe { syscall3(SYS_UNAME, buf.as_mut_ptr() as usize, 0, 0) }
}

#[repr(C)]
pub(crate) struct CapHeader {
    pub version: u32,
    pub pid: i32,
}

#[repr(C)]
pub(crate) struct CapData {
    pub effective: u32,
    pub permitted: u32,
    pub inheritable: u32,
}

pub(crate) fn capget(header: &mut CapHeader, data: &mut CapData) -> isize {
    // SAFETY: the kernel writes the capability header and data into the two
    // references, which are valid for the duration of the call.
    unsafe { syscall3(SYS_CAPGET, header as *mut CapHeader as usize, data as *mut CapData as usize, 0) }
}

pub(crate) fn map_phys(fd: usize, va: u32, pa: u32, len: u32) -> Option<Mem> {
    // SAFETY: mmap2 with MAP_FIXED at a low alias of SDRAM.  The kernel
    // either maps the requested range or returns an error; a successful
    // return is the address we asked for.
    let r = unsafe {
        syscall6(
            SYS_MMAP2,
            va as usize,
            len as usize,
            PROT_READ | PROT_WRITE,
            MAP_SHARED | MAP_FIXED,
            fd,
            (pa >> 12) as usize,
        )
    };
    if r as u32 == va {
        Some(Mem { va, len })
    } else {
        None
    }
}

pub(crate) fn socket_udp() -> isize {
    // SAFETY: socket(2) with fixed constants; no pointers.
    unsafe { syscall3(SYS_SOCKET, AF_INET, SOCK_DGRAM, 0) }
}

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

pub(crate) fn sendmsg_udp(fd: usize, port: u16, addr: u32, flags: usize) -> isize {
    let sin = SockaddrIn { family: AF_INET as u16, port, addr, zero: [0; 8] };
    let msg = Msghdr {
        name: &sin as *const SockaddrIn as *const u8,
        namelen: core::mem::size_of::<SockaddrIn>() as i32,
        iov: core::ptr::null(),
        iovlen: 0,
        control: core::ptr::null(),
        controllen: 0,
        flags: 0,
    };
    // SAFETY: sendmsg(2) reads the sockaddr and msghdr during the call;
    // both point to live stack locals and are not retained.
    unsafe { syscall3(SYS_SENDMSG, fd, &msg as *const Msghdr as usize, flags) }
}

pub(crate) fn cacheflush(start: u32, end: u32) -> isize {
    // SAFETY: the ARM private cacheflush syscall takes two address ranges;
    // no memory is accessed through them by the caller.
    unsafe { syscall3(SYS_CACHEFLUSH, start as usize, end as usize, 0) }
}

pub(crate) fn exit(code: usize) -> ! {
    // SAFETY: exit(2) does not return.
    unsafe { syscall3(SYS_EXIT, code, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

extern "C" {
    fn kernel_payload();
    static jump_blob_start: u8;
    static jump_blob_end: u8;
    static stub_blob_start: u8;
    static stub_blob_end: u8;
}

pub(crate) fn kernel_payload_addr() -> u32 {
    kernel_payload as *const () as usize as u32
}

unsafe fn blob(start: *const u8, end: *const u8) -> &'static [u8] {
    let len = end as usize - start as usize;
    // SAFETY: the linker symbols bracket a static .text blob.
    unsafe { core::slice::from_raw_parts(start, len) }
}

pub(crate) fn jump_blob() -> &'static [u8] {
    // SAFETY: jump_blob_start/end are the blob's bounds.
    unsafe { blob(&jump_blob_start, &jump_blob_end) }
}

pub(crate) fn stub_blob() -> &'static [u8] {
    // SAFETY: stub_blob_start/end are the blob's bounds.
    unsafe { blob(&stub_blob_start, &stub_blob_end) }
}

/// The process entry point, called by `_start` in `asm.rs`.
#[no_mangle]
pub unsafe extern "C" fn sideloader_main(sp: *const u32) -> ! {
    // SAFETY: _start passes the initial stack pointer.
    let args = unsafe { Args::new(sp) };
    crate::run(args)
}
