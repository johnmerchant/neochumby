//! Input devices (evdev): what an absolute axis, a touchscreen's X, Y or
//! pressure, holds now. The input core sends a reader an axis's value only
//! when it changes, so one opened since the last change never hears it:
//! it asks, with EVIOCGABS.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

/// EVIOCGABS(0), `_IOR('E', 0x40, struct input_absinfo)` with the struct as
/// 2.6.16 has it, five ints (value, minimum, maximum, fuzz, flat), 20
/// bytes. Later kernels add a sixth (resolution), and answer a request for
/// the first five as well, copying no more than it names.
const EVIOCGABS_0: u32 = (2 << 30) | (20 << 16) | ((b'E' as u32) << 8) | 0x40;

/// The value absolute axis `axis` (ABS_X is 0, ABS_Y 1, ABS_PRESSURE 0x18)
/// of the input device `dev` holds now: the last its driver reported.
pub fn abs_value(dev: &File, axis: u16) -> io::Result<i32> {
    if axis >= 0x40 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no such absolute axis"));
    }
    let mut info = [0i32; 5];
    // SAFETY: EVIOCGABS writes one input_absinfo of the size its request
    // names, 20 bytes, into `info`, ours and that long; the descriptor is
    // open, borrowed from `dev`.
    let r = unsafe { libc::ioctl(dev.as_raw_fd(), (EVIOCGABS_0 + u32::from(axis)) as libc::Ioctl, info.as_mut_ptr()) };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_linux_own() {
        // EVIOCGABS(ABS_X) as <linux/input.h> spells it for 2.6.16's
        // 20-byte input_absinfo.
        assert_eq!(EVIOCGABS_0, 0x8014_4540);
    }

    #[test]
    fn not_an_input_device_says_so() {
        let f = File::open("/dev/null").unwrap();
        assert!(abs_value(&f, 0).is_err());
        assert_eq!(abs_value(&f, 0x40).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }
}
