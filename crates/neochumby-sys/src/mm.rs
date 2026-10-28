//! A device's memory mapped into the process, such as the framebuffer.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::ptr::NonNull;

/// `len` bytes of a device file mapped shared, read and write (mmap(2) at
/// offset 0), and the file that keeps them. Dropping it closes the file;
/// the memory stays mapped until the process exits.
///
/// For device memory (/dev/fb0) only, not a regular file: a regular file
/// can be truncated under a mapping, and then touching the memory past its
/// end raises SIGBUS. What another process writes to the same device's
/// memory shows up here as changed values, which for plain integers is
/// any bit pattern and so always valid.
pub struct DeviceMap {
    ptr: NonNull<u8>,
    len: usize,
    // Kept open with the mapping, as the device expects.
    _file: File,
}

// SAFETY: the mapping belongs to the process, not to a thread, and only a
// `&mut DeviceMap` reaches its memory; moving the owner to another thread
// moves that exclusive access with it.
unsafe impl Send for DeviceMap {}

impl DeviceMap {
    /// Maps the first `len` bytes of `file`, which must be open for reading
    /// and writing.
    pub fn new(file: File, len: usize) -> io::Result<DeviceMap> {
        if len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "an empty mapping"));
        }
        let prot = libc::PROT_READ | libc::PROT_WRITE;
        // SAFETY: a null hint lets the kernel choose an address no Rust
        // object uses; the descriptor is open for the call.
        let p = unsafe { libc::mmap(std::ptr::null_mut(), len, prot, libc::MAP_SHARED, file.as_raw_fd(), 0) };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let ptr = NonNull::new(p.cast::<u8>()).ok_or_else(|| io::Error::other("mmap at address 0"))?;
        Ok(DeviceMap { ptr, len, _file: file })
    }

    /// The memory as 16-bit words; an odd last byte is left out.
    pub fn as_u16_mut(&mut self) -> &mut [u16] {
        // SAFETY: the mapping is `len` readable, writable bytes, page
        // aligned (so aligned for u16), never unmapped, and reached only
        // through this `&mut self`; every bit pattern is a valid u16.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().cast::<u16>(), self.len / 2) }
    }

    /// The memory as 32-bit words; up to three last bytes are left out.
    pub fn as_u32_mut(&mut self) -> &mut [u32] {
        // SAFETY: as for as_u16_mut; page alignment is also u32 alignment.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().cast::<u32>(), self.len / 4) }
    }
}
