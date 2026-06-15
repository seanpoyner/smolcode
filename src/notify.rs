//! Completion notifications for the smolcode coding agent: a terminal bell and
//! a best-effort desktop notification when a task finishes. std-only; shells out
//! to a desktop notifier if one is on PATH. Never blocks long, never panics.

use std::io::Write;
use std::process::{Command, Stdio};

/// Ring the terminal bell (writes the BEL byte to stdout). Best-effort.
pub fn bell() {
    let mut out = std::io::stdout();
    let _ = out.write_all(&[0x07]);
    let _ = out.flush();
}

/// Send a best-effort desktop notification with `title` and `body`. Returns
/// the backend used (e.g. "notify-send") or None if none was available/worked.
/// Never blocks for long and never panics.
pub fn desktop(title: &str, body: &str) -> Option<&'static str> {
    let title = sanitize(title, 200);
    let body = sanitize(body, 200);

    // 1. notify-send (Linux / freedesktop)
    if on_path("notify-send") {
        if run(Command::new("notify-send").arg(&title).arg(&body)) {
            return Some("notify-send");
        }
    }

    // 2. osascript (macOS). Pass the whole script as one -e arg; escape embedded
    // double-quotes by turning them into single-quotes so the AppleScript stays
    // well-formed.
    if on_path("osascript") {
        let t = title.replace('"', "'");
        let b = body.replace('"', "'");
        let script = format!("display notification \"{b}\" with title \"{t}\"");
        if run(Command::new("osascript").arg("-e").arg(&script)) {
            return Some("osascript");
        }
    }

    // 3. terminal-notifier (macOS, optional)
    if on_path("terminal-notifier") {
        if run(Command::new("terminal-notifier")
            .arg("-title")
            .arg(&title)
            .arg("-message")
            .arg(&body))
        {
            return Some("terminal-notifier");
        }
    }

    None
}

/// Convenience: bell + desktop notification for a finished task.
/// `ok` selects a success/"done" vs failure framing in the body.
pub fn task_done(summary: &str, ok: bool) -> Option<&'static str> {
    bell();
    let summary = sanitize(summary, 120);
    let body = if ok {
        format!("task complete: {summary}")
    } else {
        format!("task failed: {summary}")
    };
    desktop("smolcode", &body)
}

/// Spawn `cmd` with stdio nulled, wait, and report whether it exited success.
/// Never panics.
fn run(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// True if an executable file named `prog` is found on PATH.
fn on_path(prog: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    let path = match path.to_str() {
        Some(p) => p,
        None => return false,
    };
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = std::path::Path::new(dir).join(prog);
        match std::fs::metadata(&candidate) {
            Ok(md) if md.is_file() => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if md.permissions().mode() & 0o111 != 0 {
                        return true;
                    }
                }
                #[cfg(not(unix))]
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Strip control chars and newlines (replaced with spaces) and clip to `max`
/// chars so a notifier can't be made to misbehave.
pub(crate) fn sanitize(s: &str, max: usize) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bell_does_not_panic() {
        bell();
    }

    #[test]
    fn on_path_finds_sh() {
        assert!(on_path("sh"));
    }

    #[test]
    fn on_path_rejects_missing() {
        assert!(!on_path("definitely-not-a-real-binary-xyz"));
    }

    #[test]
    fn desktop_does_not_panic() {
        // CI may have no notifier; just assert it returns without panicking.
        let _ = desktop("t", "b");
    }

    #[test]
    fn task_done_does_not_panic() {
        let _ = task_done("did a thing", true);
        let _ = task_done("broke a thing", false);
    }

    #[test]
    fn sanitize_strips_newlines() {
        let out = sanitize("line1\nline2\tend", 200);
        assert!(!out.contains('\n'));
        assert!(!out.contains('\t'));
        assert_eq!(out, "line1 line2 end");
    }

    #[test]
    fn sanitize_clips_length() {
        let out = sanitize(&"x".repeat(500), 200);
        assert_eq!(out.chars().count(), 200);
    }
}
