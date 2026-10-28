//! Where the Python half is, and how both launchers run it.
//!
//!   NEOCHUMBY_WX_PYTHON_DIR  the scripts; default python/ beside this
//!                            binary, where tools/tree puts files/python
//!   NEOCHUMBY_WX_PYTHON      the interpreter, with Pillow (a venv's python,
//!                            say); default python3 on PATH
//!   NEOCHUMBY_STATE          the app's state directory (neochumbyd's):
//!                            config.json, photos, caches and out/; default
//!                            /var/lib/neochumbyd/apps/wx-feed
//!   NEOCHUMBY_WX_OUT         where a Chumby's renders go: a path in which
//!                            {chumby} stands for its name; default
//!                            neochumbyd's overlay for that Chumby (below)

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_STATE: &str = "/var/lib/neochumbyd/apps/wx-feed";

/// Absolute, because the Python runs inside it.
pub fn state_dir() -> PathBuf {
    let dir: PathBuf =
        std::env::var_os("NEOCHUMBY_STATE").filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_STATE.into()).into();
    std::path::absolute(&dir).unwrap_or(dir)
}

/// Where the renders go, with {chumby} for each Chumby's name: `out`
/// (NEOCHUMBY_WX_OUT) if set; a path without {chumby} gets /{chumby} added,
/// so Chumbys never share a directory. By default, neochumbyd's overlay:
/// <neochumbyd state>/overlay/{chumby}/apps/wx-feed/out, two up from the
/// app's state (<neochumbyd state>/apps/wx-feed). sync layers a Chumby's
/// overlay over the tree and sends it that Chumby alone, so its engine
/// finds the pictures at apps/wx-feed/out/ in its tree. (NFS doesn't serve
/// the overlay yet.)
pub fn out_template(out: Option<&str>, state: &Path) -> String {
    match out.filter(|s| !s.is_empty()) {
        Some(t) if t.contains(CHUMBY) => t.to_string(),
        Some(t) => format!("{}/{CHUMBY}", t.trim_end_matches('/')),
        None => {
            let neochumbyd = state.parent().and_then(Path::parent).unwrap_or(state);
            format!("{}/overlay/{CHUMBY}/apps/wx-feed/out", neochumbyd.display())
        }
    }
}

pub const CHUMBY: &str = "{chumby}";

pub fn script_dir() -> io::Result<PathBuf> {
    if let Some(d) = std::env::var_os("NEOCHUMBY_WX_PYTHON_DIR").filter(|s| !s.is_empty()) {
        return Ok(d.into());
    }
    let exe = std::env::current_exe()?;
    Ok(exe.parent().map(|d| d.join("python")).unwrap_or_else(|| "python".into()))
}

fn interpreter() -> OsString {
    std::env::var_os("NEOCHUMBY_WX_PYTHON").filter(|s| !s.is_empty()).unwrap_or_else(|| "python3".into())
}

/// `python3 <script dir>/<script>`, run in the state directory (made if
/// need be), with NEOCHUMBY_STATE set for the Python's own defaults. No
/// bytecode is written: the tree the scripts sit in is read-only.
pub fn command(script: &str) -> io::Result<Command> {
    let dir = script_dir()?;
    let path = dir.join(script);
    if !path.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{}: no such script", path.display())));
    }
    let state = state_dir();
    std::fs::create_dir_all(&state)?;
    let mut c = Command::new(interpreter());
    c.arg(path).current_dir(&state).env("NEOCHUMBY_STATE", &state).env("PYTHONDONTWRITEBYTECODE", "1");
    Ok(c)
}
