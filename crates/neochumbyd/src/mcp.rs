//! `neochumbyd mcp`: the developer link for an MCP client (Claude Code, say),
//! as a Model Context Protocol server on stdin and stdout. JSON-RPC 2.0, a
//! message a line. Its tools are the commands a developer runs:
//!
//!   chumbys      the paired Chumbys, their settings, those asking to pair
//!                and the start and players in the tree (`neochumbyd
//!                pairing`)
//!   log          the end of a Chumby's log (`neochumbyd log`)
//!   restart      its session starts again from the tree (`neochumbyd restart`)
//!   screenshot   its screen, as an image (`neochumbyd shot`)
//!   set          one of its settings (`neochumbyd set`)
//!
//! Approving, refusing and forgetting a pairing are left out: approving is a
//! person comparing the code on two screens. Like the commands, it works
//! through the state directory, so it runs wherever they do and needs no
//! port of its own: `ssh HOST neochumbyd mcp` serves a client elsewhere.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use neochumby_proto::settings::key;
use serde_json::{Value, json};

use crate::dev;
use crate::state::State;
use crate::tree;

/// The protocol versions it speaks, newest first. It uses nothing that
/// differs between them.
const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Lines of log `log` returns unless asked for more.
const LOG_DEFAULT: usize = 200;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

pub struct Server {
    pub state: State,
    pub state_dir: PathBuf,
    pub tree: PathBuf,
    /// How long restart and screenshot wait for a Chumby's link.
    pub wait: Duration,
}

/// Answers what comes on `input` until it ends.
pub fn run(server: &Server, input: impl BufRead, mut out: impl Write) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.handle(&line) {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

/// A tool's result: text, or text saying what went wrong.
fn text(r: Result<String, String>) -> Value {
    match r {
        Ok(t) => json!({"content": [{"type": "text", "text": t}]}),
        Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
    }
}

fn tools() -> Value {
    let name = json!({"type": "string", "description": "The Chumby's name, as paired"});
    json!([
        {
            "name": "chumbys",
            "description": "The Chumbys paired with this neochumbyd and their settings, those asking to pair, and the apps in the tree that may be a start app or the player",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true},
        },
        {
            "name": "log",
            "description": "The end of a paired Chumby's log: what its session, apps and services wrote, as it sent it, oldest first. Lines starting `-- neochumbyd:` are the server's own notes (connected, restart asked, lines dropped)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": name,
                    "lines": {"type": "integer", "minimum": 1, "maximum": dev::LOG_LINES, "description": format!("How many of the last lines (default {LOG_DEFAULT})")},
                },
                "required": ["name"],
            },
            "annotations": {"readOnlyHint": true},
        },
        {
            "name": "restart",
            "description": "Ends a paired Chumby's apps and services, as CHUMBY MODE does, and starts its session again from the tree: how a newly deployed tree is picked up. Fails if the Chumby isn't connected",
            "inputSchema": {"type": "object", "properties": {"name": name}, "required": ["name"]},
            "annotations": {"destructiveHint": true, "idempotentHint": false},
        },
        {
            "name": "screenshot",
            "description": "A paired Chumby's screen (320x240) as a PNG. Fails if the Chumby isn't connected",
            "inputSchema": {"type": "object", "properties": {"name": name}, "required": ["name"]},
            "annotations": {"readOnlyHint": true},
        },
        {
            "name": "set",
            "description": "Sets one of a paired Chumby's settings, sent to it when it next connects: `start` (its start app; see chumbys for those in the tree), `player` (its default player, which MUSIC buttons open; likewise) or `delivery` (`nfs` or `sync`). An empty value removes the setting. Not `dev` (remote input), which is the owner's to turn on",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": name,
                    "key": {"type": "string", "description": "a-z and _"},
                    "value": {"type": "string", "description": "A-Z a-z 0-9 . : / _ -, or empty to remove"},
                },
                "required": ["name", "key", "value"],
            },
            "annotations": {"idempotentHint": true},
        },
    ])
}

impl Server {
    /// The reply to one message, if it needs one.
    pub fn handle(&self, line: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(e) => return Some(error(Value::Null, PARSE_ERROR, e.to_string())),
        };
        let Some(msg) = msg.as_object() else {
            return Some(error(Value::Null, INVALID_REQUEST, "not a request"));
        };
        // A notification (initialized, cancelled) needs nothing, and a
        // response answers nothing: it asks nothing of the client.
        let (Some(id), Some(method)) = (msg.get("id"), msg.get("method")) else {
            return None;
        };
        let id = id.clone();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method.as_str().unwrap_or_default() {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or_default();
                let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "neochumbyd", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Develop on the Chumbys paired with this neochumbyd: read a Chumby's log, restart its session on a newly deployed tree, and see its screen. Start with chumbys to learn their names.",
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools": tools()}),
            "tools/call" => match self.call(&params) {
                Ok(r) => r,
                Err(e) => return Some(error(id, INVALID_PARAMS, e)),
            },
            m => return Some(error(id, METHOD_NOT_FOUND, format!("no method {m}"))),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    /// A tool's result. Err only for a call that names no tool of ours or
    /// lacks what the tool needs; a tool that fails says so in its result.
    fn call(&self, params: &Value) -> Result<Value, String> {
        let args = &params["arguments"];
        let arg = |key: &str| args[key].as_str().ok_or_else(|| format!("{key} is required, a string"));
        Ok(match params["name"].as_str().unwrap_or_default() {
            "chumbys" => text(self.chumbys()),
            "log" => {
                let name = arg("name")?;
                let lines = match &args["lines"] {
                    Value::Null => LOG_DEFAULT,
                    n => n.as_u64().filter(|n| *n > 0).ok_or("lines is a positive integer")? as usize,
                };
                text(self.log(name, lines.min(dev::LOG_LINES)))
            }
            "restart" => {
                let name = arg("name")?;
                text(self.paired(name).and_then(|name| {
                    dev::restart(&self.state_dir, &name, self.wait).map_err(|e| e.to_string())?;
                    Ok(format!("{name}: restarting"))
                }))
            }
            "screenshot" => {
                let name = arg("name")?;
                match self
                    .paired(name)
                    .and_then(|n| dev::shot_png(&self.state_dir, &n, self.wait).map_err(|e| e.to_string()))
                {
                    Ok(png) => json!({"content": [{"type": "image", "data": base64(&png), "mimeType": "image/png"}]}),
                    Err(e) => text(Err(e)),
                }
            }
            "set" => text(self.set(arg("name")?, arg("key")?, arg("value")?)),
            t => return Err(format!("no tool {t}")),
        })
    }

    fn paired(&self, name: &str) -> Result<String, String> {
        let pairs = self.state.pairs().map_err(|e| e.to_string())?;
        match pairs.iter().any(|(n, _)| n == name) {
            true => Ok(name.to_string()),
            false => Err(format!("no Chumby paired as {name}")),
        }
    }

    fn chumbys(&self) -> Result<String, String> {
        let (starts, players) = (tree::start_apps(&self.tree), tree::players(&self.tree));
        let mut out = String::new();
        for (name, key) in self.state.pairs().map_err(|e| e.to_string())? {
            let pairs: Vec<String> = self.state.config(&name).iter().map(|(k, v)| format!("{k}={v}")).collect();
            out += &format!("paired   {name}  {key}  {}\n", pairs.join(" "));
            let settings = self.state.settings(&name);
            let notes = [
                tree::start_note(&name, settings.start.as_deref(), &starts),
                tree::player_note(&name, settings.player.as_deref(), &players),
            ];
            for note in notes.into_iter().flatten() {
                out += &format!("         {note}\n");
            }
        }
        for p in self.state.pending().map_err(|e| e.to_string())? {
            out += &format!(
                "asking   {}  code {}  from {} (approve it on the server, typing the code from the Chumby's screen)\n",
                p.name, p.code, p.addr
            );
        }
        if out.is_empty() {
            out += "no Chumbys paired or asking\n";
        }
        out += &tree::hint(&starts);
        out += &format!("\n{}", tree::player_hint(&players));
        Ok(out)
    }

    fn log(&self, name: &str, lines: usize) -> Result<String, String> {
        let name = self.paired(name)?;
        let log = dev::read_log(&self.state_dir, &name).map_err(|e| format!("{name}: {e}"))?;
        let log = String::from_utf8_lossy(&log);
        let all: Vec<&str> = log.lines().collect();
        Ok(all[all.len().saturating_sub(lines)..].join("\n"))
    }

    fn set(&self, name: &str, key: &str, value: &str) -> Result<String, String> {
        // Remote input is the Chumby's owner's to turn on, not a client's.
        if key == key::DEV {
            return Err(format!(
                "{key} is set by the Chumby's owner, with `neochumbyd set {name} {key}=on`, not from here"
            ));
        }
        self.state.set(name, key, value).map_err(|e| e.to_string())?;
        let mut out = match value {
            "" => format!("{name}: {key} removed"),
            _ => format!("{name}: {key}={value}"),
        };
        if key == key::START {
            let starts = tree::start_apps(&self.tree);
            if !value.is_empty() && !starts.iter().any(|s| s == value) {
                out += &format!("\n{value} isn't a start app in the tree; the Chumby will open {}", tree::HOME);
            }
            out += &format!("\n{}", tree::hint(&starts));
        }
        if key == key::PLAYER {
            let players = tree::players(&self.tree);
            if let Some(note) = tree::player_note(name, Some(value).filter(|v| !v.is_empty()), &players) {
                out += &format!("\n{note}");
            }
            out += &format!("\n{}", tree::player_hint(&players));
        }
        Ok(out)
    }
}

/// Standard base64, padded: how MCP carries an image.
fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = c.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(test: &str) -> Server {
        let d = std::env::temp_dir().join(format!("neochumbyd-mcp-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let state = State::open(d.clone()).unwrap();
        std::fs::write(d.join("paired").join("ab".repeat(32)), "den\n").unwrap();
        Server { state, tree: d.join("tree"), state_dir: d, wait: Duration::from_millis(300) }
    }

    fn ask(s: &Server, method: &str, params: Value) -> Value {
        let line = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}).to_string();
        let reply = s.handle(&line).unwrap();
        assert_eq!(reply["id"], 7);
        reply
    }

    fn call(s: &Server, tool: &str, args: Value) -> Value {
        ask(s, "tools/call", json!({"name": tool, "arguments": args}))["result"].clone()
    }

    #[test]
    fn it_initializes_in_the_version_asked_for() {
        let s = server("init");
        let r = ask(&s, "initialize", json!({"protocolVersion": "2025-03-26", "capabilities": {}}));
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert!(r["result"]["capabilities"]["tools"].is_object());
        let r = ask(&s, "initialize", json!({"protocolVersion": "1999-01-01"}));
        assert_eq!(r["result"]["protocolVersion"], VERSIONS[0]);
        assert_eq!(s.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#), None);
    }

    #[test]
    fn it_lists_its_tools() {
        let s = server("list");
        let r = ask(&s, "tools/list", json!({}));
        let names: Vec<&str> =
            r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["chumbys", "log", "restart", "screenshot", "set"]);
    }

    #[test]
    fn bad_messages_get_errors() {
        let s = server("errors");
        assert_eq!(s.handle("{not json").unwrap()["error"]["code"], PARSE_ERROR);
        assert_eq!(ask(&s, "resources/list", json!({}))["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(ask(&s, "tools/call", json!({"name": "approve"}))["error"]["code"], INVALID_PARAMS);
        assert_eq!(ask(&s, "tools/call", json!({"name": "log", "arguments": {}}))["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn it_lists_chumbys_and_sets_their_settings() {
        let s = server("set");
        assert!(call(&s, "chumbys", json!({}))["content"][0]["text"].as_str().unwrap().contains("paired   den"));
        let r = call(&s, "set", json!({"name": "den", "key": "delivery", "value": "sync"}));
        assert_eq!(r["isError"], Value::Null);
        assert_eq!(s.state.config("den"), [("delivery".to_string(), "sync".to_string())]);
        let r = call(&s, "set", json!({"name": "attic", "key": "delivery", "value": "sync"}));
        assert_eq!(r["isError"], true);
        // Remote input stays the owner's to turn on.
        let r = call(&s, "set", json!({"name": "den", "key": key::DEV, "value": "on"}));
        assert_eq!(r["isError"], true);
        assert_eq!(s.state.config("den"), [("delivery".to_string(), "sync".to_string())]);
    }

    #[test]
    fn log_returns_its_end() {
        let s = server("log");
        assert_eq!(call(&s, "log", json!({"name": "den"}))["isError"], true);
        let mut log = dev::LogStore::open(&s.state_dir, "den", dev::LOG_LINES).unwrap();
        for i in 0..5 {
            log.append(&format!("line {i}")).unwrap();
        }
        let r = call(&s, "log", json!({"name": "den", "lines": 2}));
        assert_eq!(r["content"][0]["text"], "line 3\nline 4");
        assert_eq!(call(&s, "log", json!({"name": "nobody"}))["isError"], true);
    }

    #[test]
    fn a_chumby_not_connected_is_an_error_not_a_hang() {
        let s = server("unlinked");
        let r = call(&s, "screenshot", json!({"name": "den"}));
        assert_eq!(r["isError"], true);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("isn't connected"));
        assert_eq!(call(&s, "restart", json!({"name": "den"}))["isError"], true);
    }

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
