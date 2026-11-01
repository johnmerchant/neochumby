//! neochumby-handoff: writes what goes in /proc/sideload-initrd.
//!
//! ```text
//! neochumby-handoff INITRAMFS DIR... > /proc/sideload-initrd
//! ```
//!
//! Linux 7 has no NAND driver, so it can't read the Chumby's settings
//! partition: the Wi-Fi network in /psp/network_config, the pairing in
//! /psp/neochumby.*. So the stock system, which can, hands them over: this
//! writes INITRAMFS, zeros to a 4-byte boundary, and each DIR's files as one
//! more archive (cpio's newc format) under `psp/`. The kernel unpacks
//! concatenated archives, compressed or not, over one another, so they are
//! at /psp in the root file system, with their modes, when /init starts; a
//! later DIR's file replaces an earlier one's of the same name. The secrets
//! stay out of the kernel's command line, which every process can read.
//!
//! The stock system has no cpio or printf, so this is a static program. It
//! runs on 2.6.16: std's files and directories, and nothing newer. /proc's
//! file takes what one write gives it, so it writes in small pieces.

mod newc;

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;

/// Files larger than this aren't settings, and the initrd's room is small.
const MAX_FILE: u64 = 256 * 1024;
const PIECE: usize = 4096;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [initramfs, dirs @ ..] = args.as_slice() else {
        eprintln!("usage: neochumby-handoff INITRAMFS DIR... > /proc/sideload-initrd");
        return ExitCode::from(2);
    };
    match run(initramfs, dirs, &mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("neochumby-handoff: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(initramfs: &str, dirs: &[String], out: &mut impl Write) -> io::Result<()> {
    let mut archive = newc::Writer::new();
    archive.dir("psp", 0o755);
    for dir in dirs {
        pack(Path::new(dir), "psp", &mut archive)?;
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

/// `dir`'s directories and regular files, as `name/...`. What can't be read
/// is left out with a word on stderr: one bad file doesn't stop a boot.
fn pack(dir: &Path, name: &str, archive: &mut newc::Writer) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let Some(file) = entry.file_name().to_str().map(str::to_owned) else { continue };
        let path = entry.path();
        let inner = format!("{name}/{file}");
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        let mode = meta.mode() & 0o7777;
        if meta.is_dir() {
            archive.dir(&inner, mode);
            pack(&path, &inner, archive)?;
        } else if meta.is_file() && meta.len() <= MAX_FILE {
            match fs::read(&path) {
                Ok(data) => archive.file(&inner, mode, &data),
                Err(e) => eprintln!("neochumby-handoff: {}: {e}", path.display()),
            }
        }
    }
    Ok(())
}
