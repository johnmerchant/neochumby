//! WAMR's C interface, and the safe types over it: every `unsafe` block of
//! this crate is here.
//!
//! The invariants the types keep, which the `SAFETY` comments rely on:
//!
//! - **One runtime per process,** initialised by [`Runtime::init`] and never
//!   torn down (WAMR's state is global), and used from one thread: the types
//!   are neither `Send` nor `Sync`.
//! - **A [`Module`] owns the bytes it was loaded from,** which WAMR may
//!   rewrite and keeps pointers into until the module is unloaded, in its
//!   `Drop`.
//! - **An [`Instance`] borrows its module,** so it can't outlive it, and owns
//!   the host state its functions see, behind WAMR's custom-data pointer.
//! - **A host function's registration is leaked,** names, signature and
//!   all: WAMR keeps pointers to them for as long as the process runs.

#![allow(unsafe_code)]

use std::any::Any;
use std::ffi::{CStr, CString, c_char, c_void};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{Error, Func, Import, ImportKind, Trap};

use wamr_sys::{
    InstantiationArgs, NativeSymbol, WASMExecEnv, WASMModuleCommon, WASMModuleInstanceCommon,
    log_level_t_WASM_LOG_LEVEL_ERROR, package_type_t_Wasm_Module_AoT, wasm_exec_env_t, wasm_import_t,
    wasm_memory_get_cur_page_count, wasm_runtime_addr_app_to_native, wasm_runtime_call_wasm,
    wasm_runtime_clear_exception, wasm_runtime_create_exec_env, wasm_runtime_deinstantiate,
    wasm_runtime_destroy_exec_env, wasm_runtime_get_custom_data, wasm_runtime_get_default_memory,
    wasm_runtime_get_exception, wasm_runtime_get_function_attachment, wasm_runtime_get_import_count,
    wasm_runtime_get_import_type, wasm_runtime_get_module_inst, wasm_runtime_get_module_package_type,
    wasm_runtime_init, wasm_runtime_instantiate_ex, wasm_runtime_load, wasm_runtime_lookup_function,
    wasm_runtime_register_natives_raw, wasm_runtime_set_custom_data, wasm_runtime_set_exception,
    wasm_runtime_get_version, wasm_runtime_set_log_level, wasm_runtime_unload, wasm_runtime_validate_app_addr,
};

type InstT = wamr_sys::wasm_module_inst_t;
type ExecEnvT = wasm_exec_env_t;

unsafe extern "C" {
    /// WAMR's platform layer, which wasm_export.h (and so wamr-sys) leaves
    /// out: the calling thread's stack bounds, found once and kept.
    fn os_thread_get_stack_boundary() -> *mut u8;
}

/// The runtime's release, as WAMR reports it (`2.4.5`).
pub fn release() -> String {
    let (mut major, mut minor, mut patch) = (0u32, 0u32, 0u32);
    // SAFETY: writes three live u32s; needs no initialised runtime.
    unsafe { wasm_runtime_get_version(&mut major, &mut minor, &mut patch) };
    format!("{major}.{minor}.{patch}")
}

static INITIALISED: AtomicBool = AtomicBool::new(false);

/// WAMR, initialised: what loads modules and holds the host's functions.
pub struct Runtime {
    _one_thread: PhantomData<*const ()>,
}

impl Runtime {
    /// Initialises WAMR, with the system's allocator; once a process.
    pub fn init() -> Result<Runtime, Error> {
        if INITIALISED.swap(true, Ordering::SeqCst) {
            return Err(Error::Init("already initialised in this process".into()));
        }
        // SAFETY: called once a process (above), before anything else of
        // WAMR's.
        if !unsafe { wasm_runtime_init() } {
            return Err(Error::Init("wasm_runtime_init failed".into()));
        }
        // SAFETY: the runtime is initialised; this sets a global level.
        unsafe { wasm_runtime_set_log_level(log_level_t_WASM_LOG_LEVEL_ERROR) };
        // WAMR finds this thread's stack the first time a module runs, and
        // keeps it: glibc reads /proc/self/maps for that, which a process
        // locked down after loading its module can no longer open. Found
        // now, it is found once.
        // SAFETY: takes nothing; caches the calling thread's stack bounds.
        unsafe { os_thread_get_stack_boundary() };
        Ok(Runtime { _one_thread: PhantomData })
    }

    /// Makes `funcs` importable from module `module`, by every module loaded
    /// after this. Their registration lives as long as the process.
    pub fn register(&self, module: &str, funcs: &'static [Func]) -> Result<(), Error> {
        let mut symbols = Vec::with_capacity(funcs.len());
        for f in funcs {
            let name = CString::new(f.name).map_err(|_| Error::Init(format!("{}: a NUL in the name", f.name)))?;
            let sig = CString::new(f.signature()).expect("signatures have no NUL");
            symbols.push(NativeSymbol {
                symbol: name.into_raw(),
                func_ptr: trampoline as *mut c_void,
                signature: sig.into_raw(),
                attachment: f as *const Func as *mut c_void,
            });
        }
        let symbols: &'static mut [NativeSymbol] = symbols.leak();
        let module = CString::new(module).map_err(|_| Error::Init("a NUL in the module name".into()))?.into_raw();
        // SAFETY: the module name, the symbols and every string and
        // attachment they point at are leaked, so live as long as WAMR keeps
        // them; each attachment is a `&'static Func`, as `trampoline` reads
        // it.
        if !unsafe { wasm_runtime_register_natives_raw(module, symbols.as_mut_ptr(), symbols.len() as u32) } {
            return Err(Error::Init("registering the host's functions failed".into()));
        }
        Ok(())
    }

    /// Loads a module, bytecode or AOT (by its magic number), linking its
    /// imports against what is registered. An import nothing provides is
    /// left unlinked: see [`Module::imports`].
    pub fn load(&self, mut bytes: Vec<u8>) -> Result<Module<'_>, Error> {
        let size = u32::try_from(bytes.len()).map_err(|_| Error::Load("too large".into()))?;
        let mut err = [0 as c_char; 256];
        // SAFETY: `bytes` is writable and stays allocated, unmoved, for as
        // long as the Module that owns it (a Vec's buffer doesn't move when
        // the Vec does); `err` is a live buffer of the size given.
        let raw = unsafe { wasm_runtime_load(bytes.as_mut_ptr(), size, err.as_mut_ptr(), err.len() as u32) };
        let Some(raw) = NonNull::new(raw) else {
            return Err(Error::Load(message(&err)));
        };
        Ok(Module { raw, _bytes: bytes, _runtime: PhantomData })
    }
}

/// The NUL-terminated message WAMR wrote into `buf`. (c_char is u8 on ARM
/// and i8 on x86: the cast is a no-op on one.)
#[allow(clippy::unnecessary_cast)]
fn message(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A C string WAMR returned, copied; empty for NULL.
fn copied(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: WAMR returns NUL-terminated strings that live at least until
    // its next call on the same object; this copies before returning.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// A loaded module.
pub struct Module<'r> {
    raw: NonNull<WASMModuleCommon>,
    _bytes: Vec<u8>,
    _runtime: PhantomData<&'r Runtime>,
}

impl Module<'_> {
    /// Whether it was loaded from AOT-compiled code rather than bytecode.
    pub fn is_aot(&self) -> bool {
        // SAFETY: `raw` is a loaded module until Drop.
        unsafe { wasm_runtime_get_module_package_type(self.raw.as_ptr()) == package_type_t_Wasm_Module_AoT }
    }

    /// What it imports, and whether each was linked.
    pub fn imports(&self) -> Vec<Import> {
        // SAFETY: `raw` is a loaded module until Drop.
        let n = unsafe { wasm_runtime_get_import_count(self.raw.as_ptr()) };
        let mut v = Vec::new();
        for i in 0..n.max(0) {
            let mut r = wasm_import_t::default();
            // SAFETY: `i` is below the count WAMR gave; `r` is a live
            // wasm_import_t for it to fill.
            unsafe { wasm_runtime_get_import_type(self.raw.as_ptr(), i, &mut r) };
            v.push(Import {
                module: copied(r.module_name),
                name: copied(r.name),
                kind: match r.kind {
                    0 => ImportKind::Func,
                    1 => ImportKind::Table,
                    2 => ImportKind::Memory,
                    _ => ImportKind::Global,
                },
                linked: r.linked,
            });
        }
        v
    }

    /// An instance of it, with its execution environment: `stack` bytes of
    /// WAMR's own stack, its linear memory capped at `max_pages` 64 KiB
    /// pages (below what the module declares, if it declares more), and
    /// `state` for the host's functions ([`Caller::state`]). Everything it
    /// allocates is allocated here, so a process can limit itself after.
    pub fn instantiate(&self, stack: u32, max_pages: u32, state: Box<dyn Any>) -> Result<Instance<'_>, Error> {
        let args =
            InstantiationArgs { default_stack_size: stack, host_managed_heap_size: 0, max_memory_pages: max_pages };
        let mut err = [0 as c_char; 256];
        // SAFETY: `raw` is a loaded module, `args` and `err` live buffers.
        let raw = unsafe { wasm_runtime_instantiate_ex(self.raw.as_ptr(), &args, err.as_mut_ptr(), err.len() as u32) };
        let Some(raw) = NonNull::new(raw) else {
            return Err(Error::Instantiate(message(&err)));
        };
        // SAFETY: `raw` is a live instance.
        let Some(env) = NonNull::new(unsafe { wasm_runtime_create_exec_env(raw.as_ptr(), stack) }) else {
            // SAFETY: the instance was made above and nothing else has it.
            unsafe { wasm_runtime_deinstantiate(raw.as_ptr()) };
            return Err(Error::Instantiate("no execution environment".into()));
        };
        let state = Box::into_raw(Box::new(state));
        // SAFETY: `raw` is a live instance; the pointer is ours, freed in
        // Instance's Drop after the instance is gone.
        unsafe { wasm_runtime_set_custom_data(raw.as_ptr(), state.cast()) };
        Ok(Instance { raw, env, state, _module: PhantomData })
    }
}

impl Drop for Module<'_> {
    fn drop(&mut self) {
        // SAFETY: every Instance borrows this module, so none is left; the
        // bytes are dropped after this, as fields drop after `drop`.
        unsafe { wasm_runtime_unload(self.raw.as_ptr()) }
    }
}

/// An instance of a module.
pub struct Instance<'m> {
    raw: NonNull<WASMModuleInstanceCommon>,
    env: NonNull<WASMExecEnv>,
    state: *mut Box<dyn Any>,
    _module: PhantomData<&'m Module<'m>>,
}

impl Instance<'_> {
    /// Calls the exported function `name` with no arguments and no
    /// results (`_start`, say). A trap, or a host function's [`Trap`], ends
    /// it: [`Error::Trap`], or [`Error::Exit`] for [`Trap::Exit`].
    pub fn call(&mut self, name: &str) -> Result<(), Error> {
        let cname = CString::new(name).map_err(|_| Error::NoExport(name.into()))?;
        // SAFETY: `raw` is a live instance; `cname` outlives the call.
        let f = unsafe { wasm_runtime_lookup_function(self.raw.as_ptr(), cname.as_ptr()) };
        if f.is_null() {
            return Err(Error::NoExport(name.into()));
        }
        // SAFETY: `env` and `f` are this instance's; with no arguments and
        // no results argv may be NULL.
        let ok = unsafe { wasm_runtime_call_wasm(self.env.as_ptr(), f, 0, std::ptr::null_mut()) };
        if ok {
            return Ok(());
        }
        // SAFETY: `raw` is a live instance.
        let what = copied(unsafe { wasm_runtime_get_exception(self.raw.as_ptr()) });
        // SAFETY: as above.
        unsafe { wasm_runtime_clear_exception(self.raw.as_ptr()) };
        match what.strip_prefix("Exception: ").unwrap_or(&what).strip_prefix(crate::EXIT) {
            Some(code) => Err(Error::Exit(code.trim().parse().unwrap_or(1))),
            None => Err(Error::Trap(what)),
        }
    }

    /// The size of its memory now, in 64 KiB pages; 0 if it has none.
    pub fn memory_pages(&self) -> u64 {
        // SAFETY: `raw` is a live instance.
        let mem = unsafe { wasm_runtime_get_default_memory(self.raw.as_ptr()) };
        if mem.is_null() {
            return 0;
        }
        // SAFETY: `mem` is the live instance's own memory.
        unsafe { wasm_memory_get_cur_page_count(mem) }
    }

    /// The host state it was made with.
    pub fn state<S: 'static>(&mut self) -> Option<&mut S> {
        // SAFETY: `state` is the Box made in instantiate, freed only in Drop;
        // `&mut self` makes this the only reference to it.
        unsafe { (*self.state).downcast_mut() }
    }
}

impl Drop for Instance<'_> {
    fn drop(&mut self) {
        // SAFETY: the environment and the instance are live until here,
        // and nothing of them is used after; then the state it pointed at
        // is ours alone to free.
        unsafe {
            wasm_runtime_destroy_exec_env(self.env.as_ptr());
            wasm_runtime_deinstantiate(self.raw.as_ptr());
            drop(Box::from_raw(self.state));
        }
    }
}

/// What a host function is called with: its instance's memory and state.
pub struct Caller<'a> {
    mem: Memory<'a>,
    state: &'a mut Box<dyn Any>,
}

impl<'a> Caller<'a> {
    /// `len` bytes of the module's memory at `offset`, or a trap if any of
    /// them is outside it.
    pub fn bytes(&self, offset: u32, len: u32) -> Result<&[u8], Trap> {
        self.mem.bytes(offset, len)
    }

    /// The same, to write.
    pub fn bytes_mut(&mut self, offset: u32, len: u32) -> Result<&mut [u8], Trap> {
        self.mem.bytes_mut(offset, len)
    }

    /// The host state the instance was made with.
    pub fn state<S: 'static>(&mut self) -> Option<&mut S> {
        self.state.downcast_mut()
    }

    /// The module's memory and the host state at once, to copy between
    /// them.
    pub fn split<S: 'static>(&mut self) -> Option<(&mut Memory<'a>, &mut S)> {
        let s = self.state.downcast_mut()?;
        Some((&mut self.mem, s))
    }
}

/// A module's linear memory, as a host function sees it: every access
/// checked against its bounds.
pub struct Memory<'a> {
    inst: InstT,
    _call: PhantomData<&'a mut [u8]>,
}

impl Memory<'_> {
    /// `len` bytes at `offset`, or a trap if any of them is outside the
    /// memory.
    pub fn bytes(&self, offset: u32, len: u32) -> Result<&[u8], Trap> {
        let p = self.checked(offset, len)?;
        // SAFETY: WAMR checked that [offset, offset+len) is inside the
        // instance's linear memory, which can't grow or move while a host
        // function runs (the module is stopped in the call; one thread).
        Ok(unsafe { std::slice::from_raw_parts(p, len as usize) })
    }

    /// The same, to write.
    pub fn bytes_mut(&mut self, offset: u32, len: u32) -> Result<&mut [u8], Trap> {
        let p = self.checked(offset, len)?;
        // SAFETY: as in `bytes`; `&mut self` makes this the only slice of
        // the memory handed out.
        Ok(unsafe { std::slice::from_raw_parts_mut(p, len as usize) })
    }

    fn checked(&self, offset: u32, len: u32) -> Result<*mut u8, Trap> {
        if len == 0 {
            return Ok(NonNull::<u8>::dangling().as_ptr());
        }
        // SAFETY: `inst` is the live instance this call is in.
        if !unsafe { wasm_runtime_validate_app_addr(self.inst, offset as u64, len as u64) } {
            // WAMR has set the module's exception: out of bounds.
            return Err(Trap::Bounds);
        }
        // SAFETY: as above, for an address just checked.
        Ok(unsafe { wasm_runtime_addr_app_to_native(self.inst, offset as u64) }.cast())
    }
}

/// Every host function's entry from WAMR: finds its [`Func`] (the
/// attachment), runs it, and puts its result in `args[0]`, or its trap in
/// the module's exception.
extern "C" fn trampoline(env: ExecEnvT, args: *mut u64) {
    // SAFETY: WAMR passes the attachment registered with this function,
    // always a `&'static Func` (Runtime::register).
    let f: &'static Func = unsafe { &*(wasm_runtime_get_function_attachment(env) as *const Func) };
    // SAFETY: `env` is the live execution environment of this call.
    let inst = unsafe { wasm_runtime_get_module_inst(env) };
    // SAFETY: `inst` is live.
    let data = unsafe { wasm_runtime_get_custom_data(inst) } as *mut Box<dyn Any>;
    if data.is_null() {
        // A module's start function runs inside instantiation, before the
        // instance has its state: the host isn't there for it yet.
        // SAFETY: `inst` is live; the message is a static C string.
        unsafe { wasm_runtime_set_exception(inst, c"the host isn't ready during instantiation".as_ptr()) };
        return;
    }
    // SAFETY: the state is set in instantiate, right after WAMR's own, and
    // freed only after the instance is; it was checked non-null above.
    let state = unsafe { &mut *data };
    // SAFETY: in raw mode, WAMR passes one u64 slot per parameter, in a
    // buffer of at least 16 slots, and reads the result from slot 0.
    let slots = unsafe { std::slice::from_raw_parts_mut(args, f.params.len().max(1)) };
    let mut caller = Caller { mem: Memory { inst, _call: PhantomData }, state };
    let argv: Vec<u64> = slots[..f.params.len()].to_vec();
    match (f.call)(&mut caller, &argv) {
        Ok(v) => slots[0] = v,
        // WAMR's message stands (it says where).
        Err(Trap::Bounds) => {}
        Err(t) => {
            let msg = CString::new(t.to_string()).unwrap_or_else(|_| c"host function failed".into());
            // SAFETY: `inst` is live; WAMR copies the message.
            unsafe { wasm_runtime_set_exception(inst, msg.as_ptr()) };
        }
    }
}
