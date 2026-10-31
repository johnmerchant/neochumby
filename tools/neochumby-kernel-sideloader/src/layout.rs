//! Physical and virtual addresses used by the loader.
//!
//! The i.MX21 SDRAM is at 0xc0000000.  The stock 2.6.16 kernel gets the QEMU
//! loader container at 0xc2000000; that area is still free at rcS (verified),
//! and the stock kernel's direct map covers it.  User mappings use low aliases
//! because 0xc0000000 is above the 3 GiB user/kernel split.  Every physical
//! address here is SDRAM, not the NAND controller or an MTD device.

pub(crate) const SRC_PHYS: u32 = 0xc200_0000;
pub(crate) const SRC_VA: u32 = 0x0200_0000;
// Matches mkpayload.py's LIMIT: the container and the kernel copied out of
// it get 16 MiB, from 0xc2000000 to 0xc3000000.  The initramfs mapping
// starts there, so it never overlaps the source mapping (which would make
// MAP_FIXED replace part of it).
pub(crate) const SRC_MAP_LEN: u32 = 0x0100_0000;

pub(crate) const KERNEL_DST_VA: u32 = 0x0200_0000;
pub(crate) const KERNEL_MAP_LEN: u32 = 0x0100_0000;

// The 15 MiB between the kernel window and ATAGS at 0xc3f00000.
pub(crate) const INITRD_DST_PHYS: u32 = 0xc300_0000;
pub(crate) const INITRD_DST_VA: u32 = 0x0300_0000;
pub(crate) const INITRD_MAP_LEN: u32 = 0x00f0_0000;

// The earlier C chainload's ATAGS/stub addresses.  The stock kernel's direct
// map covers all 64 MiB, so the stub executes here with the MMU on and
// continues at the same physical address after the MMU is disabled.
pub(crate) const ATAGS_PHYS: u32 = 0xc3f0_0000;
pub(crate) const ATAGS_VA: u32 = 0x03f0_0000;
pub(crate) const STUB_PHYS: u32 = 0xc3f8_0000;
pub(crate) const STUB_VA: u32 = 0x03f8_0000;

pub(crate) const PAGE: u32 = 0x1000;

pub(crate) const DEFAULT_CMDLINE: &[u8] = b"console=ttymxc0,115200 rdinit=/init neochumby.sideloaded=1";
