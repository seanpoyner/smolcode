//! In-repo evaluation harness for the smolcode coding agent.
//!
//! Two kinds of cases share one runner:
//!   * the inline `smoke_suite()` (fast sanity cases, checked by Rust fns), and
//!   * data-driven cases loaded from a directory (`load_dir`), where each case
//!     is `meta.json` + `task.md` + `seed/` + `check.sh` (exit 0 = pass).
//!
//! This module is the scaffold + checkers; it does not call the LLM. The caller
//! (main::run_eval) spins the agent against a temp workspace per case, then
//! invokes the checker. Pure std + serde_json, offline-testable, never panics.

use std::path::Path;
use std::process::Command;

/// How a case decides pass/fail.
pub enum Check {
    /// Inline Rust checker (smoke suite).
    Fn(fn(&Path) -> Result<(), String>),
    /// A bash script run in the workspace; exit 0 = pass (data-driven dev set).
    Script(String),
}

/// One evaluation case: a prompt given to the agent in a fresh workspace,
/// optional seed files, and a checker over the resulting workspace.
pub struct Case {
    pub name: String,
    pub lang: String,
    pub prompt: String,
    pub seed: Vec<(String, String)>, // (relpath, contents)
    pub check: Check,
}

/// Outcome of running one case.
pub struct CaseResult {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

fn case(name: &str, prompt: &str, seed: &[(&str, &str)], check: fn(&Path) -> Result<(), String>) -> Case {
    Case {
        name: name.to_string(),
        lang: "python".to_string(),
        prompt: prompt.to_string(),
        seed: seed.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
        check: Check::Fn(check),
    }
}

/// Fast inline sanity cases (the regression guard).
pub fn smoke_suite() -> Vec<Case> {
    vec![
        case(
            "create-hello",
            "Create hello.py that prints exactly 'hello world' and nothing else.",
            &[],
            |ws| {
                check_file_exists(ws, "hello.py")?;
                check_cmd_ok(ws, "python3 hello.py | grep -qx 'hello world'")
            },
        ),
        case(
            "fix-syntax",
            "Fix the syntax error in broken.py so it imports cleanly.",
            &[("broken.py", "def f(:\n  return 1\n")],
            |ws| check_cmd_ok(ws, "python3 -m py_compile broken.py"),
        ),
        case(
            "add-function",
            "Add a function mul(a,b) that returns a*b to math_utils.py, keep the existing code.",
            &[("math_utils.py", "def add(a, b):\n    return a + b\n")],
            |ws| {
                check_file_contains(ws, "math_utils.py", "def mul")?;
                check_cmd_ok(ws, "python3 -c 'import math_utils; assert math_utils.mul(2,3)==6'")
            },
        ),
        case(
            "json-config",
            "Create config.json containing a JSON object with key \"name\" set to \"smolcode\".",
            &[],
            |ws| {
                check_file_exists(ws, "config.json")?;
                check_file_contains(ws, "config.json", "\"name\"")?;
                check_file_contains(ws, "config.json", "smolcode")
            },
        ),
    ]
}

/// Load data-driven cases from `dir`: every immediate subdirectory containing
/// `task.md` + `check.sh` is a case. `meta.json` (optional) supplies name/lang.
/// `seed/` (optional) is copied into the workspace. Returns cases sorted by name.
pub fn load_dir(dir: &Path) -> Result<Vec<Case>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("read {}: {}", dir.display(), e))?;
    let mut cases = Vec::new();
    for entry in rd.flatten() {
        let cdir = entry.path();
        if !cdir.is_dir() {
            continue;
        }
        let task = cdir.join("task.md");
        let check = cdir.join("check.sh");
        if !task.is_file() || !check.is_file() {
            continue; // not a case dir
        }
        let prompt = std::fs::read_to_string(&task)
            .map_err(|e| format!("read {}: {}", task.display(), e))?
            .trim()
            .to_string();
        let check_script = std::fs::read_to_string(&check)
            .map_err(|e| format!("read {}: {}", check.display(), e))?;
        let dir_name = cdir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        // meta.json (optional) for name + lang
        let (mut name, mut lang) = (dir_name.clone(), "unknown".to_string());
        if let Ok(meta) = std::fs::read_to_string(cdir.join("meta.json")) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&meta) {
                if let Some(n) = v.get("name").and_then(|x| x.as_str()) {
                    name = n.to_string();
                }
                if let Some(l) = v.get("lang").and_then(|x| x.as_str()) {
                    lang = l.to_string();
                }
            }
        }
        let seed = read_seed(&cdir.join("seed"));
        cases.push(Case { name, lang, prompt, seed, check: Check::Script(check_script) });
    }
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(cases)
}

/// Recursively collect (relpath, contents) under `seed_dir` (empty if absent).
fn read_seed(seed_dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let rd = match std::fs::read_dir(dir) {
            Ok(r) => r,
            Err(_) => return,
        };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(ft) if ft.is_dir() => walk(base, &p, out),
                Ok(ft) if ft.is_file() => {
                    if let (Ok(rel), Ok(body)) = (p.strip_prefix(base), std::fs::read_to_string(&p)) {
                        out.push((rel.to_string_lossy().to_string(), body));
                    }
                }
                _ => {}
            }
        }
    }
    walk(seed_dir, seed_dir, &mut out);
    out.sort();
    out
}

/// Materialize a case's seed files into `workspace` (creating parent dirs).
pub fn prepare(case: &Case, workspace: &Path) -> Result<(), String> {
    for (rel, contents) in &case.seed {
        let path = workspace.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create_dir_all {}: {}", parent.display(), e))?;
        }
        std::fs::write(&path, contents).map_err(|e| format!("write {}: {}", path.display(), e))?;
    }
    Ok(())
}

/// Run a case's checker against the (agent-modified) workspace.
pub fn judge(case: &Case, workspace: &Path) -> CaseResult {
    let result = match &case.check {
        Check::Fn(f) => f(workspace),
        Check::Script(s) => check_cmd_ok(workspace, s),
    };
    match result {
        Ok(()) => CaseResult { name: case.name.clone(), passed: true, detail: String::new() },
        Err(reason) => CaseResult { name: case.name.clone(), passed: false, detail: reason },
    }
}

/// Format results as a scorecard string.
pub fn scorecard(results: &[CaseResult]) -> String {
    let passed = results.iter().filter(|r| r.passed).count();
    let mut out = format!("eval: {}/{} passed", passed, results.len());
    for r in results {
        if r.passed {
            out.push_str(&format!("\n  \u{2713} {}", r.name));
        } else {
            out.push_str(&format!("\n  \u{2717} {}: {}", r.name, r.detail));
        }
    }
    out
}

/// Checker: the named file exists under the workspace.
pub fn check_file_exists(ws: &Path, rel: &str) -> Result<(), String> {
    if ws.join(rel).is_file() {
        Ok(())
    } else {
        Err(format!("missing file: {}", rel))
    }
}

/// Checker: the named file exists and contains `needle`.
pub fn check_file_contains(ws: &Path, rel: &str, needle: &str) -> Result<(), String> {
    let body = std::fs::read_to_string(ws.join(rel)).map_err(|e| format!("read {}: {}", rel, e))?;
    if body.contains(needle) {
        Ok(())
    } else {
        Err(format!("{} does not contain {:?}", rel, needle))
    }
}

/// Checker: running `bash -lc "<cmd>"` in the workspace exits 0.
pub fn check_cmd_ok(ws: &Path, cmd: &str) -> Result<(), String> {
    match Command::new("bash").arg("-lc").arg(cmd).current_dir(ws).output() {
        Ok(out) => {
            if out.status.success() {
                Ok(())
            } else {
                let code = out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".to_string());
                // strip benign env noise (nvm/NPM_CONFIG_PREFIX) so the real
                // failure (e.g. an AssertionError) isn't hidden by it.
                let stderr = crate::tools::denoise(&String::from_utf8_lossy(&out.stderr));
                let stdout = String::from_utf8_lossy(&out.stdout);
                let combined = format!("{} {}", stderr.trim(), stdout.trim());
                let snippet: String = combined.trim().chars().take(220).collect();
                Err(format!("cmd exit {}: {}", code, snippet))
            }
        }
        Err(e) => Err(format!("spawn failed: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_ws(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("smolcode-eval-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp ws");
        dir
    }
    fn cleanup(ws: &Path) {
        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn smoke_suite_has_four_unique_cases() {
        let cases = smoke_suite();
        assert_eq!(cases.len(), 4);
        let mut names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 4, "case names must be unique");
    }

    #[test]
    fn prepare_materializes_seed() {
        let ws = temp_ws("prepare");
        let cases = smoke_suite();
        let fix = cases.iter().find(|c| c.name == "fix-syntax").expect("fix-syntax case");
        prepare(fix, &ws).expect("prepare ok");
        let body = std::fs::read_to_string(ws.join("broken.py")).expect("read seed");
        assert_eq!(body, "def f(:\n  return 1\n");
        cleanup(&ws);
    }

    #[test]
    fn load_dir_reads_a_case() {
        let root = temp_ws("loaddir");
        let cdir = root.join("py-demo");
        std::fs::create_dir_all(cdir.join("seed/pkg")).unwrap();
        std::fs::write(cdir.join("meta.json"), r#"{"name":"py-demo","lang":"python"}"#).unwrap();
        std::fs::write(cdir.join("task.md"), "Make the test pass.\n").unwrap();
        std::fs::write(cdir.join("check.sh"), "test -f answer.txt\n").unwrap();
        std::fs::write(cdir.join("seed/pkg/mod.py"), "x = 1\n").unwrap();
        let cases = load_dir(&root).expect("load ok");
        assert_eq!(cases.len(), 1);
        let c = &cases[0];
        assert_eq!(c.name, "py-demo");
        assert_eq!(c.lang, "python");
        assert_eq!(c.prompt, "Make the test pass.");
        assert!(c.seed.iter().any(|(p, b)| p == "pkg/mod.py" && b == "x = 1\n"));
        assert!(matches!(c.check, Check::Script(_)));
        // a dir without task.md/check.sh is ignored
        std::fs::create_dir_all(root.join("not-a-case")).unwrap();
        assert_eq!(load_dir(&root).unwrap().len(), 1);
        cleanup(&root);
    }

    #[test]
    fn checkers_and_judge() {
        let ws = temp_ws("judge");
        std::fs::write(ws.join("there.txt"), "hello smolcode").unwrap();
        assert!(check_file_exists(&ws, "there.txt").is_ok());
        assert!(check_file_exists(&ws, "nope.txt").is_err());
        assert!(check_file_contains(&ws, "there.txt", "smolcode").is_ok());
        assert!(check_file_contains(&ws, "there.txt", "absent").is_err());
        assert!(check_cmd_ok(&ws, "true").is_ok());
        assert!(check_cmd_ok(&ws, "false").is_err());

        let pass = Case { name: "p".into(), lang: "x".into(), prompt: String::new(), seed: vec![], check: Check::Fn(|_| Ok(())) };
        let fail = Case { name: "f".into(), lang: "x".into(), prompt: String::new(), seed: vec![], check: Check::Script("false".into()) };
        let r1 = judge(&pass, &ws);
        let r2 = judge(&fail, &ws);
        assert!(r1.passed && !r2.passed);
        let card = scorecard(&[r1, r2]);
        assert!(card.starts_with("eval: 1/2 passed"), "got: {}", card);
        cleanup(&ws);
    }
}
