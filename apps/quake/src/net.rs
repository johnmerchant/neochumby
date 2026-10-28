//! net_udp.c is built with gethostbyname renamed to `chum_gethostbyname`
//! (build.rs). It looks up its own address from its host name, and crashed
//! when that failed, as it does for a Chumby whose name isn't in DNS: this
//! host's own name gives INADDR_ANY then, which will do. Any other name is
//! looked up as before.

use std::ffi::{CStr, c_char};

use crate::state;

unsafe extern "C" {
    fn gethostbyname(name: *const c_char) -> *mut libc::hostent;
}

/// The answer for this host's own name: a hostent whose one address is
/// INADDR_ANY. Boxed, so the pointers between its fields stay put.
pub struct OwnHost {
    any: libc::in_addr_t,
    addrs: [*mut c_char; 2],
    hostent: libc::hostent,
}

impl OwnHost {
    fn new() -> Box<OwnHost> {
        let mut h = Box::new(OwnHost {
            any: libc::INADDR_ANY,
            addrs: [std::ptr::null_mut(); 2],
            hostent: libc::hostent {
                h_name: std::ptr::null_mut(),
                h_aliases: std::ptr::null_mut(),
                h_addrtype: libc::AF_INET,
                h_length: size_of::<libc::in_addr_t>() as i32,
                h_addr_list: std::ptr::null_mut(),
            },
        });
        h.addrs[0] = (&raw mut h.any).cast();
        h.hostent.h_addr_list = h.addrs.as_mut_ptr();
        h
    }
}

/// This host's name, if it has one.
fn hostname() -> Option<Vec<u8>> {
    let mut buf = [0u8; 256];
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    // A name that filled the buffer isn't terminated: none is found then.
    Some(CStr::from_bytes_until_nul(&buf).ok()?.to_bytes().to_vec())
}

/// # Safety
/// `name` is a NUL-terminated string that outlives the answer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chum_gethostbyname(name: *const c_char) -> *mut libc::hostent {
    let h = unsafe { gethostbyname(name) };
    if !h.is_null() || hostname().as_deref() != Some(unsafe { CStr::from_ptr(name) }.to_bytes()) {
        return h;
    }
    let own = state().own_host.get_or_insert_with(OwnHost::new);
    own.hostent.h_name = name.cast_mut();
    &raw mut own.hostent
}
