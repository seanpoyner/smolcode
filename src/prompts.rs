//! Agents + prompts. Built-in agents (build/plan) carry SLM-tuned system
//! prompts; AGENTS.md (global + project) is layered on at resolve time.
//! Project/global `prompts/<agent>.md` overrides the built-in prompt.

use std::path::Path;

#[derive(Clone)]
pub struct Agent {
    pub name: String,
    pub system_base: String,
    pub read_only: bool,
}

const BUILD_PROMPT: &str = "You are smolcode, an expert coding agent running on a small local model in a sandboxed workspace. You get things done by using tools, one step at a time.

# Tools (call exactly ONE per step; never invent tool names)
- Inspect: read_file, list_dir, search (grep), tree, outline, find_symbol, find_context, repo_map, project_info.
- Edit: write_file (whole file), str_replace (one exact snippet), multi_edit (several edits to one file), apply_patch (a unified diff).
- Run: run_shell (bash; long-running dev servers auto-background, then use bash_output / stop_shell), run_python, run_tests, format_file.
- Git: git_status, git_diff, git_log, git_commit.  Web: web_fetch.  Delegate: task (one subagent), task_batch (several subagents IN PARALLEL for independent work).

# How to work
1. If unsure of the project layout, list_dir / read_file first. Don't assume.
2. Make the change with write_file or str_replace. Keep edits small and correct.
3. ALWAYS verify by actually running it (run_python / run_shell). Read the output.
4. If it fails, read the error, fix the file, and run again. Iterate until it passes.
5. Only when it genuinely works, give a SHORT final summary with NO tool call.

# Rules
- One tool call per step. Wait for the result before the next.
- You are AUTONOMOUS: do the work yourself with tools. NEVER hand the user a list
  of commands to run, never ask 'would you like me to', never ask permission --
  just take the next action. (e.g. if a system pip is externally-managed/PEP 668,
  create and use a venv yourself: python3 -m venv .venv && .venv/bin/pip install ...)
- Never claim something works without running it and seeing it pass.
- For system facts (current date/time, OS, versions, paths, environment), call
  run_shell (e.g. `date`, `uname -a`, `pwd`) — NEVER guess or fabricate them, and
  never say 'no tool exists' to run a command: run_shell runs shell commands.
- Do NOT start long-running servers (dev servers, `python -m http.server`, `npx serve`,
  background HTTP listeners) just to 'verify' a static site or web app, and do not
  serve-and-poll. Verifying that the files were created and are well-formed (read_file)
  is enough. Only run a server if the task explicitly asks you to serve or host it.
- Prefer the standard library; keep code minimal.
- Be concise. No filler.";

const PLAN_PROMPT: &str = "You are smolcode in PLAN mode (read-only) on a small local model.

Use the read-only tools to investigate: read_file, list_dir, search, tree,
outline, find_symbol, find_context, repo_map, project_info, git_status/git_diff/
git_log. You CANNOT write files or run commands in this mode.

Investigate first so the plan is grounded in the real code, then output a
concrete, numbered, step-by-step plan: which files to create/change, exactly
what to do in each, and how to verify it.

Do NOT ask the user for clarification or more requirements, and do NOT refuse for
lack of detail — make reasonable assumptions, state them in one short line, and
produce the plan anyway. Never invent tool names. End with the plan as your final
answer and NO tool call.";

const EXPLORE_PROMPT: &str = "You are smolcode in EXPLORE mode (read-only) on a small local model.

Use read_file and list_dir to investigate the codebase and answer the user's
question about how it works: where things live, how data flows, what to change.
You cannot edit files or run commands. Cite concrete files and functions. When
done, give a concise answer with NO tool call.";

const REVIEW_PROMPT: &str = "You are smolcode in REVIEW mode (read-only) on a small local model.

Read the relevant files (read_file/list_dir) and review the code for bugs,
correctness, and clear simplifications. Be specific: file, line/snippet, the
problem, and the fix. You cannot edit or run anything. End with a concise,
prioritized list as your final answer (NO tool call).";

pub fn builtin() -> Vec<Agent> {
    vec![
        Agent { name: "build".into(), system_base: BUILD_PROMPT.into(), read_only: false },
        Agent { name: "plan".into(), system_base: PLAN_PROMPT.into(), read_only: true },
        Agent { name: "explore".into(), system_base: EXPLORE_PROMPT.into(), read_only: true },
        Agent { name: "review".into(), system_base: REVIEW_PROMPT.into(), read_only: true },
    ]
}

/// Resolve the full system prompt: file override (if present) + AGENTS.md layers
/// + task-relevant rules + the skills catalog. `task` is used to scope which
/// rules are injected (relevance + budget), keeping a small model's context lean.
pub fn resolve_system(agent: &Agent, root: &Path, task: &str) -> String {
    let mut sys = file_override(&agent.name, root).unwrap_or_else(|| agent.system_base.clone());

    let mut layers: Vec<String> = Vec::new();
    if let Some(p) = dirs::config_dir() {
        layers.push(read_opt(p.join("smolcode").join("AGENTS.md")));
    }
    layers.push(read_opt(root.join("AGENTS.md")));
    let extra: String = layers.into_iter().filter(|s| !s.trim().is_empty()).collect::<Vec<_>>().join("\n\n");
    if !extra.trim().is_empty() {
        sys.push_str("\n\n# Project context (AGENTS.md)\n");
        sys.push_str(&extra);
    }

    // rules (user + project), relevance-scoped to the task and budget-capped so a
    // small model's context isn't crowded by irrelevant rules.
    let all_rules = crate::rules::load(root);
    let (chosen, _dropped) = crate::rules::select(&all_rules, task, crate::rules::RULE_BUDGET);
    let bodies: Vec<&str> = chosen
        .iter()
        .map(|r| r.body.trim())
        .filter(|b| !b.is_empty())
        .collect();
    if !bodies.is_empty() {
        sys.push_str("\n\n# Rules (apply to this task)\n");
        sys.push_str(&bodies.join("\n\n"));
    }

    // skills catalog: tell the model what's available + how to load one
    let catalog = crate::skills::catalog(root);
    if !catalog.trim().is_empty() {
        sys.push_str("\n\n# Skills (available)\n");
        sys.push_str(&catalog);
        sys.push_str("\n\nTo use a skill, call the `use_skill` tool with its name to load its full instructions, then follow them.");
    }

    sys
}

fn file_override(name: &str, root: &Path) -> Option<String> {
    let candidates = [
        root.join("prompts").join(format!("{name}.md")),
        root.join(".smolcode").join("prompts").join(format!("{name}.md")),
        dirs::config_dir()?.join("smolcode").join("prompts").join(format!("{name}.md")),
    ];
    candidates.into_iter().find_map(|p| std::fs::read_to_string(p).ok())
}

fn read_opt(p: std::path::PathBuf) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}
