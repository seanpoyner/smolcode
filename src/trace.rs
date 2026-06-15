//! Session-trace recorder — append-only JSONL of the agent's steps.
//! Stored under ~/.local/share/smolcode/traces/<session_id>.jsonl so a
//! session can be replayed and turned into fine-tuning / eval data.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// A single recorded event in a session trace.
#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceEvent {
    Task { text: String },
    ToolCall { name: String, args: String },
    ToolResult { name: String, text: String },
    Final { text: String },
    Error { text: String },
}

/// Append-only JSONL trace writer for one session.
pub struct Trace {
    #[allow(dead_code)] // retained for diagnostics/identification
    session_id: String,
    path: PathBuf,
    enabled: bool,
}

impl Trace {
    /// Create a trace for `session_id`. Files go under the traces dir
    /// (~/.local/share/smolcode/traces/<session_id>.jsonl). `enabled=false`
    /// makes all record() calls no-ops (cheap to keep in the hot path).
    pub fn new(session_id: &str, enabled: bool) -> Self {
        let path = traces_dir().join(format!("{session_id}.jsonl"));
        Trace {
            session_id: session_id.to_string(),
            path,
            enabled,
        }
    }

    /// Append one event as a single JSON line. Best-effort: never panics,
    /// never errors out the caller (IO failures are swallowed).
    pub fn record(&self, ev: &TraceEvent) {
        if !self.enabled {
            return;
        }
        let line = match serde_json::to_string(ev) {
            Ok(l) => l,
            Err(_) => return,
        };
        // Ensure the traces dir exists (best-effort).
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = f.write_all(line.as_bytes());
            let _ = f.write_all(b"\n");
        }
    }

    /// The on-disk path of this trace.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The traces directory (created on demand).
pub fn traces_dir() -> PathBuf {
    let dir = dirs::data_dir().unwrap_or_default().join("smolcode/traces");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Read a trace file back into events (for export/eval). Returns empty on any error.
pub fn read(session_id: &str) -> Vec<TraceEvent> {
    let path = traces_dir().join(format!("{session_id}.jsonl"));
    let contents = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<TraceEvent>(l).ok())
        .collect()
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("...");
    out
}

/// Render a trace as a readable markdown transcript (for /export-style output).
pub fn to_markdown(events: &[TraceEvent]) -> String {
    let mut out = String::new();
    for ev in events {
        match ev {
            TraceEvent::Task { text } => {
                out.push_str("## Task\n");
                out.push_str(text);
                out.push('\n');
            }
            TraceEvent::ToolCall { name, args } => {
                out.push_str(&format!("**tool:** `{}` `{}`\n", name, clip(args, 200)));
            }
            TraceEvent::ToolResult { name, text } => {
                out.push_str(&format!("result of `{}`:\n", name));
                out.push_str("```\n");
                out.push_str(&clip(text, 800));
                out.push_str("\n```\n");
            }
            TraceEvent::Final { text } => {
                out.push_str("## Result\n");
                out.push_str(text);
                out.push('\n');
            }
            TraceEvent::Error { text } => {
                out.push_str(&format!("> error: {}\n", text));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_id(tag: &str) -> String {
        format!("test-{}-{}", tag, std::process::id())
    }

    #[test]
    fn round_trip() {
        let id = unique_id("rt");
        let t = Trace::new(&id, true);
        // Start clean in case a stale file exists.
        let _ = fs::remove_file(t.path());

        t.record(&TraceEvent::Task {
            text: "do the thing".to_string(),
        });
        t.record(&TraceEvent::ToolCall {
            name: "read".to_string(),
            args: "{\"path\":\"a.txt\"}".to_string(),
        });
        t.record(&TraceEvent::ToolResult {
            name: "read".to_string(),
            text: "file contents".to_string(),
        });
        t.record(&TraceEvent::Final {
            text: "all done".to_string(),
        });

        let events = read(&id);
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], TraceEvent::Task { .. }));
        assert!(matches!(events[1], TraceEvent::ToolCall { .. }));
        assert!(matches!(events[2], TraceEvent::ToolResult { .. }));
        assert!(matches!(events[3], TraceEvent::Final { .. }));

        let md = to_markdown(&events);
        assert!(md.contains("## Task"));
        assert!(md.contains("## Result"));

        let _ = fs::remove_file(t.path());
    }

    #[test]
    fn disabled_writes_nothing() {
        let id = unique_id("disabled");
        let t = Trace::new(&id, false);
        let _ = fs::remove_file(t.path());

        t.record(&TraceEvent::Task {
            text: "should not be written".to_string(),
        });

        assert!(!t.path().exists());
        assert!(read(&id).is_empty());
    }

    #[test]
    fn markdown_sections() {
        let events = vec![
            TraceEvent::Task {
                text: "T".to_string(),
            },
            TraceEvent::ToolCall {
                name: "search".to_string(),
                args: "q".to_string(),
            },
            TraceEvent::ToolResult {
                name: "search".to_string(),
                text: "hits".to_string(),
            },
            TraceEvent::Final {
                text: "F".to_string(),
            },
            TraceEvent::Error {
                text: "boom".to_string(),
            },
        ];
        let md = to_markdown(&events);
        assert!(md.contains("## Task"));
        assert!(md.contains("**tool:** `search`"));
        assert!(md.contains("```"));
        assert!(md.contains("## Result"));
        assert!(md.contains("> error: boom"));
    }
}
