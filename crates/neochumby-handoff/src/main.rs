//! neochumby-handoff: writes what goes in /proc/sideload-initrd.
//!
//! ```text
//! neochumby-handoff INITRAMFS DIR FILE... > /proc/sideload-initrd
//! ```
//!
//! Linux 7 has no NAND driver and never reads flash, so what it needs of the
//! Chumby's settings comes over from the stock system, which can read them:
//! this writes INITRAMFS, zeros to a 4-byte boundary, and the FILEs named,
//! relative to DIR, as one more archive (cpio's newc format) under `psp/`.
//! The kernel unpacks concatenated archives, compressed or not, over one
//! another, so they are at /psp in the root file system, in RAM, with their
//! modes, when /init starts. Only what is named goes: not the whole
//! partition. A FILE that isn't there is left out, and a link is followed
//! (a Chumby's /psp/localtime may be one). The secrets stay out of the
//! kernel's command line, which every process can read. Nothing here writes
//! to DIR, and nothing on Linux 7 writes back.
//!
//! The stock system has no cpio or printf, so this is a static program. It
//! runs on the stock 2.6.16: std's files and directories, and nothing newer. /proc's
//! file takes what one write gives it, so it writes in small pieces.

mod newc;

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;

/// Files larger than this aren't settings, and the initrd's room is small.
const MAX_FILE: u64 = 64 * 1024;
const PIECE: usize = 4096;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [initramfs, dir, files @ ..] = args.as_slice() else {
        eprintln!("usage: neochumby-handoff INITRAMFS DIR FILE... > /proc/sideload-initrd");
        return ExitCode::from(2);
    };
    match run(initramfs, dir, files, &mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("neochumby-handoff: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(initramfs: &str, dir: &str, files: &[String], out: &mut impl Write) -> io::Result<()> {
    let mut archive = newc::Writer::new();
    archive.dir("psp", 0o755);
    for file in files {
        pack(Path::new(dir), file, &mut archive);
    }
    let archive = archive.finish();

    let mut image = File::open(initramfs).map_err(|e| io::Error::new(e.kind(), format!("{initramfs}: {e}")))?;
    let mut buf = [0u8; PIECE];
    let mut written = 0usize;
    loop {
        let n = image.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        out.flush()?;
        written += n;
    }
    out.write_all(&[0u8; 4][..newc::padding(written)])?;
    for piece in archive.chunks(PIECE) {
        out.write_all(piece)?;
        out.flush()?;
    }
    Ok(())
}

/// `file`, relative to `dir`, as `psp/file`, with the directories on the way.
/// What can't be read is left out with a word on stderr: one bad file doesn't
/// stop a boot, and what isn't there is only an option the owner didn't take.
fn pack(dir: &Path, file: &str, archive: &mut newc::Writer) {
    let path = dir.join(file);
    let meta = match fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return,
        Err(e) => return eprintln!("neochumby-handoff: {}: {e}", path.display()),
    };
    if !meta.is_file() || meta.len() > MAX_FILE {
        return eprintln!("neochumby-handoff: {}: not a settings file", path.display());
    }
    let data = match fs::read(&path) {
        Ok(d) => d,
        Err(e) => return eprintln!("neochumby-handoff: {}: {e}", path.display()),
    };
    let mut parent = String::from("psp");
    let mut parts: Vec<&str> = file.split('/').collect();
    let name = parts.pop().unwrap_or(file);
    for p in parts {
        parent = format!("{parent}/{p}");
        let mode = fs::metadata(dir.join(parent.trim_start_matches("psp/"))).map_or(0o700, |m| m.mode() & 0o7777);
        archive.dir(&parent, mode);
    }
    archive.file(&format!("{parent}/{name}"), meta.mode() & 0o7777, &data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_is_named_goes_over() {
        let d = std::env::temp_dir().join(format!("neochumby-handoff-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("psp/.ssh")).unwrap();
        for (f, body) in [
            ("psp/network_config", "wifi"),
            ("psp/.ssh/authorized_keys", "key"),
            ("psp/neochumby.key", "NOT-THIS"),
            ("psp/zone", "tz"),
            ("image", "IMAGE"),
        ] {
            fs::write(d.join(f), body).unwrap();
        }
        std::os::unix::fs::symlink(d.join("psp/zone"), d.join("psp/localtime")).unwrap();
        let (psp, image) = (d.join("psp"), d.join("image"));
        let names: Vec<String> =
            ["network_config", "localtime", "absent", ".ssh/authorized_keys"].iter().map(|s| s.to_string()).collect();
        let mut out = Vec::new();
        run(image.to_str().unwrap(), psp.to_str().unwrap(), &names, &mut out).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.starts_with("IMAGE"));
        for want in ["psp/network_config", "wifi", "psp/localtime", "tz", "psp/.ssh/authorized_keys", "key"] {
            assert!(text.contains(want), "{want}");
        }
        assert!(!text.contains("NOT-THIS") && !text.contains("absent"), "{text}");
        fs::remove_dir_all(&d).unwrap();
    }
}
