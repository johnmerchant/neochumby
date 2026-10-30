//! QEMU loader container.
//!
//! The container is loaded at 0xc2000000 by `-device loader`.  It starts with
//! an 8-byte magic, a version and a header size, then offsets and sizes for
//! the zImage and initramfs.  The zImage is copied forward over the header;
//! that is safe because the header has been parsed and the destination is
//! below the source.

use crate::raw::Mem;
use crate::sys::{print_dec, print_str};

const MAGIC: &[u8; 8] = b"NEOCHUM1";
const HEADER_SIZE: u32 = 40;

/// Parse the container in `src`, copy the zImage to the container base and
/// the initramfs to `initrd`, and return their sizes.
pub(crate) fn load_container(src: &Mem, initrd: &Mem) -> Option<(u32, u32)> {
    let mut magic = [0u8; 8];
    src.read_bytes(0, &mut magic);
    if &magic != MAGIC {
        return None;
    }
    let version = src.read_u32(8);
    let header_size = src.read_u32(12);
    let kernel_off = src.read_u32(16);
    let kernel_size = src.read_u32(20);
    let initrd_off = src.read_u32(24);
    let initrd_size = src.read_u32(28);
    if version != 1 || header_size != HEADER_SIZE {
        return None;
    }
    print_str("sideloader: container kernel=");
    print_dec(kernel_size);
    print_str("sideloader: container initramfs=");
    print_dec(initrd_size);
    if kernel_size == 0
        || kernel_off < header_size
        || kernel_off.checked_add(kernel_size)? > src.len()
        || (initrd_size != 0 && (initrd_off < header_size || initrd_off.checked_add(initrd_size)? > src.len()))
    {
        return None;
    }
    src.copy_from(0, src, kernel_off, kernel_size);
    if initrd_size != 0 {
        initrd.copy_from(0, src, initrd_off, initrd_size);
    }
    if initrd_size != 0 {
        print_str("sideloader: copied kernel and initramfs\n");
    } else {
        print_str("sideloader: copied kernel (built-in initramfs)\n");
    }
    Some((kernel_size, initrd_size))
}
