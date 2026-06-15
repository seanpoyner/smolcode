//! Session persistence — save/restore conversations like opencode.
//! Stored as JSON under ~/.local/share/smolcode/sessions/<id>.json.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
pub struct StoredMsg {
    pub role: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub created: u64,
    pub updated: u64,
    pub lines: Vec<StoredMsg>,
    pub convo: Vec<(String, String)>,
}

pub struct Meta {
    pub id: String,
    pub title: String,
    pub updated: u64,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn new_id() -> String {
    // Nanosecond clock plus a monotonically-increasing counter, so two ids
    // minted in the same instant (e.g. a session and an immediate fork) never
    // collide — a collision would make them share a file and overwrite.
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{ns}-{seq}")
}

fn dir() -> PathBuf {
    let d = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("smolcode")
        .join("sessions");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn save(s: &Session) {
    if s.lines.is_empty() {
        return;
    }
    if let Ok(j) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(dir().join(format!("{}.json", s.id)), j);
    }
}

pub fn load(id: &str) -> Option<Session> {
    serde_json::from_str(&std::fs::read_to_string(dir().join(format!("{id}.json"))).ok()?).ok()
}

pub fn list() -> Vec<Meta> {
    let mut metas = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir()) {
        for e in rd.flatten() {
            if e.path().extension().map_or(false, |x| x == "json") {
                if let Ok(txt) = std::fs::read_to_string(e.path()) {
                    if let Ok(s) = serde_json::from_str::<Session>(&txt) {
                        metas.push(Meta { id: s.id, title: s.title, updated: s.updated });
                    }
                }
            }
        }
    }
    metas.sort_by(|a, b| b.updated.cmp(&a.updated));
    metas
}

pub fn rel_time(t: u64) -> String {
    let d = now().saturating_sub(t);
    if d < 60 {
        format!("{d}s ago")
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86400)
    }
}
