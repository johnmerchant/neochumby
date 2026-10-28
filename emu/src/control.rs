//! The control port: a TCP listener on localhost taking one command per
//! line, for driving the emulated Chumby from scripts.
//!
//!     down X Y     touch the screen at pixel (X, Y) and hold
//!     up           release
//!     tap X Y      touch for 100 ms (emulated) and release
//!     shot FILE    write the screen to FILE as a PNG
//!     console TEXT type TEXT and Enter on the serial console
//!
//! Each command is answered with `ok` or `error: ...`, once carried out.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc::{Receiver, Sender, channel};

pub enum Command {
    Down(u32, u32),
    Up,
    Tap(u32, u32),
    Shot(String),
    Console(String),
}

/// A command and where to send its answer.
pub struct Request {
    pub cmd: Command,
    pub reply: Sender<String>,
}

fn parse(line: &str) -> Result<Command, String> {
    let w: Vec<&str> = line.split_whitespace().collect();
    let xy = |w: &[&str]| -> Result<(u32, u32), String> {
        match w {
            [x, y] => Ok((x.parse().map_err(|_| "bad x")?, y.parse().map_err(|_| "bad y")?)),
            _ => Err("expected X Y".into()),
        }
    };
    match w.first().copied() {
        Some("down") => xy(&w[1..]).map(|(x, y)| Command::Down(x, y)),
        Some("up") => Ok(Command::Up),
        Some("tap") => xy(&w[1..]).map(|(x, y)| Command::Tap(x, y)),
        Some("shot") if w.len() == 2 => Ok(Command::Shot(w[1].into())),
        Some("console") => Ok(Command::Console(line.trim_start()["console".len()..].trim_start().into())),
        _ => Err(format!("unknown command: {line}")),
    }
}

/// Listens on `addr` and forwards each command to the returned receiver.
pub fn listen(addr: &str) -> std::io::Result<Receiver<Request>> {
    let listener = TcpListener::bind(addr)?;
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let Ok(mut out) = stream.try_clone() else { return };
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let answer = match parse(&line) {
                        Ok(cmd) => {
                            let (rtx, rrx) = channel();
                            if tx.send(Request { cmd, reply: rtx }).is_err() {
                                return;
                            }
                            rrx.recv().unwrap_or_else(|_| "error: stopped".into())
                        }
                        Err(e) => format!("error: {e}"),
                    };
                    if writeln!(out, "{answer}").is_err() {
                        return;
                    }
                }
            });
        }
    });
    Ok(rx)
}
