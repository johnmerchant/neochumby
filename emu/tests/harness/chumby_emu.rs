//! The chumby-emu backend: this crate's own emulator, run as a process.
//! The console is its stdin and stdout; touches and screen dumps go over
//! its control port (src/control.rs), each command answered once done.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::backend::{Backend, Booted, Machine, Media, Result};

pub struct ChumbyEmu {
    pub exe: PathBuf,
}

impl ChumbyEmu {
    /// The chumby-emu cargo built alongside these tests.
    pub fn built() -> ChumbyEmu {
        ChumbyEmu { exe: PathBuf::from(env!("CARGO_BIN_EXE_chumby-emu")) }
    }
}

/// A free port on localhost, for the control port.
fn free_port() -> Result<u16> {
    let l = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("no free port: {e}"))?;
    Ok(l.local_addr().map_err(|e| e.to_string())?.port())
}

impl Backend for ChumbyEmu {
    fn name(&self) -> &'static str {
        "chumby-emu"
    }

    fn boot(&self, media: &Media, work: &Path) -> Result<Booted> {
        if media.stick.is_some() {
            return Err("chumby-emu has no USB storage yet".into());
        }
        let port = free_port()?;
        let addr = format!("127.0.0.1:{port}");
        let stderr = File::create(work.join("emu-stderr.log")).map_err(|e| e.to_string())?;
        let mut child = Command::new(&self.exe)
            .arg("boot")
            .arg("--kernel")
            .arg(&media.kernel)
            .arg("--nand")
            .arg(&media.nand)
            .args(["--nand-ro", "--quiet-mmio", "--control", &addr])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .map_err(|e| format!("{}: {e}", self.exe.display()))?;
        let console_in = Box::new(child.stdin.take().unwrap());
        let console_out = Box::new(child.stdout.take().unwrap());
        // The control port listens once the machine is set up.
        let deadline = Instant::now() + Duration::from_secs(20);
        let control = loop {
            match TcpStream::connect(&addr) {
                Ok(s) => break s,
                Err(e) if Instant::now() > deadline => {
                    let _ = child.kill();
                    return Err(format!("control port {addr}: {e}"));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        let reader = BufReader::new(control.try_clone().map_err(|e| e.to_string())?);
        Ok(Booted { console_out, console_in, machine: Box::new(Running { child, control, reader }) })
    }
}

struct Running {
    child: Child,
    control: TcpStream,
    reader: BufReader<TcpStream>,
}

impl Running {
    fn command(&mut self, line: &str) -> Result<()> {
        writeln!(self.control, "{line}").map_err(|e| format!("control: {e}"))?;
        let mut answer = String::new();
        self.reader.read_line(&mut answer).map_err(|e| format!("control: {e}"))?;
        match answer.trim() {
            "ok" => Ok(()),
            "" => Err(format!("{line}: the emulator stopped")),
            other => Err(format!("{line}: {other}")),
        }
    }
}

impl Machine for Running {
    fn press(&mut self, x: u32, y: u32) -> Result<()> {
        self.command(&format!("down {x} {y}"))
    }

    fn release(&mut self) -> Result<()> {
        self.command("up")
    }

    fn tap(&mut self, x: u32, y: u32, hold: Duration) -> Result<()> {
        self.command(&format!("tap {x} {y} {}", hold.as_millis()))
    }

    fn screendump(&mut self, path: &Path) -> Result<()> {
        let p = path.to_str().ok_or("screen dump path is not UTF-8")?;
        if p.contains(char::is_whitespace) {
            return Err(format!("{p}: the control port takes no spaces in paths"));
        }
        self.command(&format!("shot {p}"))
    }

    fn stop(&mut self) -> Result<()> {
        let _ = self.child.kill();
        self.child.wait().map(|_| ()).map_err(|e| e.to_string())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
