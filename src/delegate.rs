//! Subagent delegation — a headless sub-run of the agent loop.
//!
//! The main agent can hand a focused task to a named subagent (e.g. "explore",
//! "review", "general"). The subagent runs its OWN bounded agent loop with its
//! own system prompt + tool set and returns a final text summary. There is no
//! event channel, no approvals, and no hooks — it's a self-contained call that
//! reuses the same request/extract/dispatch pattern as `agent::run_agent`.

use crate::extract;
use crate::prompts;
use crate::tools::{self, Tools};
use futures::stream::{self, StreamExt};
use liteforge::{AsyncForgeClient, ChatCompletionRequest, Message};
use std::collections::HashMap;
use std::path::PathBuf;

const MAX_STEPS: usize = 10;
/// Max subagents running at once in a fan-out, so local inference isn't swamped.
const MAX_CONCURRENCY: usize = 4;

/// Run a named subagent on `task` and return its final text answer.
///
/// Looks up the subagent's system prompt + read_only flag from
/// `prompts::builtin()`; if the name is unknown, defaults to a read-only
/// "explore"-style agent. Runs a bounded loop (max ~10 steps): build request
/// (system + user(task) + the subagent's tool defs), call
/// `client.chat_completions`, extract tool calls (`extract::extract`), execute
/// via `Tools::dispatch` (read-only set if the subagent is read_only), feed
/// results back, until the model returns no tool calls (final answer) or the
/// step cap. Never panics; returns an error string on failure.
pub async fn delegate(
    client: &AsyncForgeClient,
    model: &str,
    root: PathBuf,
    subagent: &str,
    task: &str,
) -> String {
    // Resolve the subagent's system prompt + read_only flag, defaulting to an
    // "explore"-style read-only agent when the name is unknown.
    let agents = prompts::builtin();
    let agent = agents
        .iter()
        .find(|a| a.name == subagent)
        .or_else(|| agents.iter().find(|a| a.name == "explore"));
    let (system, read_only) = match agent {
        Some(a) => (a.system_base.clone(), a.read_only),
        None => (
            "You are a read-only exploration subagent. Use read_file and list_dir \
             to investigate the codebase and answer the task. Cite concrete files. \
             When done, give a concise answer with NO tool call."
                .to_string(),
            true,
        ),
    };

    // A subagent must not delegate further — strip task/task_batch so fan-out
    // can't recurse (each subagent is a leaf).
    let defs: Vec<_> = if read_only {
        tools::read_only_defs()
    } else {
        tools::tool_defs()
    }
    .into_iter()
    .filter(|d| d.function.name != "task" && d.function.name != "task_batch")
    .collect();

    // yolo=true: subagents auto-run their own tools; no approvals.
    let tools = Tools::new(root, true);

    let mut messages = vec![Message::system(&system), Message::user(task)];
    let mut seen: HashMap<String, usize> = HashMap::new();

    for _ in 0..MAX_STEPS {
        let mut req = ChatCompletionRequest::new(model.to_string(), messages.clone());
        req.tools = Some(defs.clone());
        req.temperature = Some(0.0);
        req.max_tokens = Some(2048);

        let resp = match client.chat_completions(req).await {
            Ok(r) => r,
            Err(e) => return format!("subagent '{subagent}' error: {e}"),
        };
        let msg = match resp.choices.into_iter().next().map(|c| c.message) {
            Some(m) => m,
            None => return format!("subagent '{subagent}' error: model returned no choices"),
        };

        let (calls, native) = extract::extract(&msg);
        if calls.is_empty() {
            // No tool calls -> this is the final answer.
            return msg.content.unwrap_or_default();
        }

        // Record the assistant turn (with any native tool_calls) so the
        // conversation stays consistent for tool-result messages.
        let mut am = Message::assistant(msg.content.clone().unwrap_or_default());
        am.tool_calls = msg.tool_calls.clone();
        messages.push(am);

        let mut observations = String::new();
        let mut max_repeat = 0usize;
        for call in &calls {
            let sig = format!("{}::{}", call.name, call.args);
            let n = seen.entry(sig).or_insert(0);
            *n += 1;
            max_repeat = max_repeat.max(*n);

            let result = tools
                .dispatch(&call.name, &call.args)
                .unwrap_or_else(|e| format!("error: {e}"));

            if native {
                messages.push(Message::tool(call.id.clone(), result));
            } else {
                observations.push_str(&format!("[{}] -> {}\n", call.name, result));
            }
        }

        if !native {
            messages.push(Message::user(format!(
                "Tool results:\n{observations}\nContinue with the next step, or give your final answer."
            )));
        }

        if max_repeat >= 5 {
            return format!(
                "subagent '{subagent}' stopped: same tool call repeated (model stuck)"
            );
        }
        if max_repeat == 3 {
            messages.push(Message::user(
                "You are repeating the same tool call. Stop repeating it: try a different \
                 approach, or if the task is done, reply with a short final answer and NO tool call.",
            ));
        }
    }

    format!("subagent '{subagent}' stopped after {MAX_STEPS} steps without finishing")
}

/// Run several subagents IN PARALLEL and return one aggregated, labeled summary.
///
/// Each job is `(subagent, prompt)` and runs its own independent `delegate` loop
/// (own workspace root, own bounded loop). At most `MAX_CONCURRENCY` run at once so
/// local inference isn't oversubscribed; results are re-ordered to match the input
/// so the calling model sees stable [1], [2], ... labels. Wall-clock is ~the slowest
/// job, not the sum — this is the point of fanning out.
pub async fn delegate_batch(
    client: &AsyncForgeClient,
    model: &str,
    root: PathBuf,
    jobs: Vec<(String, String)>,
) -> String {
    if jobs.is_empty() {
        return "task_batch: no tasks provided".to_string();
    }
    let n = jobs.len();
    let mut results: Vec<(usize, String, String)> = stream::iter(jobs.into_iter().enumerate())
        .map(|(i, (sub, prompt))| {
            let root = root.clone();
            async move {
                let r = delegate(client, model, root, &sub, &prompt).await;
                (i, sub, r)
            }
        })
        .buffer_unordered(MAX_CONCURRENCY)
        .collect()
        .await;
    results.sort_by_key(|(i, _, _)| *i);

    let mut out = format!("Ran {n} subagents in parallel. Results:\n\n");
    for (i, sub, r) in results {
        out.push_str(&format!("=== [{}] subagent '{}' ===\n{}\n\n", i + 1, sub, r.trim()));
    }
    out.trim_end().to_string()
}
