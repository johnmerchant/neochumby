//! The test images: a copy of the developer's NAND image with its
//! settings (PSP) and Cache partitions rewritten for each boot.
//!
//! - **PSP:** the developer's factory-style settings directory, plus
//!   `disable_intro` (the first-boot tour waits for a tap, and isn't what
//!   is tested), and with a tree, `rfs1/userhook2`: tests/userhook2.
//! - **Cache (mtd6, 24 MiB):** with a tree, a JFFS2 image laid out as
//!   `tools/stick` lays out a stick: this checkout's `boot/userhook2`, and
//!   the tree as `neochumby/`. Otherwise erased, as `mknand` leaves it.
//!
//! The images are built with chumby-emu's own tools (`mkpsp`, `mkjffs2`,
//! `patchnand`), whichever backend boots them.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::backend::{Media, Result};

/// The test hook, installed as /psp/rfs1/userhook2.
const TEST_HOOK: &str = include_str!("../userhook2");
/// neochumby's boot hook, as tools/stick puts it on a stick.
const BOOT_HOOK: &str = include_str!("../../../boot/userhook2");

/// The developer's images, named by environment variables.
pub struct Inputs {
    pub kernel: PathBuf,
    pub nand: PathBuf,
    pub psp: PathBuf,
    /// A tree from `tools/tree`, for the session's scenarios.
    pub tree: Option<PathBuf>,
}

fn var(name: &str) -> std::result::Result<PathBuf, String> {
    let v = std::env::var_os(name).ok_or(format!("{name} is not set"))?;
    let p = PathBuf::from(v);
    if !p.exists() {
        return Err(format!("{name}: {} doesn't exist", p.display()));
    }
    Ok(p)
}

impl Inputs {
    /// The images, or why they aren't there (which skips the tests).
    pub fn from_env() -> std::result::Result<Inputs, String> {
        Ok(Inputs {
            kernel: var("NEOCHUMBY_EMU_KERNEL")?,
            nand: var("NEOCHUMBY_EMU_NAND")?,
            psp: var("NEOCHUMBY_EMU_PSP")?,
            tree: var("NEOCHUMBY_EMU_TREE").ok(),
        })
    }
}

/// Copies a directory tree, keeping permissions (cp -R, without links).
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for e in fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let ty = e.file_type().map_err(|e| e.to_string())?;
        if ty.is_dir() {
            copy_dir(&src, &dst)?;
        } else if ty.is_symlink() {
            let target = fs::read_link(&src).map_err(|e| e.to_string())?;
            std::os::unix::fs::symlink(target, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        } else {
            fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
        }
    }
    Ok(())
}

fn write_exec(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())
}

fn emu(exe: &Path, args: &[&Path]) -> Result<()> {
    let out = Command::new(exe).args(args).output().map_err(|e| format!("{}: {e}", exe.display()))?;
    if !out.status.success() {
        return Err(format!("chumby-emu {:?}: {}", args, String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Builds the media for one boot in `work` (emptied first): with the tree
/// on the stick if `with_tree`.
pub fn build(inputs: &Inputs, with_tree: bool, emu_exe: &Path, work: &Path) -> Result<Media> {
    let _ = fs::remove_dir_all(work);
    fs::create_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))?;

    let psp = work.join("psp");
    copy_dir(&inputs.psp, &psp)?;
    fs::write(psp.join("disable_intro"), "").map_err(|e| e.to_string())?;
    let cache_img = work.join("cache.jffs2");
    let cache = if with_tree {
        let tree = inputs.tree.as_ref().ok_or("NEOCHUMBY_EMU_TREE is not set")?;
        fs::create_dir_all(psp.join("rfs1")).map_err(|e| e.to_string())?;
        write_exec(&psp.join("rfs1/userhook2"), TEST_HOOK)?;
        let stick = work.join("stick");
        copy_dir(tree, &stick.join("neochumby"))?;
        write_exec(&stick.join("userhook2"), BOOT_HOOK)?;
        emu(emu_exe, &[Path::new("mkjffs2"), Path::new("cache"), &stick, &cache_img])?;
        format!("cache={}", cache_img.display())
    } else {
        "cache=erased".to_string()
    };
    let psp_img = work.join("psp.jffs2");
    emu(emu_exe, &[Path::new("mkpsp"), &psp, &psp_img])?;

    let nand = work.join("nand.img");
    let psp_arg = format!("psp={}", psp_img.display());
    emu(emu_exe, &[Path::new("patchnand"), &inputs.nand, &nand, Path::new(&psp_arg), Path::new(&cache)])?;
    Ok(Media { kernel: inputs.kernel.clone(), nand, stick: None })
}
