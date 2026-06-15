//! File / shell / run tools, workspace-confined, with approval gating.

use anyhow::{anyhow, Context, Result};
use liteforge::{FunctionDefinition, ToolDefinition, ToolParameters};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Default wall-clock budget for a single shell/python invocation (seconds).
/// Override with `SMOLCODE_SHELL_TIMEOUT`.
fn shell_timeout_secs() -> u64 {
    std::env::var("SMOLCODE_SHELL_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120)
}

/// Whether the `timeout` coreutil is available to time-box commands.
fn has_timeout() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .any(|d| Path::new(d).join("timeout").is_file())
    })
}

/// Build a `Command` that runs `program` (with `args`) under a `timeout` wrapper
/// when available, so a hung/never-exiting process can't block the agent forever.
fn timed_command(root: &Path, secs: u64, program: &str, args: &[&str]) -> Command {
    let mut cmd = if has_timeout() {
        let mut c = Command::new("timeout");
        c.arg("-k").arg("5").arg(secs.to_string()).arg(program).args(args);
        c
    } else {
        let mut c = Command::new(program);
        c.args(args);
        c
    };
    cmd.current_dir(root);
    // nvm aborts (and drops node from PATH) when NPM_CONFIG_PREFIX is set, which
    // breaks every npm/npx/yarn command. Scrub it so node tooling works; harmless
    // for non-node commands. (We also use a non-login shell — see run_shell — so
    // nvm isn't re-sourced at all.)
    cmd.env_remove("NPM_CONFIG_PREFIX");
    cmd
}

/// Run a shell command synchronously in `root` and return formatted output.
///
/// This is the human-driven `!cmd` passthrough (TUI), so unlike the agent's
/// `run_shell` it never backgrounds — it always waits and returns the result,
/// time-boxed like every other shell call. Same denoise/format as agent output.
pub fn run_shell_sync(root: &Path, command: &str) -> String {
    let secs = shell_timeout_secs();
    match timed_command(root, secs, "bash", &["-c", command]).output() {
        Ok(out) => with_timeout_note(out, secs, command),
        Err(e) => format!("failed to run command: {e}"),
    }
}

/// Heuristic: commands that start a long-running server/watcher and would block
/// the agent indefinitely. We refuse these with guidance rather than hang.
fn is_long_running(command: &str) -> bool {
    let c = command.to_lowercase();
    const NEEDLES: &[&str] = &[
        "npm start", "npm run start", "npm run dev", "yarn start", "yarn dev",
        "pnpm dev", "pnpm start", "vite", "next dev", "react-scripts start",
        "http-server", "python -m http.server", "rails server", "rails s",
        "flask run", "uvicorn", "gunicorn", "nodemon", "--watch", "watch ",
    ];
    NEEDLES.iter().any(|n| c.contains(n))
}

/// Tools that change the world (need approval unless --yolo).
pub fn is_mutating(name: &str) -> bool {
    matches!(name, "write_file" | "str_replace" | "apply_patch" | "multi_edit" | "run_shell" | "run_python" | "git_commit")
}

#[derive(Clone)]
pub struct Tools {
    pub root: PathBuf,
    pub yolo: bool,
    pub undo: Option<std::sync::Arc<std::sync::Mutex<crate::undo::UndoStack>>>,
    extension: Option<std::sync::Arc<dyn ToolExtension>>,
}

/// Optional hook for host integrations (e.g. Python `check_app` in smolbuilder).
pub trait ToolExtension: Send + Sync {
    fn try_dispatch(&self, name: &str, args: &str) -> Option<anyhow::Result<String>>;
}

impl Tools {
    pub fn new(root: PathBuf, yolo: bool) -> Self {
        Self {
            root,
            yolo,
            undo: None,
            extension: None,
        }
    }

    pub fn with_extension(mut self, ext: std::sync::Arc<dyn ToolExtension>) -> Self {
        self.extension = Some(ext);
        self
    }

    pub fn with_undo(mut self, u: std::sync::Arc<std::sync::Mutex<crate::undo::UndoStack>>) -> Self {
        self.undo = Some(u);
        self
    }

    /// All files in the workspace (relative paths), for UI bindings.
    pub fn workspace_files(&self) -> Vec<String> {
        let mut out = Vec::new();
        walk_files(&self.root, &self.root, &mut out);
        out.sort();
        out
    }

    /// Read a workspace file; returns None if missing or unreadable.
    pub fn read_workspace_file(&self, path: &str) -> Option<String> {
        self.read_file(path).ok()
    }

    fn record_undo(&self, path: &str) {
        if let Some(u) = &self.undo {
            if let Ok(mut s) = u.lock() {
                s.record(&self.root, path);
            }
        }
    }

    /// Snapshot every file a unified diff targets (its `+++ b/<path>` lines),
    /// so `apply_patch` is undoable like a regular edit.
    fn record_undo_patch(&self, diff: &str) {
        for line in diff.lines() {
            if let Some(rest) = line.strip_prefix("+++ ") {
                let p = rest
                    .split('\t')
                    .next()
                    .unwrap_or(rest)
                    .trim()
                    .trim_start_matches("b/")
                    .trim_start_matches("a/");
                if !p.is_empty() && p != "/dev/null" {
                    self.record_undo(p);
                }
            }
        }
    }

    fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let canon_root = self.root.canonicalize().unwrap_or_else(|_| self.root.clone());
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
            return Err(anyhow!("path escapes workspace: {rel}"));
        }
        Ok(abs)
    }

    pub fn dispatch(&self, name: &str, args: &str) -> Result<String> {
        if let Some(ext) = &self.extension {
            if let Some(r) = ext.try_dispatch(name, args) {
                return r;
            }
        }
        let v: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::json!({}));
        let g = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        match name {
            "read_file" => self.read_file(&g("path")),
            "write_file" => self.write_file(&g("path"), &g("content")),
            "str_replace" => self.str_replace(&g("path"), &g("old"), &g("new")),
            "list_dir" => self.list_dir(&g("path")),
            "run_shell" => {
                let bg = v.get("background").and_then(|x| x.as_bool()).unwrap_or(false);
                self.run_shell(&g("command"), bg)
            }
            "run_python" => self.run_python(&g("path")),
            "bash_output" => Ok(crate::bgproc::output(&g("id"))),
            "stop_shell" => Ok(crate::bgproc::stop(&g("id"))),
            "search" => {
                let ci = v.get("case_insensitive").and_then(|x| x.as_bool()).unwrap_or(true);
                Ok(crate::search::search(&self.root, &g("pattern"), ci))
            }
            "apply_patch" => {
                self.record_undo_patch(&g("patch"));
                crate::patch::apply_patch(&self.root, &g("patch"))
            }
            "git_status" => Ok(crate::git::status(&self.root)),
            "git_diff" => Ok(crate::git::diff(&self.root, &g("path"))),
            "git_log" => {
                let n = v.get("count").and_then(|x| x.as_u64()).unwrap_or(15) as usize;
                Ok(crate::git::log(&self.root, n))
            }
            "git_commit" => Ok(crate::git::commit(&self.root, &g("message"))),
            "outline" => Ok(crate::symbols::outline(&self.root, &g("path"))),
            "find_symbol" => Ok(crate::symbols::find(&self.root, &g("name"))),
            "find_context" => {
                let k = v.get("k").and_then(|x| x.as_u64()).unwrap_or(5) as usize;
                Ok(crate::rag::find_context(&self.root, &g("query"), k))
            }
            "tree" => {
                let d = v.get("depth").and_then(|x| x.as_u64()).unwrap_or(3) as usize;
                Ok(crate::tree::tree(&self.root, d))
            }
            "web_fetch" => Ok(crate::web::fetch(&g("url"))),
            "project_info" => Ok(crate::project::summary(&crate::project::detect(&self.root))),
            "run_tests" => Ok(crate::testrun::run_tests(&self.root, &g("filter"))),
            "format_file" => Ok(crate::fmt::format_file(&self.root, &g("path"))),
            "multi_edit" => {
                let (path, edits) = crate::multi_edit::parse_args(args)?;
                self.record_undo(&path);
                let mut msg = crate::multi_edit::apply(&self.root, &path, &edits)?;
                msg.push_str(&self.diagnostics_suffix(&path));
                Ok(msg)
            }
            "repo_map" => Ok(crate::context::repo_map(&self.root)),
            "use_skill" => Ok(self.use_skill(&g("name"))),
            other => Ok(format!(
                "'{other}' is not a tool (do not invent tool names). Valid tools: {}.",
                tool_names().join(", ")
            )),
        }
    }

    fn read_file(&self, path: &str) -> Result<String> {
        let p = self.resolve(path)?;
        let s = std::fs::read_to_string(&p).with_context(|| format!("read {path}"))?;
        Ok(clip(&s, 12000))
    }

    /// Load a skill's full instructions by name (read-only). Returns guidance the
    /// model then follows; mentions the skill dir for any bundled files.
    fn use_skill(&self, name: &str) -> String {
        match crate::skills::find(&self.root, name) {
            Some(s) => format!(
                "Skill '{}' (files in {}):\n\n{}",
                s.name,
                s.dir.display(),
                s.body.trim()
            ),
            None => {
                let avail = crate::skills::load(&self.root)
                    .into_iter()
                    .map(|s| s.name)
                    .collect::<Vec<_>>()
                    .join(", ");
                if avail.is_empty() {
                    format!("no skill named '{name}' (no skills are configured)")
                } else {
                    format!("no skill named '{name}' (available: {avail})")
                }
            }
        }
    }

    fn write_file(&self, path: &str, content: &str) -> Result<String> {
        self.record_undo(path);
        let p = self.resolve(path)?;
        let old = std::fs::read_to_string(&p).unwrap_or_default();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&p, content).with_context(|| format!("write {path}"))?;
        let mut msg = format!(
            "wrote {} bytes to {path}\n{}",
            content.len(),
            unified_diff(&old, content)
        );
        msg.push_str(&self.diagnostics_suffix(path));
        Ok(msg)
    }

    /// Append LSP diagnostics if a language server is installed (else "").
    fn diagnostics_suffix(&self, path: &str) -> String {
        if !crate::lsp::available_for(path) {
            return String::new();
        }
        let diags = crate::lsp::diagnostics(&self.root, path);
        if diags.is_empty() {
            "\n(no diagnostics)".into()
        } else {
            format!("\n\ndiagnostics:\n{}", diags.join("\n"))
        }
    }

    fn str_replace(&self, path: &str, old: &str, new: &str) -> Result<String> {
        let p = self.resolve(path)?;
        let s = std::fs::read_to_string(&p).with_context(|| format!("read {path}"))?;
        let n = s.matches(old).count();
        if n == 0 {
            return Ok(format!("no match for the given text in {path}"));
        }
        self.record_undo(path);
        let new_content = s.replace(old, new);
        std::fs::write(&p, &new_content)?;
        let mut msg = format!(
            "replaced {n} occurrence(s) in {path}\n{}",
            unified_diff(&s, &new_content)
        );
        msg.push_str(&self.diagnostics_suffix(path));
        Ok(msg)
    }

    fn list_dir(&self, path: &str) -> Result<String> {
        let rel = if path.is_empty() { "." } else { path };
        let p = self.resolve(rel)?;
        let mut out = String::new();
        for e in std::fs::read_dir(&p).with_context(|| format!("list {rel}"))? {
            let e = e?;
            let kind = if e.file_type()?.is_dir() { "dir " } else { "file" };
            out += &format!("{kind}  {}\n", e.file_name().to_string_lossy());
        }
        Ok(if out.is_empty() { "(empty)".into() } else { out })
    }

    fn run_shell(&self, command: &str, background: bool) -> Result<String> {
        // Long-running servers/watchers (and anything the model marks
        // background) run detached so they don't block the agent.
        if background || is_long_running(command) {
            return Ok(crate::bgproc::start(&self.root, command));
        }
        let secs = shell_timeout_secs();
        let out = timed_command(&self.root, secs, "bash", &["-c", command]).output()?;
        Ok(with_timeout_note(out, secs, command))
    }

    fn run_python(&self, path: &str) -> Result<String> {
        let p = self.resolve(path)?;
        let secs = shell_timeout_secs();
        let ps = p.to_string_lossy().to_string();
        // Use the project's virtualenv interpreter if one exists, so installed
        // packages are found without the model having to activate it each call.
        let py = self.python_bin();
        let out = timed_command(&self.root, secs, &py, &[&ps]).output()?;
        Ok(with_timeout_note(out, secs, path))
    }

    /// `<root>/.venv/bin/python` (or `venv/bin/python`) if present, else `python3`.
    fn python_bin(&self) -> String {
        for venv in [".venv/bin/python", "venv/bin/python", ".venv/bin/python3"] {
            let cand = self.root.join(venv);
            if cand.is_file() {
                return cand.to_string_lossy().to_string();
            }
        }
        "python3".to_string()
    }
}

/// Format process output, turning a `timeout` kill (exit 124) into a clear note.
fn with_timeout_note(out: std::process::Output, secs: u64, what: &str) -> String {
    if out.status.code() == Some(124) {
        return format!(
            "command timed out after {secs}s and was killed: `{}`. If this is expected to run \
             longer, raise SMOLCODE_SHELL_TIMEOUT; if it is a server/watcher, run it yourself.",
            clip(what, 80)
        );
    }
    fmt_output(out)
}

/// Compact unified-ish diff (changed lines only, capped) for the UI + model.
fn unified_diff(old: &str, new: &str) -> String {
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
        if count >= 50 {
            out.push_str("…(diff truncated)\n");
            break;
        }
    }
    out.trim_end().to_string()
}

fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // clip on a char boundary
        let mut end = max;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}\n...[truncated {} bytes]", &s[..end], s.len() - end)
    }
}

fn walk_files(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            walk_files(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().to_string());
        }
    }
}

fn fmt_output(out: std::process::Output) -> String {
    let code = out.status.code().unwrap_or(-1);
    let so = String::from_utf8_lossy(&out.stdout);
    let se = denoise(&String::from_utf8_lossy(&out.stderr));
    let se = se.trim();
    // Make the result unambiguous: small models otherwise misread a non-empty
    // stderr as failure even when the command succeeded.
    let status = if code == 0 { "exit=0 (success)".to_string() } else { format!("exit={code} (FAILED)") };
    let stderr_label = if code == 0 { "stderr (warnings only, non-fatal):" } else { "stderr:" };
    if se.is_empty() {
        format!("{status}\nstdout:\n{}", clip(&so, 8000))
    } else {
        format!("{status}\nstdout:\n{}\n{stderr_label}\n{}", clip(&so, 8000), clip(se, 4000))
    }
}

/// Strip known-benign environment noise (e.g. the multi-line nvm/
/// NPM_CONFIG_PREFIX warning that login shells print on every command) so the
/// model doesn't misread harmless warnings as a command failure.
pub fn denoise(s: &str) -> String {
    const NOISE: &[&str] = &[
        "nvm is not compatible with",
        "unset NPM_CONFIG_PREFIX",
        "nvm use --delete-prefix",
        "has a `globalconfig`",
        "incompatible with nvm",
        "Your user's .npmrc file",
        "Your user’s .npmrc file",
    ];
    let kept: Vec<&str> = s
        .lines()
        .filter(|l| !NOISE.iter().any(|n| l.contains(n)))
        .collect();
    kept.join("\n").trim().to_string()
}

/// Plan mode: read-only tools only.
pub fn read_only_defs() -> Vec<ToolDefinition> {
    tool_defs()
        .into_iter()
        .filter(|d| matches!(d.function.name.as_str(),
            "read_file" | "list_dir" | "search" | "repo_map" | "git_status" | "git_diff" | "git_log"
            | "outline" | "find_symbol" | "find_context" | "tree" | "project_info"
            | "bash_output" | "use_skill"))
        .collect()
}

/// The names of all built-in tools (for the /config view).
pub fn tool_names() -> Vec<&'static str> {
    vec![
        "read_file", "write_file", "str_replace", "apply_patch", "multi_edit",
        "list_dir", "search", "outline", "find_symbol", "find_context", "tree",
        "project_info", "run_shell", "run_python", "run_tests", "format_file",
        "git_status", "git_diff", "git_log", "git_commit", "web_fetch", "repo_map", "task",
        "task_batch", "use_skill",
    ]
}

pub fn tool_defs() -> Vec<ToolDefinition> {
    use serde_json::json;
    fn def(name: &str, desc: &str, props: serde_json::Value, req: &[&str]) -> ToolDefinition {
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: name.into(),
                description: Some(desc.into()),
                parameters: Some(ToolParameters {
                    schema_type: "object".into(),
                    properties: props.as_object().cloned().unwrap_or_default(),
                    required: Some(req.iter().map(|s| s.to_string()).collect()),
                }),
            },
        }
    }
    vec![
        def("read_file", "Read a file from the workspace and return its contents.",
            json!({"path": {"type": "string", "description": "relative path"}}), &["path"]),
        def("write_file", "Create or overwrite a file with the given text content.",
            json!({"path": {"type": "string"}, "content": {"type": "string"}}), &["path", "content"]),
        def("str_replace", "Replace an exact substring in a file.",
            json!({"path": {"type": "string"}, "old": {"type": "string"}, "new": {"type": "string"}}),
            &["path", "old", "new"]),
        def("list_dir", "List files in a workspace directory (default: root).",
            json!({"path": {"type": "string"}}), &[]),
        def("run_shell", "Run a bash command in the workspace; returns stdout/stderr/exit. Long-running servers/watchers (npm run dev, vite, ...) are auto-detected and run in the background; set background=true to force it. Background jobs return a job id you can read with bash_output or stop with stop_shell.",
            json!({"command": {"type": "string"}, "background": {"type": "boolean", "description": "run detached (for dev servers/watchers)"}}), &["command"]),
        def("run_python", "Run a Python file in the workspace; returns stdout/stderr/exit.",
            json!({"path": {"type": "string"}}), &["path"]),
        def("bash_output", "Read the latest output of a background job started by run_shell (e.g. a dev server).",
            json!({"id": {"type": "string", "description": "the background job id, e.g. bg1"}}), &["id"]),
        def("stop_shell", "Stop a background job started by run_shell (kills the dev server/watcher).",
            json!({"id": {"type": "string"}}), &["id"]),
        def("search", "Search the workspace for a text pattern across files; returns matching 'path:line: text'. Use it to find where something is defined or used without reading whole files.",
            json!({
                "pattern": {"type": "string", "description": "substring to search for"},
                "case_insensitive": {"type": "boolean", "description": "default true"}
            }), &["pattern"]),
        def("apply_patch", "Apply a standard unified diff (may span multiple files) to the workspace. Prefer this for multi-file or multi-hunk edits; use write_file/str_replace for single small edits.",
            json!({"patch": {"type": "string", "description": "unified diff with ---/+++ headers and @@ hunks"}}), &["patch"]),
        def("git_status", "Show git working-tree status (branch + changed/untracked files).",
            json!({}), &[]),
        def("git_diff", "Show the git diff of current changes; optional path narrows it.",
            json!({"path": {"type": "string", "description": "optional file/dir to limit the diff"}}), &[]),
        def("git_log", "Show recent commits (oneline).",
            json!({"count": {"type": "integer", "description": "how many commits (default 15)"}}), &[]),
        def("git_commit", "Stage all changes and commit with a message. Use after the user asks to commit.",
            json!({"message": {"type": "string"}}), &["message"]),
        def("outline", "List the symbols (functions/classes/etc.) defined in a single file. Use it before reading a file in full.",
            json!({"path": {"type": "string", "description": "relative file path"}}), &["path"]),
        def("find_symbol", "Find where a symbol (function/class/struct name) is defined across the repo.",
            json!({"name": {"type": "string"}}), &["name"]),
        def("find_context", "Retrieve the most relevant code chunks for a natural-language query (semantic-ish search). Use it to gather context without reading whole files.",
            json!({"query": {"type": "string"}, "k": {"type": "integer", "description": "how many chunks (default 5)"}}), &["query"]),
        def("tree", "Show the project directory tree (structure overview).",
            json!({"depth": {"type": "integer", "description": "max depth (default 3)"}}), &[]),
        def("web_fetch", "Fetch an http(s) URL and return its readable text (HTML stripped). Use it to read documentation.",
            json!({"url": {"type": "string"}}), &["url"]),
        def("project_info", "Detect the project type and its build/test/lint/format commands.",
            json!({}), &[]),
        def("run_tests", "Run the project's test suite and return a pass/fail summary. Optional filter narrows to a test name/path.",
            json!({"filter": {"type": "string", "description": "optional test name or path filter"}}), &[]),
        def("format_file", "Format a file in place with the right formatter (rustfmt/black/prettier/gofmt).",
            json!({"path": {"type": "string"}}), &["path"]),
        def("multi_edit", "Apply several find/replace edits to ONE file atomically (all-or-nothing). Prefer this over repeated str_replace on the same file.",
            json!({
                "path": {"type": "string"},
                "edits": {"type": "array", "items": {"type": "object", "properties": {"old": {"type": "string"}, "new": {"type": "string"}}}}
            }), &["path", "edits"]),
        def("repo_map", "Get a compact symbol map of the project (files and their functions/classes). Use it to orient before reading files.",
            json!({}), &[]),
        def("task", "Delegate a focused sub-task to a specialized subagent (explore=investigate code, review=review for bugs, general=multi-step). Returns the subagent's findings.",
            json!({
                "subagent": {"type": "string", "description": "explore | review | general"},
                "prompt": {"type": "string", "description": "the focused task for the subagent"}
            }), &["subagent", "prompt"]),
        def("task_batch", "Run MULTIPLE subagents IN PARALLEL and get all their findings back together. Use when you have several INDEPENDENT focused tasks — e.g. exploring or reviewing several files/areas at once — to save wall-clock time. Each task: {subagent: explore|review|general, prompt}.",
            json!({
                "tasks": {"type": "array", "description": "independent jobs to run concurrently",
                    "items": {"type": "object", "properties": {
                        "subagent": {"type": "string", "description": "explore | review | general"},
                        "prompt": {"type": "string", "description": "the focused task for this subagent"}
                    }, "required": ["subagent", "prompt"]}}
            }), &["tasks"]),
        def("use_skill", "Load a named skill's full instructions (from the Skills list) and then follow them. Use when a task matches an available skill.",
            json!({"name": {"type": "string", "description": "the skill name"}}), &["name"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::run_shell_sync;

    #[test]
    fn run_shell_sync_captures_stdout() {
        let out = run_shell_sync(std::path::Path::new("."), "echo hi");
        assert!(out.contains("hi"), "output was: {out}");
    }

    #[test]
    fn run_shell_sync_runs_in_root() {
        let dir = std::env::temp_dir().join("smolcode_rss_test");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("marker.txt"), "x");
        let out = run_shell_sync(&dir, "ls");
        assert!(out.contains("marker.txt"), "output was: {out}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
