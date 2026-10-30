//! neochumby-wamr: [WAMR](https://github.com/bytecodealliance/wasm-micro-runtime),
//! the WebAssembly Micro Runtime, built for the Chumby, with a small safe
//! interface: load a module (bytecode, or AOT-compiled for the Chumby),
//! see what it imports, give it the host's functions, run it.
//!
//! WAMR itself is built by `wamr-sys`, from neochumby's fork of
//! wamr-rust-sdk (github.com/johnmerchant/wamr-rust-sdk), pinned: WAMR-2.4.5
//! with the fork's patches (the fast interpreter's loader, alignment-safe
//! on ARMv5), its AOT loader and fast interpreter, with bounds checks, and
//! none of its WASI, SIMD or JIT (`default-features = false`, `aot` and
//! `fast-interp`); for an armv5te target, as ARMV5TE. wamr-rust-sdk's own
//! safe API isn't used: it registers host functions only by bare
//! pointer, with no signature, attachment or raw arguments, and has no way
//! to list a module's imports, keep host state on an instance or check a
//! module's memory, which the runner's host API and import policy are
//! built on. This crate is that layer, over wamr-sys's bindings. Its host
//! API is whatever [`Runtime::register`] is given: neochumby-wasm, the
//! runner, gives each app only what its manifest grants.
//!
//! ```no_run
//! use neochumby_wamr::{Func, Runtime, Val};
//!
//! static HOST: [Func; 1] = [Func {
//!     name: "now_ms",
//!     params: &[],
//!     result: Some(Val::I64),
//!     call: |_, _| Ok(42),
//! }];
//! let rt = Runtime::init().unwrap();
//! rt.register("neochumby", &HOST).unwrap();
//! let module = rt.load(std::fs::read("app.wasm").unwrap()).unwrap();
//! let mut inst = module.instantiate(1 << 20, 256, Box::new(())).unwrap();
//! inst.call("_start").unwrap();
//! ```

mod runtime;

pub use runtime::{Caller, Instance, Memory, Module, Runtime};

/// WAMR's release, as wamr-sys pins it: part of what an `.aot` file is
/// compiled for. A test checks it against the runtime's own.
pub const VERSION: &str = "2.4.5";

pub use runtime::release;

/// How a host function's exit (`Trap::Exit`) is marked in the module's
/// exception, to tell it from a trap.
const EXIT: &str = "neochumby: exit ";

/// A wasm value type, as a host function takes and returns them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Val {
    I32,
    I64,
    F32,
    F64,
}

impl Val {
    fn letter(self) -> char {
        match self {
            Val::I32 => 'i',
            Val::I64 => 'I',
            Val::F32 => 'f',
            Val::F64 => 'F',
        }
    }
}

/// A host function: what a module imports by `name`, with its parameters
/// and result, and what runs when it's called. `call` gets each argument
/// as a u64 (an i32 in its low 32 bits) and returns the result the same
/// way, or a [`Trap`] that ends the module.
pub struct Func {
    pub name: &'static str,
    pub params: &'static [Val],
    pub result: Option<Val>,
    pub call: fn(&mut Caller<'_>, &[u64]) -> Result<u64, Trap>,
}

impl Func {
    /// WAMR's signature string, `(iiI)i`: checked against the import's own
    /// type when a module is linked, so a mismatch is left unlinked.
    pub fn signature(&self) -> String {
        let params: String = self.params.iter().map(|v| v.letter()).collect();
        let result = self.result.map(|v| v.letter().to_string()).unwrap_or_default();
        format!("({params}){result}")
    }
}

/// What a host function can end its module with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trap {
    /// It asked for memory outside the module's own (WAMR has said where).
    Bounds,
    /// The module asked to exit, with this status (WASI's `proc_exit`).
    Exit(u32),
    /// Anything else, said in words.
    Other(String),
}

impl std::fmt::Display for Trap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Trap::Bounds => f.write_str("out of bounds memory access"),
            Trap::Exit(code) => write!(f, "{EXIT}{code}"),
            Trap::Other(s) => f.write_str(s),
        }
    }
}

/// What a module imports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    pub module: String,
    pub name: String,
    pub kind: ImportKind,
    /// Whether something registered provides it, with the right type.
    pub linked: bool,
}

/// What kind of thing an import is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportKind {
    Func,
    Table,
    Memory,
    Global,
}

/// What went wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Init(String),
    /// Not a module this runtime can load: WAMR's words.
    Load(String),
    Instantiate(String),
    /// No such exported function.
    NoExport(String),
    /// The module trapped: WAMR's words (`out of bounds memory access`,
    /// `unreachable`...), or a host function's.
    Trap(String),
    /// The module exited, with this status.
    Exit(u32),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Init(s) => write!(f, "starting WAMR: {s}"),
            Error::Load(s) => write!(f, "loading: {s}"),
            Error::Instantiate(s) => write!(f, "instantiating: {s}"),
            Error::NoExport(s) => write!(f, "no exported function {s}"),
            Error::Trap(s) => write!(f, "trapped: {s}"),
            Error::Exit(c) => write!(f, "exited with {c}"),
        }
    }
}

impl std::error::Error for Error {}
