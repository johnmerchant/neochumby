//! QEMU loader container.
//!
//! The container is loaded at 0xc2000000 by `-device loader`.  It starts with
//! an 8-byte magic, a version and a header size, then offsets and sizes for
//! the zImage and initramfs.  The zImage is copied forward over the header;
//! that is safe because the header has been parsed and the destination is
//! below the source.

use crate::layout::SRC_MAP_LEN;
use crate::runtime::copy;
use crate::sys::{print_dec, print_str};

const MAGIC: &[u8; 8] = b"NEOCHUM1";

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

/// Parse the container at `base_va`, copy the zImage to the container base
/// and the initramfs to `initrd_dst`, and return their sizes.
pub(crate) unsafe fn load_container(base_va: u32, initrd_dst: u32) -> Option<(u32, u32)> {
    let h = &*(base_va as *const Header);
    if &h.magic != MAGIC {
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
    print_str("sideloader: container kernel=");
    print_dec(kernel_size);
    print_str("sideloader: container initramfs=");
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
    print_str("sideloader: copied kernel and initramfs\n");
    Some((kernel_size, initrd_size))
}
