//! Git tools for the smolcode agent: inspect status/diff/log and, with
//! approval, stage-all + commit. All functions run `git` directly (arg
//! vectors, never a shell), are confined to the workspace `root`, and never
//! panic — failures come back as human/model-readable strings.

use std::path::Path;
use std::process::{Command, Output};

/// Run `git <args>` in `root`. Never goes through a shell, so arguments with
/// spaces or quotes (e.g. commit messages) are passed verbatim.
fn run(root: &Path, args: &[&str]) -> std::io::Result<Output> {
    Command::new("git").args(args).current_dir(root).output()
}

/// Trimmed stdout of a `git` invocation, or an empty string on any failure.
fn out(root: &Path, args: &[&str]) -> String {
    match run(root, args) {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim_end().to_string(),
        Err(_) => String::new(),
    }
}

/// Cap a string to ~`max` chars on a char boundary, mirroring tools.rs::clip.
fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}\n...[truncated {} bytes]", &s[..end], s.len() - end)
    }
}

/// True if `root` is inside a git work tree.
pub fn is_repo(root: &Path) -> bool {
    match run(root, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(o) => o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true",
        Err(_) => false,
    }
}

/// `git status --short --branch` plus a one-line summary. "(not a git repo)"
/// if `root` isn't inside a work tree.
pub fn status(root: &Path) -> String {
    if !is_repo(root) {
        return "(not a git repo)".into();
    }
    let full = match run(root, &["status", "--short", "--branch"]) {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return "(git not available)".into(),
    };
    let mut lines = full.lines();
    let branch = lines.next().unwrap_or("").to_string();
    let body: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();
    if body.is_empty() {
        return format!("clean (nothing to commit)\n{branch}");
    }
    let summary = format!("{} change(s)", body.len());
    clip(&format!("{summary}\n{branch}\n{}", body.join("\n")), 6000)
}

/// `git diff` of unstaged+staged changes (a `--stat` summary first, then the
/// patch). Optional `path` narrows it (empty = whole tree). "(no changes)"
/// when empty.
pub fn diff(root: &Path, path: &str) -> String {
    if !is_repo(root) {
        return "(not a git repo)".into();
    }
    // Combine working tree + index against HEAD so staged changes show too.
    let mut stat_args = vec!["diff", "HEAD", "--stat"];
    let mut patch_args = vec!["diff", "HEAD"];
    let p = path.trim();
    if !p.is_empty() {
        stat_args.extend_from_slice(&["--", p]);
        patch_args.extend_from_slice(&["--", p]);
    }
    let stat = out(root, &stat_args);
    let patch = out(root, &patch_args);
    if stat.is_empty() && patch.is_empty() {
        return "(no changes)".into();
    }
    let combined = if stat.is_empty() {
        patch
    } else {
        format!("{stat}\n\n{patch}")
    };
    clip(&combined, 6000)
}

/// `git log --oneline -n <count>` (count clamped to 1..=50, default 15).
pub fn log(root: &Path, count: usize) -> String {
    if !is_repo(root) {
        return "(not a git repo)".into();
    }
    let n = if count == 0 { 15 } else { count.clamp(1, 50) };
    let n_str = n.to_string();
    match run(root, &["log", "--oneline", "-n", &n_str]) {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout).trim_end().to_string();
            if s.is_empty() {
                "(no commits yet)".into()
            } else {
                clip(&s, 6000)
            }
        }
        Err(_) => "(git not available)".into(),
    }
}

/// Stage all changes and commit with `message`. Returns the resulting
/// `git log --oneline -1` line (prefixed "committed: ") on success, or an
/// error string. Refuses to commit when there is nothing staged/changed.
/// The message is passed as a single arg — never shell-interpolated.
pub fn commit(root: &Path, message: &str) -> String {
    if !is_repo(root) {
        return "(not a git repo)".into();
    }
    if let Err(_) = run(root, &["add", "-A"]) {
        return "(git not available)".into();
    }
    // Anything staged/changed after `add -A`?
    let porcelain = out(root, &["status", "--porcelain"]);
    if porcelain.trim().is_empty() {
        return "nothing to commit".into();
    }
    match run(root, &["commit", "-m", message]) {
        Ok(o) if o.status.success() => {
            let line = out(root, &["log", "--oneline", "-1"]);
            format!("committed: {line}")
        }
        Ok(o) => {
            let se = String::from_utf8_lossy(&o.stderr).trim_end().to_string();
            let so = String::from_utf8_lossy(&o.stdout).trim_end().to_string();
            let msg = if se.is_empty() { so } else { se };
            format!("commit failed: {}", msg.trim())
        }
        Err(_) => "(git not available)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_repo() -> PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!("smolcode-git-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        // Best-effort init + identity; if git is absent these just fail.
        let _ = Command::new("git").arg("init").current_dir(&d).output();
        let _ = Command::new("git")
            .args(["-C", d.to_str().unwrap(), "config", "user.email", "t@example.com"])
            .output();
        let _ = Command::new("git")
            .args(["-C", d.to_str().unwrap(), "config", "user.name", "Test"])
            .output();
        d
    }

    #[test]
    fn git_roundtrip() {
        let dir = temp_repo();
        // No git installed (or init failed): exercise the not-a-repo path and bail.
        if !is_repo(&dir) {
            assert!(!is_repo(&dir));
            let _ = fs::remove_dir_all(&dir);
            return;
        }

        fs::write(dir.join("hello.txt"), "hi\n").unwrap();

        let st = status(&dir);
        assert!(st.contains("hello.txt"), "status should list the new file: {st}");

        let c1 = commit(&dir, "add hello with spaces & quotes \"x\"");
        assert!(c1.starts_with("committed: "), "first commit: {c1}");

        let c2 = commit(&dir, "noop");
        assert_eq!(c2, "nothing to commit");

        let lg = log(&dir, 5);
        assert!(!lg.is_empty() && !lg.starts_with('('), "log should have entries: {lg}");

        // A fresh change should be visible to diff().
        fs::write(dir.join("hello.txt"), "hi there\n").unwrap();
        let d = diff(&dir, "");
        assert!(d.contains("hello.txt") || d.contains("hi there"), "diff: {d}");

        let _ = fs::remove_dir_all(&dir);
    }
}
