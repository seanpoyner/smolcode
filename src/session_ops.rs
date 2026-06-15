//! Session management operations (fork, rename, delete, timeline) for smolcode.
//! Thin layer over `crate::session`; never panics, returns None/false/empty on error.

use std::path::PathBuf;

use crate::session;

/// Directory where session JSON files live: `<data_dir>/smolcode/sessions`.
fn sessions_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_default().join("smolcode/sessions")
}

/// Clip `s` to at most `max` chars on a char boundary, appending "…" if truncated.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Duplicate session `id` into a brand-new session (new id, "(fork)" appended
/// to the title, fresh created/updated). Returns the new Session on success.
pub fn fork(id: &str) -> Option<session::Session> {
    let old = session::load(id)?;
    let ts = session::now();
    let new = session::Session {
        id: session::new_id(),
        title: format!("{} (fork)", old.title),
        created: ts,
        updated: ts,
        lines: old.lines,
        convo: old.convo,
    };
    session::save(&new);
    Some(new)
}

/// Rename session `id`'s title in place; persists. Returns true on success.
pub fn rename(id: &str, new_title: &str) -> bool {
    let title = new_title.trim();
    if title.is_empty() {
        return false;
    }
    match session::load(id) {
        Some(mut s) => {
            s.title = title.to_string();
            s.updated = session::now();
            session::save(&s);
            true
        }
        None => false,
    }
}

/// Delete session `id`'s JSON file from disk. Returns true if a file was removed.
pub fn delete(id: &str) -> bool {
    let path = sessions_dir().join(format!("{id}.json"));
    std::fs::remove_file(path).is_ok()
}

/// A compact human timeline of a session's turns for a detail view:
/// one line per stored message, "<index>  <role>: <clipped text>".
pub fn timeline(s: &session::Session) -> Vec<String> {
    if s.lines.is_empty() {
        return vec!["(no messages)".into()];
    }
    s.lines
        .iter()
        .enumerate()
        .map(|(i, m)| format!("{:>3}  {}: {}", i + 1, m.role, clip(&m.text, 80)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Session, StoredMsg};

    fn sample(id: &str) -> Session {
        let ts = session::now();
        Session {
            id: id.to_string(),
            title: "Original".to_string(),
            created: ts,
            updated: ts,
            lines: vec![
                StoredMsg { role: "user".into(), text: "hello world".into() },
                StoredMsg { role: "assistant".into(), text: "hi there".into() },
            ],
            convo: vec![("user".into(), "hello world".into())],
        }
    }

    #[test]
    fn fork_rename_delete_timeline_end_to_end() {
        let id = session::new_id();
        let s = sample(&id);
        session::save(&s);

        // fork
        let forked = fork(&id).expect("fork should succeed");
        assert_ne!(forked.id, id, "fork must have a new id");
        assert!(forked.title.contains("(fork)"), "fork title must be marked");
        assert_eq!(forked.lines.len(), s.lines.len());

        // rename (and empty-title rejection)
        assert!(!rename(&id, "   "), "empty title must be rejected");
        assert!(rename(&id, "Renamed"), "rename should succeed");
        let reloaded = session::load(&id).expect("load after rename");
        assert_eq!(reloaded.title, "Renamed");
        assert!(
            session::list().iter().any(|m| m.id == id && m.title == "Renamed"),
            "list must reflect the new title"
        );

        // timeline
        let tl = timeline(&reloaded);
        assert_eq!(tl.len(), reloaded.lines.len());
        let empty = Session { lines: vec![], ..sample("x") };
        assert_eq!(timeline(&empty), vec!["(no messages)".to_string()]);

        // delete both
        assert!(delete(&id), "original should delete");
        assert!(delete(&forked.id), "fork should delete");
        assert!(!delete(&id), "second delete is a no-op");

        // confirm files are gone
        assert!(!sessions_dir().join(format!("{id}.json")).exists());
        assert!(!sessions_dir().join(format!("{}.json", forked.id)).exists());
    }
}
