//! Command hooks — opencode-style extensibility without a JS runtime.
//!
//! Config maps a lifecycle event to a shell command. The command receives a
//! JSON payload on stdin and may reply on stdout with
//! `{"decision":"allow|deny|modify","reason":"…","args":{…}}`.
//! Events: `tool.execute.before` (can deny/modify), `tool.execute.after`,
//! `session.start`, `session.idle` (fire-and-forget).

use serde::Deserialize;
use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Clone, Deserialize)]
pub struct CommandHook {
    pub event: String,
    pub command: String,
}

#[derive(Clone, Default)]
pub struct Hooks {
    pub hooks: Vec<CommandHook>,
}

pub enum Decision {
    Allow,
    Deny(String),
    Modify(String),
}

impl Hooks {
    pub fn new(hooks: Vec<CommandHook>) -> Self {
        Self { hooks }
    }

    /// Run `tool.execute.before` hooks; first deny/modify wins.
    pub fn before_tool(&self, tool: &str, args: &str) -> Decision {
        let payload = serde_json::json!({
            "event": "tool.execute.before", "tool": tool, "args": args
        })
        .to_string();
        for h in self.hooks.iter().filter(|h| h.event == "tool.execute.before") {
            if let Some(v) = run_hook(&h.command, &payload) {
                match v.get("decision").and_then(|d| d.as_str()) {
                    Some("deny") => {
                        let reason = v
                            .get("reason")
                            .and_then(|r| r.as_str())
                            .unwrap_or("denied by hook")
                            .to_string();
                        return Decision::Deny(reason);
                    }
                    Some("modify") => {
                        if let Some(a) = v.get("args") {
                            let s = if a.is_string() {
                                a.as_str().unwrap().to_string()
                            } else {
                                a.to_string()
                            };
                            return Decision::Modify(s);
                        }
                    }
                    _ => {}
                }
            }
        }
        Decision::Allow
    }

    pub fn fire(&self, event: &str, payload: serde_json::Value) {
        for h in self.hooks.iter().filter(|h| h.event == event) {
            let _ = run_hook(&h.command, &payload.to_string());
        }
    }
}

fn run_hook(command: &str, payload: &str) -> Option<serde_json::Value> {
    let mut child = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().ok()?;
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).ok()
}
