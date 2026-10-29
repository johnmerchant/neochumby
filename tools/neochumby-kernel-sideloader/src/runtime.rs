//! Compiler runtime helpers and volatile copies.
//!
//! The binary is `no_std`, but the compiler may still emit calls to the C
//! memory routines.  These implementations are deliberately simple: the
//! program is a single-threaded loader and does not need optimized versions.

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

/// Copy between two physical/virtual addresses.
pub(crate) unsafe fn copy(dst: u32, src: u32, len: u32) {
    let d = dst as *mut u8;
    let s = src as *const u8;
    let mut i = 0;
    while i < len {
        core::ptr::write_volatile(d.add(i as usize), core::ptr::read_volatile(s.add(i as usize)));
        i += 1;
    }
}

/// Copy a byte slice to a raw destination, one volatile byte at a time.
pub(crate) unsafe fn copy_volatile(dst: *mut u8, src: *const u8, len: usize) {
    let mut i = 0;
    while i < len {
        core::ptr::write_volatile(dst.add(i), core::ptr::read_volatile(src.add(i)));
        i += 1;
    }
}
