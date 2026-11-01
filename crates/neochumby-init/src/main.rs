//! ncinit: the dist image's /init, process 1.
//!
//! It mounts the kernel's file systems, joins the network the stock software
//! was on (handed over at /psp, in RAM: network.rs), runs the stick's boot
//! hook, the one hook there is, and keeps a shell on the console and one on
//! the screen. With no stick there is nothing to run but the shells. Throughout, it
//! loads the module for each device the kernel announces (its modalias,
//! matched against depmod's modules.alias), for the devices there at boot
//! too, and reaps every process the kernel hands it.
//!
//! Process 1 must not exit: the kernel panics when it does. Nothing here
//! unwraps; what fails is logged and skipped. A panic all the same, or the
//! end of the loop, which can't happen, ends in a shell on the console.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod logo;
mod modules;
mod network;
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

const STARTING: &str = "Starting neochumby";
const PATH: &str = "/bin:/sbin:/usr/bin:/usr/sbin";
/// How long the stick gets to show up: several seconds, more when a
/// control transfer times out and the hub tries it again.
const STICK_WAIT: Duration = Duration::from_secs(15);
/// How long the network adapter gets to show up, once it has a stick's
/// wait behind it.
const ADAPTER_WAIT: Duration = Duration::from_secs(20);
/// What the session leaves just before it opens its first app, which then has
/// the screen: nothing more is drawn on it from here.
const SESSION_STARTED: &str = "/tmp/neochumby.started";
/// Waited this long for the session, it's the server that's being waited for.
const SERVER_SLOW: Duration = Duration::from_secs(25);
/// The zone, as the sideload handed it over.
const LOCALTIME: &str = "/psp/localtime";
/// Where the time comes from.
const NTP_SERVER: &str = "pool.ntp.org";
/// The stock software's network, as the sideload handed it over.
const NETWORK_CONFIG: &str = "/psp/network_config";
/// A shell that exits sooner than this after it started is restarted only
/// once this has passed, not in a loop.
const RESPAWN_GAP: Duration = Duration::from_secs(1);
/// How long the loop waits for an event before reaping.
const TICK_MS: u16 = 500;

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
        let mut init = Init { db, loaded, events, shells: Vec::new(), hook: None };

        // The devices already there, whose announcements nobody heard.
        init.coldplug();
        let stick_hook = init.stick();
        install_timezone();
        init.network();
        match stick_hook {
            Some(hook) => init.hook("the stick's userhook2", hook),
            None => log("no neochumby stick: nothing to run. A reboot without one is the stock firmware's"),
        }

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
        // How long the session has been coming, while it is.
        let mut coming = self.hook.map(|_| Instant::now());
        loop {
            for shell in &mut self.shells {
                shell.keep_running();
            }
            self.service(TICK_MS);
            if let Some(since) = coming {
                let started = Path::new(SESSION_STARTED).exists();
                if started || self.hook.is_none() {
                    let what = if started { STARTING } else { "Starting neochumby: its hook gave up" };
                    screen_step(what, if started { Status::Ok } else { Status::Failed });
                    coming = None;
                } else {
                    let what = if since.elapsed() < SERVER_SLOW { STARTING } else { "Waiting for the server" };
                    spin(what, since);
                }
            }
            while let Some(pid) = boot::reap() {
                if self.hook == Some(pid) {
                    log("userhook2 exited");
                    self.hook = None;
                    // The screen is free again: a shell on it, for a keyboard.
                    if is_char_device("/dev/tty1") && !self.shells.iter().any(|s| s.tty == "/dev/tty1") {
                        self.shells.push(Shell::new("/dev/tty1"));
                    }
                }
                for shell in &mut self.shells {
                    if shell.pid == Some(pid) {
                        shell.pid = None;
                    }
                }
            }
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

    /// The neochumby stick, as the stock boot has one: its first partition on
    /// /mnt/usb, read-write, for what the Chumby keeps there. Its boot hook,
    /// if it has one.
    fn stick(&mut self) -> Option<&'static str> {
        let since = Instant::now();
        let until = since + STICK_WAIT;
        while !is_block_device("/dev/sda1") && Instant::now() < until {
            spin("Looking for a neochumby stick", since);
            self.service(TICK_MS);
        }
        if !is_block_device("/dev/sda1") {
            screen_step("Looking for a neochumby stick", Status::Skipped);
            return None;
        }
        let _ = fs::create_dir_all("/mnt/usb");
        if let Err(e) = boot::mount("/dev/sda1", Path::new("/mnt/usb"), "vfat", MountFlags::default()) {
            step("Mounting the stick on /mnt/usb", Status::Failed, &format!("/dev/sda1: {e}"));
            return None;
        }
        step("Mounting the stick on /mnt/usb", Status::Ok, "stick mounted on /mnt/usb");
        is_executable("/mnt/usb/userhook2").then_some("/mnt/usb/userhook2")
    }

    /// Joins the network the stock software was on. Doesn't wait for an
    /// address: the hook does, with a deadline, and the shells needn't.
    fn network(&mut self) {
        const WHAT: &str = "Joining the network";
        let Ok(text) = fs::read_to_string(NETWORK_CONFIG) else {
            screen_step(WHAT, Status::Skipped);
            return;
        };
        // Never logs the configuration or wpa_supplicant's, which hold the key.
        let config = match network::Config::parse(&text) {
            Ok(c) => c,
            Err(e) => {
                step(WHAT, Status::Failed, &format!("{NETWORK_CONFIG}: {e}"));
                return;
            }
        };
        let interface = if config.wireless { "wlan0" } else { "eth0" };
        let since = Instant::now();
        let until = since + ADAPTER_WAIT;
        let present = |i: &str| Path::new("/sys/class/net").join(i).exists();
        while !present(interface) && Instant::now() < until {
            spin(WHAT, since);
            self.service(TICK_MS);
        }
        if !present(interface) {
            network_failed(&format!("no {interface}"));
            return;
        }
        run_quietly(Command::new("/bin/ifconfig").args(["lo", "127.0.0.1", "up"]).env("PATH", PATH));
        if config.wireless
            && let Err(e) = start_wpa_supplicant(&config, interface)
        {
            network_failed(&e);
            return;
        }
        match configure_address(&config.address, interface) {
            Ok(()) => {
                step(WHAT, Status::Ok, &format!("network: {interface}, {}", config.ssid));
                start_ntp();
                start_ssh();
            }
            Err(e) => network_failed(&e),
        }
    }

    /// Runs a boot hook beside the shells, as the stock boot runs one.
    fn hook(&mut self, what: &str, path: &str) {
        let mut hook = Command::new(path);
        hook.env("PATH", PATH).stdin(Stdio::null());
        // For the apps: a jail without `system` has no /etc/localtime.
        if let Some(tz) = fs::read(LOCALTIME).ok().and_then(|z| posix_tz(&z)) {
            hook.env("TZ", tz);
        }
        if let Some((out, err)) = terminal("/dev/console").and_then(|f| Some((f.try_clone().ok()?, f))) {
            hook.stdout(out).stderr(err);
        }
        let label = format!("Running {what}");
        match hook.spawn() {
            Ok(child) => {
                step(&label, Status::Ok, &format!("running {path}"));
                self.hook = Some(child.id());
            }
            Err(e) => step(&label, Status::Failed, &format!("{path}: {e}")),
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
        let Some(mut tty) = terminal(self.tty) else { return };
        let _ = tty.write_all(status::SHOW_CURSOR.as_bytes());
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

/// The network failed: why, on the screen, where a Chumby with no serial
/// line can show it, with what the kernel said of the USB bus and the adapter.
fn network_failed(why: &str) {
    step("Joining the network", Status::Failed, &format!("network: {why}"));
    screen(&format!("{why}\n"));
    let Ok(out) = Command::new("/bin/dmesg").env("PATH", PATH).stdin(Stdio::null()).output() else { return };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in network_lines(&text) {
        screen(&format!("{line}\n"));
    }
}

/// The last of the kernel's lines about USB and the adapter, without their
/// times, which a screen of 64 columns has no room for.
fn network_lines(dmesg: &str) -> Vec<&str> {
    const WORDS: [&str; 7] = ["usb", "rt73", "rt2x00", "wlan", "firmware", "imx21", "hub"];
    let lines: Vec<&str> = dmesg
        .lines()
        .filter(|l| WORDS.iter().any(|w| l.to_ascii_lowercase().contains(w)))
        .map(|l| l.split_once("] ").map_or(l, |(_, rest)| rest))
        .collect();
    let from = lines.len().saturating_sub(12);
    lines[from..].to_vec()
}

/// The zone the stock software was set to, where the C library looks.
/// Without it, local time is UTC.
fn install_timezone() {
    if let Err(e) = fs::copy(LOCALTIME, "/etc/localtime")
        && e.kind() != io::ErrorKind::NotFound
    {
        kmsg(&format!("timezone: {e}"));
    }
}

/// The rule a zone file (TZif, version 2 and later) ends with, as `TZ` takes
/// it: the text between its last two newlines, as in `ACST-9:30ACDT,M10.1.0,M4.1.0/3`.
fn posix_tz(zone: &[u8]) -> Option<String> {
    let body = zone.strip_suffix(b"\n")?;
    let from = body.iter().rposition(|&b| b == b'\n')? + 1;
    let rule = std::str::from_utf8(&body[from..]).ok()?;
    let plain = |c: char| c.is_ascii_alphanumeric() || "<>+-:,./".contains(c);
    (!rule.is_empty() && rule.len() <= 64 && rule.chars().all(plain) && zone.starts_with(b"TZif")).then(|| rule.to_owned())
}

/// The time, from the network, kept in step: there's no clock that runs while
/// the Chumby is off. It looks the servers up itself, as often as it takes, so
/// it needn't wait for an address. Unauthenticated, as NTP is here: nothing
/// that protects the Chumby depends on the time, its connections being to
/// pinned keys.
fn start_ntp() {
    let started = Command::new("/usr/sbin/ntpd")
        .args(["-p", NTP_SERVER])
        .env("PATH", PATH)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match started {
        Ok(s) if s.success() => kmsg("ntp: ntpd started"),
        Ok(s) => step("Starting the clock's sync", Status::Failed, &format!("ntp: ntpd: {s}")),
        Err(e) => step("Starting the clock's sync", Status::Failed, &format!("ntp: ntpd: {e}")),
    }
}

/// An SSH server, only if the owner asked for one: /psp/neochumby-ssh there,
/// and keys to let in at /psp/.ssh/authorized_keys. No password logs in. The
/// host key is made anew each boot, there being nowhere to keep it.
fn start_ssh() {
    use std::os::unix::fs::PermissionsExt;
    const WHAT: &str = "Starting the SSH server";
    if !Path::new("/psp/neochumby-ssh").exists() {
        return;
    }
    let Ok(keys) = fs::read("/psp/.ssh/authorized_keys") else {
        step(WHAT, Status::Failed, "ssh: no /psp/.ssh/authorized_keys");
        return;
    };
    let installed = fs::create_dir_all("/root/.ssh")
        .and_then(|()| fs::set_permissions("/root", fs::Permissions::from_mode(0o700)))
        .and_then(|()| fs::set_permissions("/root/.ssh", fs::Permissions::from_mode(0o700)))
        .and_then(|()| fs::write("/root/.ssh/authorized_keys", keys))
        .and_then(|()| fs::set_permissions("/root/.ssh/authorized_keys", fs::Permissions::from_mode(0o600)))
        // Where Buildroot's /etc/dropbear, a link, points.
        .and_then(|()| fs::create_dir_all("/var/run/dropbear"));
    if let Err(e) = installed {
        step(WHAT, Status::Failed, &format!("ssh: {e}"));
        screen(&format!("ssh: {e}\n"));
        return;
    }
    let started = Command::new("/usr/sbin/dropbear")
        .args(["-R", "-s", "-g"])
        .env("PATH", PATH)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match started {
        Ok(s) if s.success() => step(WHAT, Status::Ok, "ssh: dropbear started"),
        Ok(s) => step(WHAT, Status::Failed, &format!("ssh: dropbear: {s}")),
        Err(e) => step(WHAT, Status::Failed, &format!("ssh: dropbear: {e}")),
    }
}

/// wpa_supplicant, in the background, from a file only root can read.
fn start_wpa_supplicant(config: &network::Config, interface: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    const CONF: &str = "/tmp/wpa_supplicant.conf";
    let conf = config.wpa_conf().map_err(|e| format!("{NETWORK_CONFIG}: {e}"))?;
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(CONF)
        .and_then(|mut f| f.write_all(conf.as_bytes()))
        .map_err(|e| format!("{CONF}: {e}"))?;
    let status = Command::new("/usr/sbin/wpa_supplicant")
        .args(["-B", "-D", "nl80211", "-i", interface, "-c", CONF])
        .env("PATH", PATH)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("wpa_supplicant: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("wpa_supplicant: {status}")) }
}

/// The interface's address: DHCP, in the background until it has a lease
/// and keeping it after, or the stock configuration's static one.
fn configure_address(address: &network::Address, interface: &str) -> Result<(), String> {
    let run = |program: &str, args: &[&str]| -> Result<(), String> {
        let status = Command::new(program)
            .args(args)
            .env("PATH", PATH)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("{program}: {e}"))?;
        if status.success() { Ok(()) } else { Err(format!("{program}: {status}")) }
    };
    match address {
        network::Address::Dhcp => {
            run("/sbin/udhcpc", &["-b", "-i", interface, "-s", "/usr/share/udhcpc/default.script"])
        }
        network::Address::Static { ip, netmask, gateway, nameservers } => {
            run("/bin/ifconfig", &[interface, ip, "netmask", netmask, "up"])?;
            if !gateway.is_empty() {
                run("/sbin/route", &["add", "default", "gw", gateway])?;
            }
            let resolv: String = nameservers.iter().map(|n| format!("nameserver {n}\n")).collect();
            fs::write("/etc/resolv.conf", resolv).map_err(|e| format!("/etc/resolv.conf: {e}"))
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
    // Pseudo-terminals, on devtmpfs' /dev/ptmx: the session runs each
    // text-mode app on one (neochumby::term).
    let _ = fs::create_dir_all("/dev/pts");
    let pts = MountFlags { nosuid: true, noexec: true, ..MountFlags::default() };
    if let Err(e) = boot::mount("devpts", Path::new("/dev/pts"), "devpts", pts)
        && e.kind() != io::ErrorKind::ResourceBusy
    {
        kmsg(&format!("/dev/pts: {e}"));
        ok = false;
    }
    // The kernel drops what userspace writes to /dev/kmsg past a few lines
    // a second (printk_devkmsg=ratelimit, its default), which a boot's
    // burst of module loads passes: the log is init's, so keep all of it.
    let _ = fs::write("/proc/sys/kernel/printk_devkmsg", "on\n");
    // Only now: the screen's console is in /dev, devtmpfs. The logo first,
    // under the kernel's Tux, then the steps.
    screen(&logo::for_screen());
    screen(status::HIDE_CURSOR);
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

/// A step still going, in place of the console's cursor: a spinner and the
/// seconds so far, on a line the step's result replaces.
fn spin(what: &str, since: Instant) {
    let elapsed = since.elapsed();
    screen(&status::working(what, elapsed.as_millis() / u128::from(TICK_MS), elapsed.as_secs()));
}

/// A step on the screen only.
fn screen_step(what: &str, status: Status) {
    let cols =
        OpenOptions::new().read(true).open("/dev/tty1").ok().and_then(|t| boot::columns(&t)).map_or(64, usize::from);
    screen(&format!("\r{}", status::line(what, status, cols)));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zones_rule_is_its_last_line() {
        let zone = b"TZif2\0\0\xff\nACST-9:30ACDT,M10.1.0,M4.1.0/3\n";
        assert_eq!(posix_tz(zone).as_deref(), Some("ACST-9:30ACDT,M10.1.0,M4.1.0/3"));
        assert_eq!(posix_tz(b"TZif2\n<+0530>-5:30\n").as_deref(), Some("<+0530>-5:30"));
        // Not a zone file, no rule (version 1), nothing odd for a shell or the C library.
        assert_eq!(posix_tz(b"nonsense\nUTC0\n"), None);
        assert_eq!(posix_tz(b"TZif\0\0"), None);
        assert_eq!(posix_tz(b"TZif2\n$(reboot)\n"), None);
    }

    #[test]
    fn network_lines_are_the_relevant_last_few() {
        let dmesg = "[ 1.0] Booting\n[ 2.0] usb 1-1: new full-speed\n[ 3.0] other\n[ 4.0] phy0: rt2x00 failed\n";
        assert_eq!(network_lines(dmesg), ["usb 1-1: new full-speed", "phy0: rt2x00 failed"]);
        let many: String = (0..20).map(|i| format!("[ 1.0] usb {i}\n")).collect();
        let last = network_lines(&many);
        assert_eq!((last.len(), last[0], last[11]), (12, "usb 8", "usb 19"));
    }
}
