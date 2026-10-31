//! Which module the runner loads: the AOT-compiled one when the tree has it
//! and it is the one compiled from this `.wasm` for this runner, else the
//! bytecode, run by the fast interpreter.
//!
//! An `.aot` is native code: it is only ever compiled by neochumbyd or by
//! tools/tree on the PC, from the pinned `.wasm`, with bounds checks on, and
//! never taken from an app's author. The tree records what each was
//! compiled from and for beside it, in the manifest:
//!
//!   aot=neochumby-clock.aot
//!   aot-key=sha256:<the .wasm's>/wamr:<version>/<flags>      (key)
//!   aot-sha256=<the .aot's own>
//!
//! The runner checks all three, and that the `.wasm` really is bytecode (an
//! AOT module named `.wasm` would otherwise be loaded as one: WAMR goes by
//! the magic number).

use sha2::{Digest, Sha256};

/// How every `.aot` is compiled, as one word for the key: wamrc
/// `--target=armv5te --target-abi=eabi --cpu=arm926ej-s`, bounds checks on,
/// by a wamrc built without WAMR's own native libraries (tools/wamrc).
///
/// That last matters. A wamrc that knows a native's signature (WAMR's own
/// WASI, say) compiles a call to it as a direct call in WAMR's native
/// calling convention, with pointers already converted; the runner's host
/// functions are raw natives, called with a slot array, and would take
/// the module's arguments as an address. Built without them, wamrc calls
/// every import through WAMR's own invoke, as the interpreter does.
pub const FLAGS: &str = "armv5te-eabi-arm926ej-s-bounds-nonatives";

/// The arguments to wamrc that [`FLAGS`] names (tools/tree and neochumbyd
/// pass these, and nothing else).
pub const WAMRC_ARGS: &[&str] = &["--target=armv5te", "--target-abi=eabi", "--cpu=arm926ej-s", "--bounds-checks=1"];

/// WebAssembly bytecode's magic number and version 1.
const WASM_MAGIC: &[u8] = b"\0asm\x01\0\0\0";

/// The hex sha256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// What an `.aot` compiled from `wasm` for this runner is recorded as.
pub fn key(wasm: &[u8]) -> String {
    format!("sha256:{}/wamr:{}/{FLAGS}", sha256(wasm), neochumby_wamr::VERSION)
}

/// Whether `bytes` are WebAssembly bytecode, not an AOT module or anything
/// else.
pub fn is_bytecode(bytes: &[u8]) -> bool {
    bytes.starts_with(WASM_MAGIC)
}

/// The module to load: the `.aot`'s bytes if `aot` (its bytes, with what the
/// manifest recorded for it) is this `.wasm`'s, compiled for this runner;
/// else None and why not, for the log.
pub fn choose(wasm: &[u8], aot: Option<(Vec<u8>, Option<&str>, Option<&str>)>) -> Result<Vec<u8>, String> {
    let Some((bytes, recorded_key, recorded_sha)) = aot else {
        return Err("no .aot: the interpreter runs it, slowly".into());
    };
    let want = key(wasm);
    if recorded_key != Some(want.as_str()) {
        return Err(format!(
            "its .aot was compiled from another .wasm or for another runner (recorded {}, this is {want}): the interpreter runs it",
            recorded_key.unwrap_or("nothing")
        ));
    }
    if recorded_sha != Some(sha256(&bytes).as_str()) {
        return Err("its .aot isn't the one its compiler recorded: refused; the interpreter runs it".into());
    }
    if is_bytecode(&bytes) {
        return Err("its .aot is bytecode, not compiled: the interpreter runs it".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WASM: &[u8] = b"\0asm\x01\0\0\0";
    const AOT: &[u8] = b"\0aot\x03\0\0\0native";

    #[test]
    fn takes_only_the_aot_compiled_from_this_wasm_for_this_runner() {
        let k = key(WASM);
        let s = sha256(AOT);
        assert!(k.starts_with("sha256:") && k.contains(neochumby_wamr::VERSION) && k.ends_with(FLAGS));
        assert_eq!(choose(WASM, Some((AOT.to_vec(), Some(&k), Some(&s)))).unwrap(), AOT);
        assert!(choose(WASM, None).is_err());
    }

    #[test]
    fn a_forged_or_stale_aot_is_refused() {
        let k = key(WASM);
        let s = sha256(AOT);
        // Another .wasm's, or another runner's.
        let other = key(b"\0asm\x01\0\0\0\x00");
        assert!(choose(WASM, Some((AOT.to_vec(), Some(&other), Some(&s)))).is_err());
        let older = k.replace(neochumby_wamr::VERSION, "1.0.0");
        assert!(choose(WASM, Some((AOT.to_vec(), Some(&older), Some(&s)))).is_err());
        assert!(choose(WASM, Some((AOT.to_vec(), None, Some(&s)))).is_err());
        // Swapped for other native code, with the key left as it was.
        let forged = b"\0aot\x03\0\0\0evil".to_vec();
        let e = choose(WASM, Some((forged, Some(&k), Some(&s)))).unwrap_err();
        assert!(e.contains("refused"), "{e}");
        assert!(choose(WASM, Some((AOT.to_vec(), Some(&k), None))).is_err());
    }

    #[test]
    fn bytecode_is_told_from_compiled_code() {
        assert!(is_bytecode(WASM));
        assert!(!is_bytecode(AOT));
        assert!(!is_bytecode(b"\x7fELF"));
        assert_eq!(sha256(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
}
