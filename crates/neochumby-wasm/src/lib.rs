//! neochumby-wasm: the runner for apps built as WebAssembly
//! (docs/proposals/0003-wasm-apps.md).
//!
//! The session starts it, in the app's jail, in place of a native `exec`,
//! with the module's path in NEOCHUMBY_WASM. It:
//!
//! 1. reads the app's manifest, and refuses a catalog app anything a
//!    catalog app is never given ([`grant::catalog_allowed`]);
//! 2. opens what the app's `access` grants, and nothing else;
//! 3. loads the module: its `.aot` if the tree's record says it is this
//!    `.wasm`'s, compiled for this runner ([`aot`]), else the bytecode, for
//!    WAMR's fast interpreter;
//! 4. refuses it if it imports anything not granted ([`grant::check`]);
//! 5. instantiates it with its memory capped, and limits its own address
//!    space;
//! 6. locks itself down (neochumby_sys::lockdown: Landlock and seccomp
//!    where the kernel has them), and refuses a catalog app where it has
//!    neither, unless its owner trusts it;
//! 7. runs `_start`.
//!
//! The library is everything but the devices, so it tests on the host;
//! main.rs adds the Chumby's screen, touchscreen and accelerometer.

pub mod aot;
pub mod grant;
pub mod host;
pub mod manifest;

use neochumby_wamr::{Error, Module, Runtime};

use crate::manifest::Manifest;

/// The variable naming the module to run.
pub const WASM_ENV: &str = "NEOCHUMBY_WASM";
/// Set to `1` by the session for an app its owner marked trusted.
pub const TRUSTED_ENV: &str = "NEOCHUMBY_WASM_TRUSTED";

/// The cap on a module's linear memory, in 64 KiB pages: 16 MiB.
pub const MAX_PAGES: u32 = 256;
/// WAMR's own stack for the module's calls (the interpreter's frames, and
/// the AOT code's calls into the host); the module's own stack is in its
/// memory (1 MiB, as tools/tree links it).
pub const STACK: u32 = 128 << 10;
/// The runner's whole address space: the memory cap, WAMR, the module and
/// its compiled code, and the screen's mapping, with room to spare.
pub const ADDRESS_SPACE: u32 = 40 << 20;

/// Registers the host API with `rt`, before any module is loaded.
pub fn register(rt: &Runtime) -> Result<(), Error> {
    rt.register("wasi_snapshot_preview1", &host::WASI)?;
    rt.register("neochumby", &host::NEOCHUMBY)
}

/// A module ready to run, and what was chosen.
pub struct Loaded<'r> {
    pub module: Module<'r>,
    /// Why the interpreter runs it, if it does.
    pub interpreted: Option<String>,
}

/// Loads the app's module, from `wasm` (the bytecode's bytes) and, if the
/// tree has one, `aot` (the compiled module's), checking both and the
/// module's imports against `manifest`.
pub fn load<'r>(
    rt: &'r Runtime,
    wasm: Vec<u8>,
    aot: Option<Vec<u8>>,
    manifest: &Manifest,
) -> Result<Loaded<'r>, String> {
    if !aot::is_bytecode(&wasm) {
        return Err(
            "its .wasm isn't WebAssembly bytecode: refused (compiled code is only ever the tree's own .aot)".into()
        );
    }
    if manifest.catalog {
        grant::catalog_allowed(manifest)?;
    }
    let recorded = aot.map(|b| (b, manifest.aot_key.as_deref(), manifest.aot_sha256.as_deref()));
    let (module, interpreted) = match aot::choose(&wasm, recorded) {
        // A compiled module this WAMR can't load after all (its AOT format
        // moved within a release, say) costs speed, not the app.
        Ok(compiled) => match rt.load(compiled) {
            Ok(m) if m.is_aot() => (m, None),
            Ok(_) => return Err("its .aot isn't compiled code: refused".into()),
            Err(e) => (
                rt.load(wasm).map_err(|e| e.to_string())?,
                Some(format!("its .aot didn't load ({e}): the interpreter runs it")),
            ),
        },
        Err(why) => (rt.load(wasm).map_err(|e| e.to_string())?, Some(why)),
    };
    if interpreted.is_some() && module.is_aot() {
        return Err("its bytecode loaded as compiled code: refused".into());
    }
    grant::check(&module.imports(), manifest)?;
    Ok(Loaded { module, interpreted })
}

/// How a module's run ended, as the runner's exit status: its own exit
/// code, or 1 for anything else (said on stderr).
pub fn exit_code(r: Result<(), Error>, name: &str) -> u8 {
    match r {
        Ok(()) => 0,
        Err(Error::Exit(c)) => c.min(255) as u8,
        Err(e) => {
            eprintln!("{name}: {e}");
            1
        }
    }
}
