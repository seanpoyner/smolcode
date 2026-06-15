//! `run_tests` tool: detect the project's test command (via `crate::project`),
//! run it through `bash -lc` under the workspace root, and return a concise
//! pass/fail summary — headline, extracted result lines, prioritized failure
//! lines, and a tail of combined stdout+stderr. Pure std; never panics.

use std::path::Path;
use std::process::Command;

use crate::project;

/// Run the project's test command under `root` and return a readable summary:
/// the command, exit status, a pass/fail headline, and a tail of the output
/// (errors/failures prioritized). If `extra` is non-empty it is appended to
/// the command (e.g. a specific test name / path filter).
/// Returns "(no test command detected for this project)" when none is known.
pub fn run_tests(root: &Path, extra: &str) -> String {
    let test_cmd = match project::detect(root).test {
        Some(c) => c,
        None => return "(no test command detected for this project)".to_string(),
    };

    let extra = extra.trim();
    let cmdline = if extra.is_empty() {
        test_cmd.trim().to_string()
    } else {
        format!("{} {}", test_cmd.trim(), extra)
    };

    // Time-box the test run (default 300s; tests can be slow) so a watch-mode
    // command can't hang the agent. Use `timeout` when available.
    let secs: u64 = std::env::var("SMOLCODE_TEST_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    let has_timeout = std::env::var("PATH").unwrap_or_default().split(':').any(|d| std::path::Path::new(d).join("timeout").is_file());
    let mut cmd = if has_timeout {
        let mut c = Command::new("timeout");
        c.arg("-k").arg("5").arg(secs.to_string()).arg("bash").arg("-lc").arg(&cmdline);
        c
    } else {
        let mut c = Command::new("bash");
        c.arg("-lc").arg(&cmdline);
        c
    };
    let out = match cmd.current_dir(root).output() {
        Ok(o) => o,
        Err(e) => return format!("(could not run tests: {e})"),
    };

    let code = out.status.code().unwrap_or(-1);
    if code == 124 {
        return format!("✗ tests timed out after {secs}s and were killed: `{}`", crate::agent::clip(&cmdline, 80));
    }
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&out.stdout));
    if !out.stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    let combined = crate::tools::denoise(&combined);

    summarize(&cmdline, code, &combined)
}

/// Build the human-readable report from a command line, exit code, and the
/// combined output text. Factored out so it can be unit-tested without ever
/// compiling/running a real project.
fn summarize(cmdline: &str, code: i32, out: &str) -> String {
    let passed = code == 0;
    let headline = if passed {
        "✓ tests passed".to_string()
    } else {
        format!("✗ tests failed (exit {code})")
    };

    let lines: Vec<&str> = out.lines().collect();

    // Best-effort summary lines across common frameworks.
    let mut summary: Vec<String> = Vec::new();
    for raw in &lines {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let low = line.to_ascii_lowercase();
        let is_summary = low.contains("test result:")            // rust
            || low.starts_with("tests:")                          // jest/npm
            || (line.starts_with("ok") && low.contains("test"))   // go ok lines
            || line.starts_with("FAIL")                           // go FAIL lines
            || (line.contains('=') && (low.contains("passed") || low.contains("failed") || low.contains("error"))); // pytest
        if is_summary {
            summary.push(line.to_string());
        }
    }
    summary.dedup();
    if summary.len() > 8 {
        summary.truncate(8);
    }

    // Prioritized failure lines (only meaningful when failing).
    let mut failures: Vec<String> = Vec::new();
    if !passed {
        for raw in &lines {
            let line = raw.trim_end();
            let low = line.to_ascii_lowercase();
            if low.contains("fail") || low.contains("error") || low.contains("panic") {
                let t = line.trim();
                if !t.is_empty() {
                    failures.push(t.to_string());
                }
            }
            if failures.len() >= 15 {
                break;
            }
        }
    }

    let (tail, truncated) = tail_of(out, 40, 3000);

    let mut s = String::new();
    s.push_str(&format!("$ {cmdline}\n"));
    s.push_str(&headline);
    s.push('\n');

    if !summary.is_empty() {
        s.push_str("summary: ");
        s.push_str(&summary.join(" | "));
        s.push('\n');
    }

    if !passed && !failures.is_empty() {
        s.push_str("failures:\n");
        for f in &failures {
            s.push_str("  ");
            s.push_str(f);
            s.push('\n');
        }
    }

    s.push_str("output (tail):\n");
    if truncated {
        s.push_str("... (truncated)\n");
    }
    s.push_str(&tail);
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Return the last `max_lines` lines of `text`, further capped to `max_chars`
/// (counting from the end), plus whether anything was dropped.
fn tail_of(text: &str, max_lines: usize, max_chars: usize) -> (String, bool) {
    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let start = total.saturating_sub(max_lines);
    let mut truncated = start > 0;

    let mut tail = all[start..].join("\n");

    if tail.len() > max_chars {
        // Keep the trailing `max_chars` bytes, snapped to a char boundary.
        let mut cut = tail.len() - max_chars;
        while cut < tail.len() && !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail = tail[cut..].to_string();
        truncated = true;
    }

    (tail, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fresh_tmp() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("smolcode-testrun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn no_command_for_unrecognized_project() {
        let dir = fresh_tmp();
        let got = run_tests(&dir, "");
        assert_eq!(got, "(no test command detected for this project)");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn passing_rust_output_is_summarized() {
        let out = "running 3 tests\n\
                   test it_works ... ok\n\
                   test result: ok. 3 passed; 0 failed; 0 ignored\n";
        let s = summarize("cargo test", 0, out);
        assert!(s.contains("✓ tests passed"), "headline: {s}");
        assert!(s.contains("summary: "), "summary present: {s}");
        assert!(s.contains("test result: ok. 3 passed; 0 failed"), "result line: {s}");
        // No failures section on success.
        assert!(!s.contains("failures:"), "no failures on pass: {s}");
    }

    #[test]
    fn failing_rust_output_lists_panic() {
        let out = "running 1 test\n\
                   test boom ... FAILED\n\
                   thread 'boom' panicked at src/lib.rs:4:5\n\
                   test result: FAILED. 0 passed; 1 failed; 0 ignored\n";
        let s = summarize("cargo test", 101, out);
        assert!(s.contains("✗ tests failed (exit 101)"), "headline: {s}");
        assert!(s.contains("failures:"), "failures section: {s}");
        assert!(s.contains("panicked at src/lib.rs"), "panic line under failures: {s}");
        assert!(s.contains("test result: FAILED"), "summary line: {s}");
    }
}
