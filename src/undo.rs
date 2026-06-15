//! Undo/redo of file edits the agent makes.
//!
//! Before each mutating write (`write_file` / `str_replace`), call
//! [`UndoStack::record`] with the workspace root and the relative path. It
//! snapshots the file's current content (or `None` if the file does not yet
//! exist). [`UndoStack::undo`] then restores that pre-edit state, and
//! [`UndoStack::redo`] reapplies it. Paths resolve the same way the tools do:
//! `root.join(rel_path)`. Nothing here panics — filesystem errors on restore
//! are best-effort and ignored, matching the tools' `.ok()` style.

use std::path::Path;

/// An entry capturing a file's content before an edit. `before == None` means
/// the file did not exist (undo should delete it).
pub struct Snapshot {
    pub path: String,
    pub before: Option<String>,
}

impl Snapshot {
    /// Read the current state of `rel_path` under `root` into a snapshot.
    /// `before` is `None` when the file is absent or unreadable.
    fn capture(root: &Path, rel_path: &str) -> Snapshot {
        let abs = root.join(rel_path);
        let before = std::fs::read_to_string(&abs).ok();
        Snapshot {
            path: rel_path.to_string(),
            before,
        }
    }

    /// Apply this snapshot to disk: write `before` (creating parent dirs) or
    /// delete the file when `before` is `None`. Errors are ignored.
    fn restore(&self, root: &Path) {
        let abs = root.join(&self.path);
        match &self.before {
            Some(content) => {
                if let Some(parent) = abs.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                std::fs::write(&abs, content).ok();
            }
            None => {
                std::fs::remove_file(&abs).ok();
            }
        }
    }
}

#[derive(Default)]
pub struct UndoStack {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the pre-edit state of a workspace-relative file. Call this BEFORE
    /// a `write_file` / `str_replace`. Reads current content (`None` if absent).
    /// Clears the redo stack (a fresh edit invalidates any undone history).
    pub fn record(&mut self, root: &Path, rel_path: &str) {
        self.undo.push(Snapshot::capture(root, rel_path));
        self.redo.clear();
    }

    /// Undo the most recent edit: restore the file to its `before` content
    /// (or delete it if `before == None`), pushing the *current* state onto the
    /// redo stack so the edit can be reapplied. Returns a human description
    /// (e.g. `"reverted src/x.rs"`) or `None` if there is nothing to undo.
    pub fn undo(&mut self, root: &Path) -> Option<String> {
        let snap = self.undo.pop()?;
        // Capture what's on disk now so redo can put it back.
        self.redo.push(Snapshot::capture(root, &snap.path));
        snap.restore(root);
        Some(format!("reverted {}", snap.path))
    }

    /// Redo the most recently undone edit: reapply the snapshot captured during
    /// undo, pushing the now-current state back onto the undo stack. Returns a
    /// description or `None` if there is nothing to redo.
    pub fn redo(&mut self, root: &Path) -> Option<String> {
        let snap = self.redo.pop()?;
        // Capture current state so it can be undone again.
        self.undo.push(Snapshot::capture(root, &snap.path));
        snap.restore(root);
        Some(format!("reapplied {}", snap.path))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("smolcode-undo-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn undo_redo_modifies_existing_file() {
        let root = tmp();
        let file = "a.txt";
        std::fs::write(root.join(file), "v1").unwrap();

        let mut s = UndoStack::new();
        s.record(&root, file); // snapshot "v1"
        std::fs::write(root.join(file), "v2").unwrap();

        assert!(s.can_undo());
        assert!(!s.can_redo());
        assert_eq!(s.undo(&root).as_deref(), Some("reverted a.txt"));
        assert_eq!(std::fs::read_to_string(root.join(file)).unwrap(), "v1");

        assert!(s.can_redo());
        assert_eq!(s.redo(&root).as_deref(), Some("reapplied a.txt"));
        assert_eq!(std::fs::read_to_string(root.join(file)).unwrap(), "v2");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undo_deletes_newly_created_file() {
        let root = tmp();
        let file = "new/nested.txt";

        let mut s = UndoStack::new();
        s.record(&root, file); // file absent -> before == None
        std::fs::create_dir_all(root.join("new")).unwrap();
        std::fs::write(root.join(file), "created").unwrap();

        s.undo(&root);
        assert!(!root.join(file).exists());

        s.redo(&root);
        assert_eq!(std::fs::read_to_string(root.join(file)).unwrap(), "created");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn record_clears_redo() {
        let root = tmp();
        let mut s = UndoStack::new();
        s.record(&root, "x.txt");
        s.undo(&root);
        assert!(s.can_redo());
        s.record(&root, "y.txt");
        assert!(!s.can_redo());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undo_on_empty_is_none() {
        let root = tmp();
        let mut s = UndoStack::new();
        assert!(s.undo(&root).is_none());
        assert!(s.redo(&root).is_none());
        std::fs::remove_dir_all(&root).ok();
    }
}
