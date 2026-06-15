//! Atomic multi-edit tool: apply several find/replace edits to a SINGLE file
//! transactionally. Either every edit succeeds and the file is written once, or
//! nothing is written. Mirrors `tools.rs` resolve() + compact-diff conventions.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

/// One find/replace within a file.
#[derive(Clone)]
pub struct EditOp {
    pub old: String,
    pub new: String,
}

/// Resolve `rel` under `root`, rejecting `..` escapes. Inlined mini-version of
/// the `Tools::resolve` pattern in tools.rs.
fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let p = canon_root.join(rel);
    let abs = if p.exists() {
        p.canonicalize()?
    } else {
        p
    };
    if !abs.starts_with(&canon_root) {
        return Err(anyhow!("path escapes workspace: {rel}"));
    }
    Ok(abs)
}

/// Apply all `edits` to the file at `path` (relative to `root`) atomically.
/// Each edit's `old` must occur EXACTLY ONCE in the running content (after
/// prior edits applied) — zero matches or multiple matches abort the whole
/// operation with an Err naming the offending edit index. On success writes the
/// file once and returns a summary + a compact combined diff.
pub fn apply(root: &Path, path: &str, edits: &[EditOp]) -> Result<String> {
    if edits.is_empty() {
        return Err(anyhow!("no edits given"));
    }
    let abs = resolve(root, path)?;
    let original = std::fs::read_to_string(&abs)
        .map_err(|e| anyhow!("cannot read {path}: {e}"))?;

    let mut content = original.clone();
    for (i, ed) in edits.iter().enumerate() {
        if ed.old.is_empty() {
            return Err(anyhow!("edit {i}: 'old' must not be empty"));
        }
        let n = content.matches(ed.old.as_str()).count();
        if n == 0 {
            return Err(anyhow!("edit {i}: no match for the given text"));
        }
        if n > 1 {
            return Err(anyhow!(
                "edit {i}: 'old' is ambiguous (matches {n} times); include more context"
            ));
        }
        // Replace exactly the single occurrence.
        content = content.replacen(ed.old.as_str(), ed.new.as_str(), 1);
    }

    // Only now, after every edit succeeded in memory, write once.
    std::fs::write(&abs, &content).map_err(|e| anyhow!("cannot write {path}: {e}"))?;

    let diff = combined_diff(&original, &content);
    Ok(format!("applied {} edit(s) to {path}\n{diff}", edits.len()))
}

/// Parse the tool's JSON arguments into (path, edits). Expected shape:
/// {"path": "...", "edits": [{"old": "...", "new": "..."}, ...]}.
pub fn parse_args(args: &str) -> Result<(String, Vec<EditOp>)> {
    let v: serde_json::Value =
        serde_json::from_str(args).map_err(|e| anyhow!("invalid JSON arguments: {e}"))?;
    let path = v
        .get("path")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("missing string field 'path'"))?
        .to_string();
    let raw = v
        .get("edits")
        .and_then(|x| x.as_array())
        .ok_or_else(|| anyhow!("missing array field 'edits'"))?;
    let mut edits = Vec::with_capacity(raw.len());
    for (i, e) in raw.iter().enumerate() {
        let old = e
            .get("old")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("edit {i}: missing string field 'old'"))?
            .to_string();
        let new = e
            .get("new")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("edit {i}: missing string field 'new'"))?
            .to_string();
        edits.push(EditOp { old, new });
    }
    Ok((path, edits))
}

/// Compact unified-ish diff (changed lines only, capped) — matches tools.rs.
fn combined_diff(old: &str, new: &str) -> String {
    use similar::{ChangeTag, TextDiff};
    if old == new {
        return "(no changes)".into();
    }
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    let mut count = 0;
    for ch in diff.iter_all_changes() {
        let sign = match ch.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => continue,
        };
        out.push_str(sign);
        out.push_str(ch.value().trim_end_matches('\n'));
        out.push('\n');
        count += 1;
        if count >= 60 {
            out.push_str("…(diff truncated)\n");
            break;
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("smolcode-medit-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn applies_multiple_edits() {
        let root = tmp_dir();
        let file = root.join("a.txt");
        std::fs::write(&file, "alpha line\nbeta line\n").unwrap();
        let edits = vec![
            EditOp { old: "alpha line".into(), new: "ALPHA".into() },
            EditOp { old: "beta line".into(), new: "BETA".into() },
        ];
        let summary = apply(&root, "a.txt", &edits).unwrap();
        let got = std::fs::read_to_string(&file).unwrap();
        assert_eq!(got, "ALPHA\nBETA\n");
        assert!(summary.contains("2 edit"), "summary: {summary}");
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn no_match_is_atomic() {
        let root = tmp_dir();
        let file = root.join("b.txt");
        let original = "one\ntwo\n";
        std::fs::write(&file, original).unwrap();
        let edits = vec![
            EditOp { old: "one".into(), new: "ONE".into() },
            EditOp { old: "does-not-exist".into(), new: "X".into() },
        ];
        let err = apply(&root, "b.txt", &edits).unwrap_err();
        assert!(err.to_string().contains("no match"), "err: {err}");
        // Atomicity: file on disk must be unchanged (first edit not written).
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(after, original);
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn parses_valid_args() {
        let json = r#"{"path":"src/x.rs","edits":[{"old":"a","new":"b"},{"old":"c","new":"d"}]}"#;
        let (path, edits) = parse_args(json).unwrap();
        assert_eq!(path, "src/x.rs");
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].old, "a");
        assert_eq!(edits[1].new, "d");
    }

    #[test]
    fn ambiguous_old_aborts() {
        let root = tmp_dir();
        let file = root.join("c.txt");
        std::fs::write(&file, "dup\ndup\n").unwrap();
        let edits = vec![EditOp { old: "dup".into(), new: "X".into() }];
        let err = apply(&root, "c.txt", &edits).unwrap_err();
        assert!(err.to_string().contains("ambiguous"), "err: {err}");
        std::fs::remove_file(&file).ok();
    }
}
