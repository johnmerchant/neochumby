//! neochumby-wasmbench: runs each workload under each runtime, in turn, from
//! the files beside it, and logs what it measured to stderr (the session's
//! log). A measurement spike for running apps as WebAssembly.
//!
//! Two files say what to run (tools/stage.sh writes them):
//!
//!   runtimes   NAME ARGV..., with {w} for the workload's name, e.g.
//!              `wamr-aot ./iwasm-aot ./aot/{w}.aot`
//!   plan       WORKLOAD RUNS [ARG ...], e.g. `float 3 5000 20`
//!
//! Each run is timed from spawn to exit. Its peak RSS is its VmHWM in
//! /proc/PID/status as it exits: it runs traced (PTRACE_TRACEME), stopped
//! only at its exec and at its exit (PTRACE_O_TRACEEXIT), where its memory
//! is still there to read. (The Chumby's 2.6.16 fills in no ru_maxrss.) Its stdout's `NAME ... ms=X` lines, and
//! CoreMark's Iterations/Sec, are its own measurements. At the end, a
//! table of medians, and each against native.

// Unsafe code, through libc: on .github/unsafe-allowed.
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::time::Instant;

use neochumby::fb::{Fb, rgb};

const BG: u16 = rgb(0, 0, 0);
const FG: u16 = rgb(255, 170, 0);
const DIM: u16 = rgb(160, 160, 160);

struct Screen(Option<Fb>);

impl Screen {
    fn show(&mut self, state: &str, line: &str) {
        let Some(fb) = self.0.as_mut() else { return };
        fb.clear(BG);
        fb.text(16, 40, "BENCH", 4, FG);
        fb.text(16, 110, state, 2, FG);
        fb.text(16, 160, line, 1, DIM);
    }
}

/// One run's results.
struct Run {
    ok: bool,
    wall_ms: f64,
    hwm_kb: Option<u64>,
    /// (metric, value): its own ms= lines, CoreMark's iterations/s.
    metrics: Vec<(String, f64)>,
    /// Its stdout's checksum lines, without their timings.
    checks: Vec<String>,
}

fn hwm_kb(pid: u32) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let l = s.lines().find(|l| l.starts_with("VmHWM:"))?;
    l.split_whitespace().nth(1)?.parse().ok()
}

fn parse(out: &str) -> (Vec<(String, f64)>, Vec<String>) {
    let mut metrics = Vec::new();
    let mut checks = Vec::new();
    for l in out.lines() {
        if let Some(v) = l.strip_prefix("Iterations/Sec   : ") {
            if let Ok(v) = v.trim().parse() {
                metrics.push(("iter/s".to_string(), v));
            }
        }
        // (Not crcfinal: it depends on the iterations, which CoreMark
        // calibrates to each runtime's speed.)
        if l.starts_with("Correct operation validated") || l.starts_with("[0]crclist") {
            checks.push(l.to_string());
        }
        let words: Vec<&str> = l.split_whitespace().collect();
        if let Some(ms) = words.iter().find_map(|w| w.strip_prefix("ms=")) {
            if let Ok(v) = ms.parse() {
                metrics.push((format!("{}_ms", words[0]), v));
            }
            checks.push(words.iter().filter(|w| !w.starts_with("ms=")).cloned().collect::<Vec<_>>().join(" "));
        }
    }
    (metrics, checks)
}

fn run(argv: &[String]) -> Run {
    let failed = |why: String| {
        eprintln!("wasmbench:   failed: {why}");
        Run { ok: false, wall_ms: 0.0, hwm_kb: None, metrics: Vec::new(), checks: Vec::new() }
    };
    let t0 = Instant::now();
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // SAFETY: ptrace(PTRACE_TRACEME) is a system call, safe between fork
    // and exec; it makes this process its tracer.
    unsafe {
        cmd.pre_exec(|| {
            if libc::ptrace(libc::PTRACE_TRACEME, 0, std::ptr::null_mut::<libc::c_void>(), std::ptr::null_mut::<libc::c_void>()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return failed(format!("{}: {e}", argv[0])),
    };
    let pid = child.id() as libc::pid_t;
    let mut hwm = None;
    // Waited for here, blocking (a sleep would round wall times to the
    // kernel's 10 ms tick), and not by std, which would take its stops for
    // exits.
    let status = loop {
        let mut st: libc::c_int = 0;
        // SAFETY: waitpid(2) on our own child, writing a live c_int.
        let r = unsafe { libc::waitpid(pid, &mut st, 0) };
        if r < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return failed(format!("wait: {e}"));
        }
        if libc::WIFSTOPPED(st) {
            let sig = libc::WSTOPSIG(st);
            let mut deliver = 0;
            if sig == libc::SIGTRAP && st >> 16 == libc::PTRACE_EVENT_EXIT {
                hwm = hwm_kb(pid as u32);
            } else if sig == libc::SIGTRAP && hwm.is_none() {
                // Its exec: from now on, stop it at its exit too.
                // SAFETY: ptrace(2) on our own stopped tracee.
                unsafe {
                    libc::ptrace(libc::PTRACE_SETOPTIONS, pid, std::ptr::null_mut::<libc::c_void>(), libc::PTRACE_O_TRACEEXIT as usize as *mut libc::c_void);
                }
                hwm = Some(0);
            } else {
                if sig == libc::SIGILL || sig == libc::SIGSEGV || sig == libc::SIGBUS {
                    // Where it faulted, and the word there: r0..r15, cpsr.
                    let mut regs = [0 as libc::c_ulong; 18];
                    // SAFETY: PTRACE_GETREGS (12 on ARM) writes 18 words
                    // into `regs`; PEEKTEXT reads a word of the tracee.
                    unsafe {
                        libc::ptrace(12, pid, std::ptr::null_mut::<libc::c_void>(), regs.as_mut_ptr() as *mut libc::c_void);
                        let pc = regs[15];
                        let word = libc::ptrace(libc::PTRACE_PEEKTEXT, pid, pc as *mut libc::c_void, std::ptr::null_mut::<libc::c_void>());
                        eprintln!(
                            "wasmbench:   signal {sig} at pc={pc:#x} insn={:#010x} lr={:#x} sp={:#x} cpsr={:#x}",
                            word as u32, regs[14], regs[13], regs[16]
                        );
                    }
                }
                deliver = sig;
            }
            // SAFETY: as above; resumes it, with any signal it was sent.
            unsafe {
                libc::ptrace(libc::PTRACE_CONT, pid, std::ptr::null_mut::<libc::c_void>(), deliver as usize as *mut libc::c_void);
            }
            continue;
        }
        break std::process::ExitStatus::from_raw(st);
    };
    let hwm = hwm.filter(|h| *h > 0);
    let wall_ms = t0.elapsed().as_secs_f64() * 1e3;
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    for l in out.lines() {
        eprintln!("wasmbench:   | {l}");
    }
    for l in err.lines() {
        eprintln!("wasmbench:   ! {l}");
    }
    let (metrics, checks) = parse(&out);
    let ok = status.success();
    eprintln!(
        "wasmbench:   {} wall_ms={wall_ms:.1} hwm_kb={}",
        if ok { "ok".to_string() } else { format!("FAILED ({status})") },
        hwm.map_or("?".to_string(), |h| h.to_string())
    );
    Run { ok, wall_ms, hwm_kb: hwm, metrics, checks }
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(v[v.len() / 2])
}

fn file_kb(p: &str) -> String {
    std::fs::metadata(p).map_or("-".to_string(), |m| format!("{:.0}", m.len() as f64 / 1024.0))
}

fn main() {
    neochumby::debug::install("neochumby-wasmbench");
    let mut screen = Screen(Fb::open().ok());
    let lines = |f: &str| -> Vec<Vec<String>> {
        std::fs::read_to_string(f)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .map(|l| l.split_whitespace().map(String::from).collect())
            .collect()
    };
    let runtimes = lines("runtimes");
    let plan = lines("plan");
    eprintln!("wasmbench: running: {} runtimes, {} workloads", runtimes.len(), plan.len());
    screen.show("running", "");

    for rt in &runtimes {
        eprintln!("wasmbench: size {} binary_kb={}", rt[0], file_kb(&rt[1]));
    }
    // results[(workload, runtime)] = runs
    let mut results: BTreeMap<(String, String), Vec<Run>> = BTreeMap::new();
    let total: usize = plan.iter().map(|p| p[1].parse::<usize>().unwrap_or(1)).sum::<usize>() * runtimes.len();
    let mut done = 0;
    for p in &plan {
        let (w, runs) = (&p[0], p[1].parse::<usize>().unwrap_or(1));
        for rt in &runtimes {
            let argv: Vec<String> =
                rt[1..].iter().map(|a| a.replace("{w}", w)).chain(p[2..].iter().cloned()).collect();
            if let Some(m) = argv.get(1).filter(|_| rt[0] != "native") {
                if std::fs::metadata(m).is_err() {
                    // Not built for this runtime (wamr-aot-nobc has only some).
                    continue;
                }
                eprintln!("wasmbench: size {w} {} module_kb={}", rt[0], file_kb(m));
            }
            for i in 0..runs {
                done += 1;
                let what = format!("{w} {} {}/{runs}", rt[0], i + 1);
                screen.show("running", &format!("{what}   ({done}/{total})"));
                eprintln!("wasmbench: run {what}: {}", argv.join(" "));
                let r = run(&argv);
                results.entry((w.clone(), rt[0].clone())).or_default().push(r);
            }
        }
    }

    eprintln!("wasmbench: === medians (ratio: native's time over this one's, or rate over native's; 1.00 = native) ===");
    for p in &plan {
        let w = &p[0];
        let native: Option<&Vec<Run>> = results.get(&(w.clone(), "native".to_string()));
        let native_checks = native.and_then(|r| r.iter().find(|r| r.ok)).map(|r| r.checks.clone());
        for rt in &runtimes {
            let Some(runs) = results.get(&(w.clone(), rt[0].clone())) else { continue };
            let good: Vec<&Run> = runs.iter().filter(|r| r.ok).collect();
            if good.is_empty() {
                eprintln!("wasmbench: result {w} {} FAILED", rt[0]);
                continue;
            }
            let mut names: Vec<String> = good[0].metrics.iter().map(|(n, _)| n.clone()).collect();
            names.push("wall_ms".to_string());
            let mut cols = Vec::new();
            for n in &names {
                let get = |r: &Run| -> Option<f64> {
                    if n == "wall_ms" { Some(r.wall_ms) } else { r.metrics.iter().find(|(m, _)| m == n).map(|(_, v)| *v) }
                };
                let m = median(good.iter().filter_map(|r| get(r)).collect());
                let nat = native.and_then(|nr| median(nr.iter().filter(|r| r.ok).filter_map(|r| get(r)).collect()));
                let ratio = match (m, nat) {
                    (Some(m), Some(nat)) if m > 0.0 && nat > 0.0 => {
                        if n == "iter/s" { format!("{:.3}", m / nat) } else { format!("{:.3}", nat / m) }
                    }
                    _ => "-".to_string(),
                };
                cols.push(format!("{n}={}(x{ratio})", m.map_or("-".to_string(), |m| format!("{m:.2}"))));
            }
            let hwm = good.iter().filter_map(|r| r.hwm_kb).max();
            let same = match &native_checks {
                Some(c) if rt[0] != "native" => {
                    if good.iter().all(|r| &r.checks == c) { " checks=same" } else { " checks=DIFFER" }
                }
                _ => "",
            };
            eprintln!(
                "wasmbench: result {w} {} {} hwm_kb={}{same}",
                rt[0],
                cols.join(" "),
                hwm.map_or("?".to_string(), |h| h.to_string())
            );
        }
    }
    eprintln!("wasmbench: done");
    screen.show("done", "results in the log");
    loop {
        neochumby::sleep_ms(1000);
    }
}
