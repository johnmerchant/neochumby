//! ALSA's mixer controls, from sound/asound.h: a control's value written
//! by its name through a card's /dev/snd/controlC<n>, as `amixer cset`
//! does. The stock 2.6.16 kernel has its own nodes for what the session
//! sets; a newer kernel's (the dist image's) drivers have ALSA controls in
//! their place, such as the speaker amplifier's switch.

use std::fs::OpenOptions;
use std::io;
use std::mem::size_of;
use std::os::fd::AsRawFd;

/// SNDRV_CTL_ELEM_IFACE_MIXER: a mixer control, as a card's are.
const IFACE_MIXER: libc::c_int = 2;
/// The cards a kernel numbers, at most (SNDRV_CARDS).
const CARDS: usize = 8;

/// struct snd_ctl_elem_id: which control, here by its name alone.
#[repr(C)]
struct ElemId {
    numid: libc::c_uint,
    iface: libc::c_int,
    device: libc::c_uint,
    subdevice: libc::c_uint,
    name: [u8; 44],
    index: libc::c_uint,
}

/// The value union of struct snd_ctl_elem_value. Its integer64 member
/// gives it a long long's alignment, as in C.
#[repr(C)]
union Values {
    integer: [libc::c_long; 128],
    integer64: [i64; 64],
    bytes: [u8; 512],
}

/// struct snd_ctl_elem_value.
#[repr(C)]
struct ElemValue {
    id: ElemId,
    indirect: libc::c_uint,
    value: Values,
    reserved: [u8; 128],
}

/// SNDRV_CTL_IOCTL_ELEM_WRITE: _IOWR('U', 0x13, struct snd_ctl_elem_value).
const ELEM_WRITE: u32 = 3 << 30 | (size_of::<ElemValue>() as u32) << 16 | (b'U' as u32) << 8 | 0x13;

/// Sets every channel of the mixer control `name` (a switch's 0 or 1, or a
/// volume's step) on the first card that has one. Fails with
/// [`io::ErrorKind::NotFound`] if no card has it.
pub fn set(name: &str, value: i64) -> io::Result<()> {
    let mut v = ElemValue {
        id: ElemId { numid: 0, iface: IFACE_MIXER, device: 0, subdevice: 0, name: [0; 44], index: 0 },
        indirect: 0,
        value: Values { integer: [value as libc::c_long; 128] },
        reserved: [0; 128],
    };
    let n = name.as_bytes();
    if n.len() >= v.id.name.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "an ALSA control's name is under 44 bytes"));
    }
    v.id.name[..n.len()].copy_from_slice(n);
    for card in 0..CARDS {
        let ctl = match OpenOptions::new().read(true).write(true).open(format!("/dev/snd/controlC{card}")) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        // SAFETY: ELEM_WRITE reads and writes one snd_ctl_elem_value through
        // the pointer, and `v` is one, laid out as the kernel's for this
        // target; the descriptor is open while `ctl` lives.
        if unsafe { libc::ioctl(ctl.as_raw_fd(), ELEM_WRITE as libc::Ioctl, &mut v) } == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        // ENOENT: this card hasn't the control.
        if e.kind() != io::ErrorKind::NotFound {
            return Err(e);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("no sound card has the control '{name}'")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn laid_out_as_the_kernels() {
        // 64 bytes of id, the indirect flag, the union at a long long's
        // alignment (512 bytes of longs on 32-bit, 1024 on 64-bit) and 128
        // reserved.
        let longs = 128 * size_of::<libc::c_long>();
        assert_eq!(size_of::<ElemId>(), 64);
        assert_eq!(size_of::<ElemValue>(), 72 + longs + 128);
        // The request as asound.h makes it on the Chumby (32-bit ARM) and on
        // a 64-bit host.
        assert_eq!(ELEM_WRITE, if longs == 512 { 0xc2c8_5513 } else { 0xc4c8_5513 });
    }
}
