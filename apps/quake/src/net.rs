//! net_udp.c is built with gethostbyname renamed to `chum_gethostbyname`
//! (build.rs). It looks up its own address from its host name, and crashed
//! when that failed, as it does for a Chumby whose name isn't in DNS: this
//! host's own name gives INADDR_ANY then, which will do. Any other name is
//! looked up as before.

use std::ffi::{CStr, c_char};

unsafe extern "C" {
    fn gethostbyname(name: *const c_char) -> *mut libc::hostent;
}

static mut ANY: libc::in_addr_t = libc::INADDR_ANY;
static mut ADDRS: [*mut c_char; 2] = [std::ptr::null_mut(); 2];
static mut SELF: libc::hostent = libc::hostent {
    h_name: std::ptr::null_mut(),
    h_aliases: std::ptr::null_mut(),
    h_addrtype: libc::AF_INET,
    h_length: size_of::<libc::in_addr_t>() as i32,
    h_addr_list: std::ptr::null_mut(),
};

/// # Safety
/// `name` is a NUL-terminated string that outlives the answer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chum_gethostbyname(name: *const c_char) -> *mut libc::hostent {
    let h = unsafe { gethostbyname(name) };
    let mut me = [0 as c_char; 256];
    if !h.is_null() || unsafe { libc::gethostname(me.as_mut_ptr(), me.len()) } != 0 {
        return h;
    }
    // A name that filled the buffer isn't terminated.
    me[me.len() - 1] = 0;
    if unsafe { CStr::from_ptr(me.as_ptr()) != CStr::from_ptr(name) } {
        return h;
    }
    unsafe {
        let addrs = &raw mut ADDRS;
        (*addrs)[0] = (&raw mut ANY).cast();
        let me = &raw mut SELF;
        (*me).h_name = name.cast_mut();
        (*me).h_addr_list = addrs.cast();
        me
    }
}
