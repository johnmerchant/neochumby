//! ncinit: the dist image's /init, process 1.
//!
//! It mounts the kernel's file systems, loads the NAND modules when the
//! image was flashed (not sideloaded), runs a neochumby stick's boot hook,
//! and keeps a shell on the console and one on the screen. Throughout, it
//! loads the module for each device the kernel announces (its modalias,
//! matched against depmod's modules.alias), for the devices there at boot
//! too, and reaps every process the kernel hands it.
//!
//! Process 1 must not exit: the kernel panics when it does. Nothing here
//! unwraps; what fails is logged and skipped. A panic all the same, or the
//! end of the loop, which can't happen, ends in a shell on the console.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod modules;
mod status;
mod uevent;

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use neochumby_sys::boot::{self, MountFlags};

use crate::modules::Db;
use crate::status::Status;

const PATH: &str = "/bin:/sbin:/usr/bin:/usr/sbin";
/// How long the stick gets to show up: several seconds, more when a
/// control transfer times out and the hub tries it again.
const STICK_WAIT: Duration = Duration::from_secs(15);
/// A shell that exits sooner than this after it started is restarted only
/// once this has passed, not in a loop.
const RESPAWN_GAP: Duration = Duration::from_secs(1);
/// How long the loop waits for an event before reaping.
const TICK_MS: u16 = 500;
/// The NAND's drivers, which a sideloaded kernel never loads, whatever
/// device asks for them: the flash stays the stock firmware's.
const NAND_DRIVERS: &str = "kernel/drivers/mtd/";

fn main() {
    std::panic::set_hook(Box::new(|info| {
        log(&format!("panic: {info}"));
        rescue();
    }));
    let mut init = Init::start();
    init.run();
}

/// What ncinit keeps track of.
struct Init {
    /// Handed off from the stock 2.6.16 by the sideloader, not flashed.
    sideloaded: bool,
    db: Db,
    loaded: HashSet<String>,
    events: Option<OwnedFd>,
    shells: Vec<Shell>,
    hook: Option<u32>,
}

/// A shell kept running on a terminal.
struct Shell {
    tty: &'static str,
    pid: Option<u32>,
    started: Option<Instant>,
}

impl Init {
    fn start() -> Init {
        mount_kernel_fs();
        // busybox's applets, as links, in case the image lacks one.
        run_quietly(Command::new("/bin/busybox").args(["--install", "-s", "/bin"]));

        let (db, loaded) = module_db();
        let events = match boot::uevent_socket() {
            Ok(s) => {
                screen_step("Listening for devices", Status::Ok);
                Some(s)
            }
            Err(e) => {
                step("Listening for devices", Status::Failed, &format!("no device events: {e}"));
                None
            }
        };
        let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
        let sideloaded = cmdline.split_whitespace().any(|w| w == "neochumby.sideloaded=1");
        let mut init = Init { sideloaded, db, loaded, events, shells: Vec::new(), hook: None };

        init.nand();
        // The devices already there, whose announcements nobody heard.
        init.coldplug();
        init.stick();

        // A shell on the console the kernel was given, the serial port,
        // and one on the screen (a USB keyboard types into it), unless a
        // hook runs: the screen is then its session's.
        if init.hook.is_none() && is_char_device("/dev/tty1") {
            step("Starting shells", Status::Ok, "starting shells");
            init.shells.push(Shell::new("/dev/tty1"));
        } else {
            step("Starting a shell on the console", Status::Ok, "starting shell");
        }
        init.shells.push(Shell::new("/dev/console"));
        init
    }

    fn run(&mut self) -> ! {
        loop {
            for shell in &mut self.shells {
                shell.keep_running();
            }
            self.service(TICK_MS);
            while let Some(pid) = boot::reap() {
                if self.hook == Some(pid) {
                    log("userhook2 exited");
                    self.hook = None;
                }
                for shell in &mut self.shells {
                    if shell.pid == Some(pid) {
                        shell.pid = None;
                    }
                }
            }
        }
    }

    /// Without the sideloader's marker the kernel was flashed, and the
    /// NAND's modules are loaded; a normal sideload never loads them.
    fn nand(&mut self) {
        if self.sideloaded {
            log("sideloaded; NAND modules stay unloaded");
            return;
        }
        log("no sideload marker; this kernel was flashed");
        log("loading the NAND recovery modules");
        for m in ["cmdlinepart", "mxc_nand", "mtdblock"] {
            self.load(m, None);
        }
        for line in fs::read_to_string("/proc/mtd").unwrap_or_default().lines() {
            log(&format!("/proc/mtd: {line}"));
        }
        if let Ok(dir) = fs::read_dir("/dev") {
            let mut blocks: Vec<String> = dir
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("mtdblock"))
                .collect();
            blocks.sort();
            for b in blocks {
                log(&format!("/dev/{b}"));
            }
        }
        if modules::loaded(&fs::read_to_string("/proc/modules").unwrap_or_default()).contains("mxc_nand") {
            log("module mxc_nand loaded");
        }
    }

    /// A modalias for each device on each bus.
    fn coldplug(&mut self) {
        let Ok(buses) = fs::read_dir("/sys/bus") else { return };
        let mut aliases = Vec::new();
        for bus in buses.flatten() {
            let Ok(devices) = fs::read_dir(bus.path().join("devices")) else { continue };
            for dev in devices.flatten() {
                if let Ok(a) = fs::read_to_string(dev.path().join("modalias")) {
                    aliases.push(a.trim().to_owned());
                }
            }
        }
        for a in aliases {
            self.load_for(&a);
        }
        // What those modules found, announced meanwhile.
        self.service(0);
    }

    /// A neochumby stick, as the stock boot runs one: its first partition
    /// on /mnt/usb, and its boot hook, userhook2, beside the shells.
    fn stick(&mut self) {
        let until = Instant::now() + STICK_WAIT;
        while !is_block_device("/dev/sda1") && Instant::now() < until {
            self.service(TICK_MS);
        }
        if !is_block_device("/dev/sda1") {
            screen_step("Looking for a neochumby stick", Status::Skipped);
            return;
        }
        let _ = fs::create_dir_all("/mnt/usb");
        if let Err(e) = boot::mount("/dev/sda1", Path::new("/mnt/usb"), "vfat", MountFlags::default()) {
            step("Mounting the stick on /mnt/usb", Status::Failed, &format!("/dev/sda1: {e}"));
            return;
        }
        step("Mounting the stick on /mnt/usb", Status::Ok, "stick mounted on /mnt/usb");
        if !is_executable("/mnt/usb/userhook2") {
            return;
        }
        let mut hook = Command::new("/mnt/usb/userhook2");
        hook.env("PATH", PATH).stdin(Stdio::null());
        if let Some((out, err)) = terminal("/dev/console").and_then(|f| Some((f.try_clone().ok()?, f))) {
            hook.stdout(out).stderr(err);
        }
        match hook.spawn() {
            Ok(child) => {
                step("Running the stick's userhook2", Status::Ok, "running /mnt/usb/userhook2");
                self.hook = Some(child.id());
            }
            Err(e) => step("Running the stick's userhook2", Status::Failed, &format!("userhook2: {e}")),
        }
    }

    /// Loads modules for the devices the kernel announces, waiting up to
    /// `ms` for the first announcement; returns once none is waiting.
    fn service(&mut self, ms: u16) {
        let Some(events) = self.events.as_ref() else {
            std::thread::sleep(Duration::from_millis(ms.into()));
            return;
        };
        if !boot::readable(events.as_fd(), ms).unwrap_or(false) {
            return;
        }
        let mut aliases = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match boot::recv_uevent(events.as_fd(), &mut buf) {
                Ok(Some(n)) => {
                    if let Some(e) = buf.get(..n).and_then(uevent::parse)
                        && e.action == "add"
                        && let Some(a) = e.modalias
                    {
                        aliases.push(a.to_owned());
                    }
                }
                Ok(None) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        for a in aliases {
            self.load_for(&a);
        }
    }

    fn load_for(&mut self, modalias: &str) {
        for m in self.db.matching(modalias) {
            self.load(&m, Some(modalias));
        }
    }

    /// Loads module `name` and what it needs, unless loaded already.
    fn load(&mut self, name: &str, why: Option<&str>) {
        for m in self.db.load_order(name, &self.loaded) {
            let Some(path) = self.db.module(&m).map(|m| m.path.clone()) else { continue };
            if self.sideloaded && path.starts_with(NAND_DRIVERS) {
                step(&format!("Loading {m}"), Status::Skipped, &format!("{m} stays unloaded: sideloaded"));
                self.loaded.insert(m);
                continue;
            }
            let result = File::open(Path::new(&modules_dir()).join(&path)).and_then(|f| boot::load_module(&f));
            let what = format!("Loading {m}");
            match result {
                Ok(()) => match why {
                    Some(alias) => step(&what, Status::Ok, &format!("loaded {m} for {alias}")),
                    None => step(&what, Status::Ok, &format!("loaded {m}")),
                },
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                // Not tried again, for every device it would drive.
                Err(e) => step(&what, Status::Failed, &format!("{m}: {e}")),
            }
            self.loaded.insert(m);
        }
    }
}

impl Shell {
    fn new(tty: &'static str) -> Shell {
        Shell { tty, pid: None, started: None }
    }

    /// Starts the shell if it isn't running, no sooner than RESPAWN_GAP
    /// after it last started.
    fn keep_running(&mut self) {
        if self.pid.is_some() || self.started.is_some_and(|s| s.elapsed() < RESPAWN_GAP) {
            return;
        }
        self.started = Some(Instant::now());
        let Some(tty) = terminal(self.tty) else { return };
        let (Ok(i), Ok(o)) = (tty.try_clone(), tty.try_clone()) else { return };
        let mut sh = Command::new("/bin/sh");
        sh.arg("-i").env("PATH", PATH).stdin(i).stdout(o).stderr(tty);
        boot::new_session(&mut sh);
        match sh.spawn() {
            Ok(child) => self.pid = Some(child.id()),
            Err(e) => step(
                &format!("Starting a shell on {}", self.tty),
                Status::Failed,
                &format!("{}: /bin/sh: {e}", self.tty),
            ),
        }
    }
}

fn mount_kernel_fs() {
    let hardened = MountFlags { nosuid: true, nodev: true, noexec: true };
    let mut ok = true;
    for (dir, fstype, flags) in [
        ("/proc", "proc", hardened),
        ("/sys", "sysfs", hardened),
        ("/dev", "devtmpfs", MountFlags { nosuid: true, ..MountFlags::default() }),
    ] {
        let _ = fs::create_dir_all(dir);
        if let Err(e) = boot::mount(fstype, Path::new(dir), fstype, flags) {
            // Mounted already.
            if e.kind() != io::ErrorKind::ResourceBusy {
                kmsg(&format!("{dir}: {e}"));
                ok = false;
            }
        }
    }
    // Only now: the screen's console is in /dev, devtmpfs.
    screen_step("Mounting /proc, /sys and /dev", if ok { Status::Ok } else { Status::Failed });
    for dir in ["/tmp", "/mnt"] {
        let _ = fs::create_dir_all(dir);
    }
}

fn modules_dir() -> String {
    let release = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    format!("/lib/modules/{}", release.trim())
}

/// depmod's lists for the running kernel, and what's loaded already.
fn module_db() -> (Db, HashSet<String>) {
    let dir = modules_dir();
    let dep = fs::read_to_string(format!("{dir}/modules.dep"));
    let alias = fs::read_to_string(format!("{dir}/modules.alias"));
    let db = match (dep, alias) {
        (Ok(dep), Ok(alias)) => Db::parse(&dep, &alias),
        (Err(e), _) | (_, Err(e)) => {
            step("Reading the module lists", Status::Failed, &format!("{dir}: no module lists: {e}"));
            Db::default()
        }
    };
    (db, modules::loaded(&fs::read_to_string("/proc/modules").unwrap_or_default()))
}

/// A message to the kernel's log, which is how init normally speaks (and
/// which the serial console shows), and to the screen, so that it shows on
/// the Chumby itself.
fn log(msg: &str) {
    kmsg(msg);
    screen(&format!("{msg}\n"));
}

/// A step that ended `status`: `msg` in the kernel's log as ever, and on
/// the screen `what` with its result, as Red Hat's init scripts show it.
fn step(what: &str, status: Status, msg: &str) {
    kmsg(msg);
    screen_step(what, status);
}

/// A step on the screen only.
fn screen_step(what: &str, status: Status) {
    let cols =
        OpenOptions::new().read(true).open("/dev/tty1").ok().and_then(|t| boot::columns(&t)).map_or(64, usize::from);
    screen(&status::line(what, status, cols));
}

fn kmsg(msg: &str) {
    if let Ok(mut k) = OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = k.write_all(format!("init: {msg}\n").as_bytes());
    }
}

/// The screen's console, tty1, if there is one.
fn screen(text: &str) {
    if is_char_device("/dev/tty1")
        && let Ok(mut t) = OpenOptions::new().write(true).open("/dev/tty1")
    {
        let _ = t.write_all(text.as_bytes());
    }
}

fn terminal(path: &str) -> Option<File> {
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => Some(f),
        Err(e) => {
            kmsg(&format!("{path}: {e}"));
            None
        }
    }
}

fn run_quietly(cmd: &mut Command) {
    let _ = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn is_char_device(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|m| m.file_type().is_char_device())
}

fn is_block_device(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|m| m.file_type().is_block_device())
}

fn is_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The last resort: a shell on the console in place of ncinit, which as
/// process 1 then becomes the shell. If even that fails, wait forever: the
/// kernel panics if process 1 exits.
fn rescue() -> ! {
    if let Some(console) = terminal("/dev/console")
        && let (Ok(i), Ok(o)) = (console.try_clone(), console.try_clone())
    {
        let e = Command::new("/bin/sh").arg("-i").env("PATH", PATH).stdin(i).stdout(o).stderr(console).exec();
        log(&format!("rescue shell: {e}"));
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
