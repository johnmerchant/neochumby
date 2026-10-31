//! Compiling a catalog app's `.wasm` for the Chumby, with wamrc (WAMR's AOT
//! compiler, which is LLVM), built by tools/wamrc: `--wamrc PATH`, or
//! NEOCHUMBY_WAMRC. `wamrc.id` beside it says which WAMR it compiles for,
//! and goes in the key the runner checks (neochumby_wasm::aot).
//!
//! wamrc reads a module someone else wrote, so it runs as a sandboxed
//! child: in a scratch directory of its own with nothing in it but the
//! module, with an empty environment but PATH and LD_LIBRARY_PATH, an
//! address space of [`MEMORY`], [`CPU_SECS`] of CPU and [`WALL`] of wall
//! time (then its process group is killed), no core dumps, and on Linux, where
//! unprivileged user namespaces are allowed, a network namespace of its
//! own, so it can reach nothing. It runs as neochumbyd's own user.
//!
//! With no wamrc, an app is installed without an `.aot`, and the runner's
//! interpreter runs it, slowly.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use neochumby_sys::process::{self, ChildLimits, Signal};

/// How every `.aot` is compiled, as one word for its key; the same as
/// neochumby_wasm::aot::FLAGS, which the runner checks it against.
pub const FLAGS: &str = "armv5te-eabi-arm926ej-s-bounds-nonatives";
/// The arguments [`FLAGS`] names: neochumby_wasm::aot::WAMRC_ARGS.
pub const ARGS: &[&str] = &["--target=armv5te", "--target-abi=eabi", "--cpu=arm926ej-s", "--bounds-checks=1"];

/// wamrc's wall time.
pub const WALL: Duration = Duration::from_secs(120);
/// wamrc's CPU time.
pub const CPU_SECS: u64 = 120;
/// wamrc's address space.
pub const MEMORY: u64 = 2 << 30;
/// The largest `.aot` taken (and file wamrc may write).
pub const AOT_LIMIT: u64 = 64 << 20;

/// A wamrc, and what it compiles for.
#[derive(Clone, Debug)]
pub struct Wamrc {
    pub path: PathBuf,
    /// From `wamrc.id` beside it: WAMR's release and commit
    /// (`2.4.5-25bd7eb63e82`), as neochumby_wamr::VERSION is.
    pub id: String,
    /// Its wall time: [`WALL`].
    pub wall: Duration,
}

impl Wamrc {
    /// The wamrc `option` names, else NEOCHUMBY_WAMRC's; None if neither
    /// does.
    pub fn find(option: Option<PathBuf>) -> Result<Option<Wamrc>, String> {
        let path = option.or_else(|| std::env::var_os("NEOCHUMBY_WAMRC").filter(|v| !v.is_empty()).map(PathBuf::from));
        path.map(Wamrc::at).transpose()
    }

    /// The wamrc at `path`, with its `wamrc.id`.
    pub fn at(path: PathBuf) -> Result<Wamrc, String> {
        if !path.is_file() {
            return Err(format!("{}: no wamrc there (tools/wamrc builds one)", path.display()));
        }
        let id_file = path.parent().unwrap_or(Path::new(".")).join("wamrc.id");
        let id = std::fs::read_to_string(&id_file).map_err(|e| format!("{}: {e}", id_file.display()))?;
        let id = id.trim().to_string();
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_+".contains(&b)) {
            return Err(format!("{}: not a WAMR version", id_file.display()));
        }
        Ok(Wamrc { path, id, wall: WALL })
    }

    /// The key an `.aot` compiled from `wasm` by this wamrc is recorded
    /// under: `sha256:<the .wasm's>/wamr:<id>/<flags>`.
    pub fn key(&self, wasm: &[u8]) -> String {
        format!("sha256:{}/wamr:{}/{FLAGS}", super::sha256(wasm), self.id)
    }

    /// Compiles `wasm`, in a new directory under `scratch` (removed after):
    /// the `.aot`'s bytes, and whether wamrc had no network (None if that
    /// couldn't be told).
    pub fn compile(&self, wasm: &[u8], scratch: &Path) -> Result<(Vec<u8>, Option<bool>), String> {
        std::fs::create_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
        let dir = scratch.join(format!("wamrc-{}-{}", std::process::id(), super::unique()));
        std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let r = self.compile_in(wasm, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    fn compile_in(&self, wasm: &[u8], dir: &Path) -> Result<(Vec<u8>, Option<bool>), String> {
        let io = |e: std::io::Error| format!("wamrc: {e}");
        std::fs::write(dir.join("app.wasm"), wasm).map_err(io)?;
        let log = std::fs::File::create(dir.join("wamrc.log")).map_err(io)?;
        let mut cmd = Command::new(&self.path);
        cmd.args(ARGS).args(["-o", "app.aot", "app.wasm"]).current_dir(dir).env_clear();
        for var in ["PATH", "LD_LIBRARY_PATH"] {
            if let Some(v) = std::env::var_os(var) {
                cmd.env(var, v);
            }
        }
        cmd.env("HOME", dir).env("TMPDIR", dir);
        cmd.stdin(Stdio::null()).stdout(log.try_clone().map_err(io)?).stderr(log).process_group(0);
        process::limit_child(
            &mut cmd,
            ChildLimits { cpu_secs: CPU_SECS, memory: MEMORY, file_size: AOT_LIMIT, no_network: true },
        );
        let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", self.path.display()))?;
        let isolated = process::own_network(child.id());
        let deadline = Instant::now() + self.wall;
        let status = loop {
            match child.try_wait().map_err(io)? {
                Some(s) => break s,
                None if Instant::now() >= deadline => {
                    // Not yet reaped, so its group's id is still its own.
                    let _ = process::kill_group(child.id(), Signal::Kill);
                    let _ = child.wait();
                    return Err(format!("wamrc took more than {} s: stopped", self.wall.as_secs_f32()));
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        };
        if !status.success() {
            let log = std::fs::read(dir.join("wamrc.log")).unwrap_or_default();
            let tail = String::from_utf8_lossy(&log[log.len().saturating_sub(400)..]).trim().to_string();
            return Err(format!("wamrc failed ({status}): {tail}"));
        }
        let aot = std::fs::read(dir.join("app.aot")).map_err(|e| format!("wamrc wrote no .aot: {e}"))?;
        if aot.is_empty() || aot.len() as u64 > AOT_LIMIT || super::is_bytecode(&aot) {
            return Err("wamrc's .aot isn't a compiled module".into());
        }
        Ok((aot, isolated))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_its_id() {
        let dir = super::super::tests::temp("wamrc-id");
        let path = dir.join("wamrc");
        assert!(Wamrc::at(path.clone()).unwrap_err().contains("no wamrc"));
        std::fs::write(&path, "").unwrap();
        assert!(Wamrc::at(path.clone()).is_err());
        std::fs::write(dir.join("wamrc.id"), "2.4.3-f8ed1f442969\n").unwrap();
        let w = Wamrc::at(path.clone()).unwrap();
        assert_eq!(w.id, "2.4.3-f8ed1f442969");
        std::fs::write(dir.join("wamrc.id"), "a/b=c\n").unwrap();
        assert!(Wamrc::at(path).is_err());
    }

    #[test]
    fn a_wamrc_that_hangs_is_stopped() {
        let dir = super::super::tests::temp("wamrc-hang");
        let mut w = super::super::tests::fake_wamrc(&dir, "sleep 30");
        w.wall = Duration::from_millis(300);
        let start = Instant::now();
        let e = w.compile(b"\0asm\x01\0\0\0", &dir.join("scratch")).unwrap_err();
        assert!(e.contains("took more than"), "{e}");
        assert!(start.elapsed() < Duration::from_secs(10));
        // Its scratch directory goes with it.
        assert_eq!(std::fs::read_dir(dir.join("scratch")).unwrap().count(), 0);
    }

    #[test]
    fn a_failing_wamrc_says_why() {
        let dir = super::super::tests::temp("wamrc-fail");
        let w = super::super::tests::fake_wamrc(&dir, "echo no such target >&2; exit 3");
        let e = w.compile(b"\0asm\x01\0\0\0", &dir.join("scratch")).unwrap_err();
        assert!(e.contains("no such target"), "{e}");
        // Nor is bytecode taken back as compiled code.
        let w = super::super::tests::fake_wamrc(&dir, "cp app.wasm app.aot");
        assert!(w.compile(b"\0asm\x01\0\0\0", &dir.join("scratch")).is_err());
    }

    /// With a real wamrc (NEOCHUMBY_WAMRC, from tools/wamrc under `nix
    /// develop .#wasm`): `cargo test -p neochumbyd -- --ignored`.
    #[test]
    #[ignore = "needs a wamrc built by tools/wamrc, as NEOCHUMBY_WAMRC"]
    fn a_real_wamrc_compiles_in_its_sandbox() {
        let w = Wamrc::find(None).unwrap().expect("NEOCHUMBY_WAMRC");
        let dir = super::super::tests::temp("wamrc-real");
        // (module (func (export "_start")))
        let wasm = b"\0asm\x01\0\0\0\x01\x04\x01\x60\0\0\x03\x02\x01\0\x07\x0a\x01\x06_start\0\0\x0a\x04\x01\x02\0\x0b";
        let (aot, isolated) = w.compile(wasm, &dir).unwrap();
        assert!(aot.starts_with(b"\0aot"));
        eprintln!("compiled {} bytes; no network: {isolated:?}", aot.len());
    }
}
