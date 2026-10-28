//! chumby-emu: a Chumby Classic emulator for running the stock firmware.

// Register offsets are written as in the i.MX21 manual (`0x00 / 4`), and
// register lists are walked by number, as the ARM manual describes them.
#![allow(clippy::erasing_op, clippy::eq_op, clippy::identity_op, clippy::needless_range_loop)]

mod arm;
mod control;
mod cpu;
mod image;
mod machine;
mod mmu;
mod png;
mod soc;
mod thumb;

use std::process::ExitCode;
use std::time::Instant;

use machine::Machine;

/// The stock bootloader's command line for Root Disk1 on hardware 3.8.
const STOCK_CMDLINE: &str =
    "console=ttyS0,38400 root=/dev/mtdblock5 rootfstype=cramfs init=/linuxrc chumby_hwversion=3.8";
/// MACH_TYPE_MX21ADS, which the Chumby kernel is built as.
const MACH_MX21ADS: u32 = 851;

const USAGE: &str = "\
usage: chumby-emu boot --kernel ZIMAGE [options]
       chumby-emu mkpsp DIR OUT        JFFS2 image of a settings directory
       chumby-emu mknand BACKUP PSP OUT
                     a full NAND image: the partitions from a backup's
                     nanddumps, PSP (a JFFS2 image) as the settings
                     partition, and the Cache partition erased
       chumby-emu mkjffs2 PART DIR OUT
                     JFFS2 image of a directory, the size of partition
                     PART (psp, cache, or a number)
       chumby-emu patchnand NAND OUT PART=FILE|PART=erased ...
                     a copy of a whole NAND image with those partitions
                     replaced by plain images (mkpsp, mkjffs2) or erased

  --kernel FILE      a zImage (a Kernel partition image works as is)
  --nand FILE        the NAND image; programs and erases are saved to it
  --nand-ro          keep NAND writes in memory only
  --cmdline TEXT     kernel command line (default: the stock one)
  --cycles N         stop after N emulated cycles
  --trace-at ADDR    trace every instruction once the PC reaches ADDR (hex)
  --quiet-mmio       don't log accesses to unmodelled registers
  --history          keep recent PCs and print them on stop
  --screenshot FILE  write the screen to FILE (PNG) every emulated second
  --realtime         hold emulated time to wall-clock time
  --control ADDR     take commands (touch, screenshots) on a TCP address,
                     e.g. 127.0.0.1:5810; see src/control.rs
";

struct Options {
    kernel: Option<String>,
    cmdline: String,
    cycles: u64,
    trace_at: Option<u32>,
    quiet_mmio: bool,
    history: bool,
    nand: Option<String>,
    nand_ro: bool,
    screenshot: Option<String>,
    control: Option<String>,
    realtime: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        kernel: None,
        cmdline: STOCK_CMDLINE.into(),
        cycles: u64::MAX,
        trace_at: None,
        quiet_mmio: false,
        history: false,
        nand: None,
        nand_ro: false,
        screenshot: None,
        control: None,
        realtime: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--kernel" => o.kernel = Some(val()?),
            "--cmdline" => o.cmdline = val()?,
            "--cycles" => o.cycles = val()?.parse().map_err(|e| format!("--cycles: {e}"))?,
            "--trace-at" => {
                let v = val()?;
                o.trace_at =
                    Some(u32::from_str_radix(v.trim_start_matches("0x"), 16).map_err(|e| format!("--trace-at: {e}"))?)
            }
            "--quiet-mmio" => o.quiet_mmio = true,
            "--history" => o.history = true,
            "--nand" => o.nand = Some(val()?),
            "--nand-ro" => o.nand_ro = true,
            "--screenshot" => o.screenshot = Some(val()?),
            "--control" => o.control = Some(val()?),
            "--realtime" => o.realtime = true,
            _ => return Err(format!("unknown option {a}")),
        }
    }
    Ok(o)
}

fn boot(o: Options) -> Result<(), String> {
    let path = o.kernel.ok_or("--kernel is required")?;
    let kernel = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let nfc = match &o.nand {
        Some(p) => {
            let data = std::fs::read(p).map_err(|e| format!("{p}: {e}"))?;
            let file = if o.nand_ro {
                None
            } else {
                Some(std::fs::OpenOptions::new().write(true).open(p).map_err(|e| format!("{p}: {e}"))?)
            };
            soc::nfc::Nfc::new(data, file)
        }
        None => soc::nfc::Nfc::new(Vec::new(), None),
    };
    let mut soc = soc::Soc::new(nfc);
    soc.log_mmio = !o.quiet_mmio;
    soc.attach_stdin();
    if let Some(addr) = &o.control {
        soc.control = Some(control::listen(addr).map_err(|e| format!("{addr}: {e}"))?);
    }
    let mut m = Machine::new(soc);
    // Loaded clear of where the kernel decompresses itself (0xc0008000).
    m.boot_linux(&kernel, 0xc080_0000, MACH_MX21ADS, &o.cmdline);
    let start = Instant::now();
    if o.realtime {
        m.realtime = Some((start, 0));
    }
    m.history_on = o.history;
    if let Some(t) = o.trace_at {
        m.trace_at = t;
    }
    while !m.stop && m.clock < o.cycles {
        let next = (m.clock / machine::CPU_HZ + 1) * machine::CPU_HZ;
        m.run_until(next.min(o.cycles));
        if let Some(path) = &o.screenshot {
            screenshot(&m, path);
        }
    }
    let secs = start.elapsed().as_secs_f64();
    eprintln!(
        "\n[emu] stopped at pc={:08x} after {} instructions, {:.2}s emulated, {:.2}s real ({:.0} MIPS)",
        m.cpu.r[15],
        m.icount,
        m.clock as f64 / machine::CPU_HZ as f64,
        secs,
        m.icount as f64 / secs / 1e6
    );
    eprintln!("[emu] {}", m.dump());
    Ok(())
}

/// Writes the composed screen as a PNG, via a temporary file so readers
/// never see half a file.
fn screenshot(m: &Machine, path: &str) {
    let (w, h, px) = m.soc.lcdc.render(&m.ram);
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, png::encode_for(path, w, h, &px)).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn mkpsp(dir: &str, out: &str) -> Result<(), String> {
    let psp = image::PARTITIONS[1].2;
    let img = image::mkjffs2(std::path::Path::new(dir), psp)?;
    std::fs::write(out, img).map_err(|e| format!("{out}: {e}"))
}

fn mknand(backup: &str, psp: &str, out: &str) -> Result<(), String> {
    let read = |p: String| std::fs::read(&p).map_err(|e| format!("{p}: {e}"));
    let mut parts = Vec::new();
    for (name, _, _) in image::PARTITIONS {
        parts.push(match name {
            "mtd1-PSP" => image::Source::Data(read(psp.to_string())?),
            "mtd6-Cache" => image::Source::Erased,
            _ => image::Source::Dump(read(format!("{backup}/{name}.nanddump"))?),
        });
    }
    let chip = image::mknand(parts)?;
    std::fs::write(out, chip).map_err(|e| format!("{out}: {e}"))
}

/// A JFFS2 image of a directory, as big as a partition.
fn mkjffs2(part: &str, dir: &str, out: &str) -> Result<(), String> {
    let n = image::partition(part).ok_or(format!("no partition {part}"))?;
    let img = image::mkjffs2(std::path::Path::new(dir), image::PARTITIONS[n].2)?;
    std::fs::write(out, img).map_err(|e| format!("{out}: {e}"))
}

/// A copy of a whole NAND image with some partitions replaced, each
/// `PART=FILE` (a plain image of the partition's size) or `PART=erased`.
fn patchnand(input: &str, out: &str, edits: &[String]) -> Result<(), String> {
    let mut chip = std::fs::read(input).map_err(|e| format!("{input}: {e}"))?;
    for e in edits {
        let (part, file) = e.split_once('=').ok_or(format!("{e}: expected PART=FILE or PART=erased"))?;
        let n = image::partition(part).ok_or(format!("no partition {part}"))?;
        let src = match file {
            "erased" => image::Source::Erased,
            f => image::Source::Data(std::fs::read(f).map_err(|e| format!("{f}: {e}"))?),
        };
        image::replace(&mut chip, n, src)?;
    }
    std::fs::write(out, chip).map_err(|e| format!("{out}: {e}"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("boot") => parse(&args[1..]).and_then(boot),
        Some("mkpsp") if args.len() == 3 => mkpsp(&args[1], &args[2]),
        Some("mkjffs2") if args.len() == 4 => mkjffs2(&args[1], &args[2], &args[3]),
        Some("patchnand") if args.len() >= 3 => patchnand(&args[1], &args[2], &args[3..]),
        Some("mknand") if args.len() == 4 => mknand(&args[1], &args[2], &args[3]),
        _ => Err(USAGE.into()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
