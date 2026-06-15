//! Multi-file unified-diff applier, workspace-confined, fuzzy hunk matching.

use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};

struct Hunk {
    old_start: usize,
    /// Context + removed lines, in file order (what we expect to match).
    old_lines: Vec<String>,
    /// Context + added lines, in file order (what we replace them with).
    new_lines: Vec<String>,
    added: usize,
    removed: usize,
}

struct FileDiff {
    path: String,
    create: bool,
    hunks: Vec<Hunk>,
}

/// Strip a leading `a/` or `b/` prefix from a diff header path.
fn strip_prefix(p: &str) -> &str {
    p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")).unwrap_or(p)
}

/// Resolve `rel` under `root`, rejecting `..` escapes (mirrors tools.rs::resolve).
fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let p = canon_root.join(rel);
    let abs = if p.exists() {
        p.canonicalize()?
    } else {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        p
    };
    if !abs.starts_with(&canon_root) {
        bail!("path escapes workspace: {rel}");
    }
    Ok(abs)
}

/// Parse a `@@ -l,s +l,s @@` header, returning (old_start, new_start).
fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let inner = line.trim_start_matches('@').trim().trim_end_matches('@').trim();
    let mut parts = inner.split_whitespace();
    let old = parts.next()?.trim_start_matches('-');
    let new = parts.next()?.trim_start_matches('+');
    let first = |s: &str| s.split(',').next().and_then(|n| n.parse::<usize>().ok());
    Some((first(old)?, first(new)?))
}

/// Split a unified diff into per-file sections.
fn parse(diff: &str) -> Result<Vec<FileDiff>> {
    let lines: Vec<&str> = diff.lines().collect();
    let mut files: Vec<FileDiff> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].starts_with("--- ") {
            i += 1;
            continue;
        }
        let old_path = lines[i][4..].trim();
        i += 1;
        if i >= lines.len() || !lines[i].starts_with("+++ ") {
            bail!("malformed diff: '---' without '+++'");
        }
        let new_path = lines[i][4..].trim();
        i += 1;
        let create = old_path == "/dev/null";
        let path = strip_prefix(if create { new_path } else { old_path }).to_string();
        let mut hunks = Vec::new();
        while i < lines.len() && lines[i].starts_with("@@") {
            let (old_start, _) =
                parse_hunk_header(lines[i]).ok_or_else(|| anyhow!("bad hunk header: {}", lines[i]))?;
            i += 1;
            let (mut old_lines, mut new_lines) = (Vec::new(), Vec::new());
            let (mut added, mut removed) = (0usize, 0usize);
            while i < lines.len() && !lines[i].starts_with("@@") && !lines[i].starts_with("--- ") {
                let l = lines[i];
                match l.chars().next() {
                    Some('+') => {
                        new_lines.push(l[1..].to_string());
                        added += 1;
                    }
                    Some('-') => {
                        old_lines.push(l[1..].to_string());
                        removed += 1;
                    }
                    Some(' ') => {
                        old_lines.push(l[1..].to_string());
                        new_lines.push(l[1..].to_string());
                    }
                    None => {
                        old_lines.push(String::new());
                        new_lines.push(String::new());
                    }
                    _ => break, // "\ No newline at end of file" etc.
                }
                i += 1;
            }
            hunks.push(Hunk { old_start, old_lines, new_lines, added, removed });
        }
        files.push(FileDiff { path, create, hunks });
    }
    if files.is_empty() {
        bail!("no file sections found in diff");
    }
    Ok(files)
}

/// Find where `needle` matches in `hay`, searching ±3 around `hint` (0-based).
fn locate(hay: &[String], needle: &[String], hint: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(hint.min(hay.len()));
    }
    let max = hay.len().saturating_sub(needle.len());
    let matches = |start: usize| hay[start..start + needle.len()] == needle[..];
    // Try the hint first, then expand outward within the window.
    for delta in 0..=3usize {
        for cand in [hint.saturating_sub(delta), hint + delta] {
            if cand <= max && matches(cand) {
                return Some(cand);
            }
        }
    }
    // Last resort: scan the whole file for a unique-enough anchor.
    (0..=max).find(|&c| matches(c))
}

/// Apply a unified diff (possibly spanning multiple files) to files under `root`.
/// Returns a human-readable summary (files changed, hunks applied) on success,
/// or an Err describing the first hunk that failed to apply cleanly.
pub fn apply_patch(root: &Path, diff: &str) -> Result<String> {
    let files = parse(diff)?;
    let mut summary = String::new();
    let (mut nfiles, mut nhunks) = (0usize, 0usize);

    for f in &files {
        let abs = resolve(root, &f.path)?;
        let (mut added, mut removed) = (0usize, 0usize);

        if f.create {
            let content: Vec<String> =
                f.hunks.iter().flat_map(|h| h.new_lines.iter().cloned()).collect();
            added = content.len();
            std::fs::write(&abs, content.join("\n") + "\n")?;
            summary.push_str(&format!("  A {} (+{} -0)\n", f.path, added));
            nfiles += 1;
            nhunks += f.hunks.len();
            continue;
        }

        let orig = std::fs::read_to_string(&abs)
            .map_err(|e| anyhow!("cannot read {}: {e}", f.path))?;
        let mut lines: Vec<String> = orig.lines().map(String::from).collect();
        let mut offset: isize = 0; // running shift from prior hunks

        for h in &f.hunks {
            let hint = ((h.old_start as isize - 1) + offset).max(0) as usize;
            let at = locate(&lines, &h.old_lines, hint).ok_or_else(|| {
                anyhow!("hunk failed to apply in {} @@ -{},{}", f.path, h.old_start, h.old_lines.len())
            })?;
            lines.splice(at..at + h.old_lines.len(), h.new_lines.iter().cloned());
            offset += h.new_lines.len() as isize - h.old_lines.len() as isize;
            added += h.added;
            removed += h.removed;
            nhunks += 1;
        }

        let mut out = lines.join("\n");
        if orig.ends_with('\n') || orig.is_empty() {
            out.push('\n');
        }
        std::fs::write(&abs, out)?;
        summary.push_str(&format!("  M {} (+{added} -{removed})\n", f.path));
        nfiles += 1;
    }

    Ok(format!("applied patch: {nfiles} file(s), {nhunks} hunk(s)\n{summary}").trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("smolcode-patch-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn applies_single_file_hunk() {
        let dir = tmpdir("edit");
        let file = dir.join("foo.txt");
        std::fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();
        let diff = "\
--- a/foo.txt
+++ b/foo.txt
@@ -1,3 +1,3 @@
 alpha
-beta
+BETA
 gamma
";
        let out = apply_patch(&dir, diff).unwrap();
        assert!(out.contains("1 file(s), 1 hunk(s)"), "summary was: {out}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "alpha\nBETA\ngamma\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn creates_new_file_from_dev_null() {
        let dir = tmpdir("create");
        let diff = "\
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
";
        apply_patch(&dir, diff).unwrap();
        let created = dir.join("new.txt");
        assert!(created.exists());
        assert_eq!(std::fs::read_to_string(&created).unwrap(), "hello\nworld\n");
        std::fs::remove_dir_all(&dir).ok();
    }
}
