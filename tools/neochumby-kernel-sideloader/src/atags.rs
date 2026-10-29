//! ARM ATAGS for the 5.9.16 handoff.

const ATAG_CORE: u32 = 0x5441_0001;
const ATAG_MEM: u32 = 0x5441_0002;
const ATAG_INITRD2: u32 = 0x5442_0005;
const ATAG_CMDLINE: u32 = 0x5441_0009;

/// Build the ATAG list at `p`.
///
/// ARM ATAGS are a sequence of `{size, tag, payload...}` words; `ATAG_MEM` is
/// `{size, start}`, `ATAG_INITRD2` is `{start, size}`, and `ATAG_NONE` is two
/// zero words.  `ATAG_INITRD2` is an addition over the earlier C chainload,
/// because the initramfs is a separate file here.
pub(crate) unsafe fn build_atags(p: u32, initrd_start: u32, initrd_size: u32, cmdline: &[u8]) {
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
