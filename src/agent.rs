//! Event-driven agent loop shared by the headless driver and the TUI.
//!
//! `run_agent` drives the write→run→fix loop and emits `AgentEvent`s over an
//! mpsc channel; mutating tools request approval via a oneshot the consumer
//! answers (TUI modal / stdin prompt / auto with --yolo).

use crate::extract;
use crate::hooks::{Decision, Hooks};
use crate::permission::{Permission, PermissionSet};
use crate::tools::{self, Tools};
use liteforge::{AsyncForgeClient, ChatCompletionRequest, Message};
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

/// Steps per "leg" before the supervisor (judge) is consulted.
const MAX_STEPS: usize = 18;
/// Maximum number of judge-granted extensions before a hard stop.
const MAX_LEGS: usize = 3;

/// Events streamed from the agent to whatever is driving the UI.
pub enum AgentEvent {
    Token(String),
    Assistant(String),
    ToolCall { name: String, args: String },
    ToolResult { name: String, text: String },
    Approval { desc: String, resp: oneshot::Sender<bool> },
    Final(String),
    Error(String),
    Done,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_agent(
    client: AsyncForgeClient,
    model: String,
    ladder: crate::router::Ladder,
    tools: Tools,
    task: String,
    system: String,
    read_only: bool,
    history: Vec<(String, String)>,
    perms: PermissionSet,
    hooks: Hooks,
    mcp: std::sync::Arc<crate::mcp_tools::McpTools>,
    think: crate::router::Think,
    tx: mpsc::Sender<AgentEvent>,
) {
    let mut defs = if read_only {
        tools::read_only_defs()
    } else {
        tools::tool_defs()
    };
    if !read_only {
        defs.extend(mcp.defs());
    }
    // For models that don't reliably native-tool-call, append a strict
    // tool-call protocol hint to the system prompt (prompt-level structured
    // output; backends that enforce a schema would also accept the schema).
    let system = if crate::structured::wants_constraint(&model) {
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        format!("{system}\n\n{}", crate::structured::tool_protocol_hint(&names))
    } else {
        system
    };
    // reasoning-effort hint for elevated thinking levels
    let system = match think {
        crate::router::Think::High => {
            format!("{system}\n\nThink carefully and reason step by step before acting.")
        }
        crate::router::Think::Xtra => {
            format!("{system}\n\nUse extended, thorough reasoning. Consider edge cases and alternative approaches, and double-check each step before and after acting.")
        }
        _ => system,
    };
    let mut messages = vec![Message::system(&system)];
    for (u, a) in &history {
        messages.push(Message::user(u));
        messages.push(Message::assistant(a));
    }
    messages.push(Message::user(&task));
    let mut seen: HashMap<String, usize> = HashMap::new();

    // tier router: keep the user's model as the floor, escalate only when stuck.
    // The ladder is supplied by the caller (the TUI passes the routed specialty
    // ladder for "Auto"; other entry points pass the generic local ladder).
    let mut model = model;
    let mut ladder = ladder;
    if !ladder.models.iter().any(|m| m == &model) {
        ladder.models.insert(0, model.clone());
    }
    // complexity-aware starting tier: bump UP for clearly-hard tasks, but never
    // below the model the user chose (their selection is the floor). When an
    // elevated thinking level is set, the CALLER already supplied the bigger
    // "thinking" model as `model`, so we skip the classify-bump and respect it.
    if !think.forces_top() {
        let want = crate::router::classify_start(&task);
        let suggested = ladder.model_for(want);
        if let (Some(ci), Some(si)) = (
            ladder.models.iter().position(|m| m == &model),
            ladder.models.iter().position(|m| m == &suggested),
        ) {
            if si > ci {
                model = suggested;
            }
        }
    }
    let mut empty_intent = 0usize; // turns that looked like a tool call but didn't parse
    let mut errors = 0usize; // consecutive tool-error turns
    let mut last_had_error = false; // did the most recent tool turn fail?
    let mut final_after_error = 0usize; // judge gates on a final that follows a failure
    let mut punt_pushes = 0usize; // nudges when the model instructs/asks the user instead of acting
    let mut edits_made = 0usize; // successful file-mutating tool calls this run
    let mut noop_pushes = 0usize; // nudges when the model claims success with no edits

    macro_rules! send {
        ($e:expr) => {
            if tx.send($e).await.is_err() {
                return;
            }
        };
    }

    // Pre-flight triage (non-tool model, via the judge): small models over-call
    // tools, so before the loop a plain-text pass decides the minimal path. A pure
    // question -> answer directly and never enter the tool loop; anything else ->
    // inject a minimal plan so the agent doesn't fire redundant calls. Guarded so an
    // actionable task is never skipped (that would re-introduce the do-nothing punt),
    // and the loop's escalate-on-no-edit net still applies. Disable: SMOLCODE_PREFLIGHT=0.
    if std::env::var("SMOLCODE_PREFLIGHT").map(|v| v != "0").unwrap_or(true) {
        // Triage on a reliable mid model (the generic tier, not a tiny specialist):
        // the stop/continue decision quality matters more than its cost. Falls back
        // to the start model for a single-rung (pinned) ladder.
        let pf_model = ladder
            .models
            .get(ladder.models.len().saturating_sub(2))
            .cloned()
            .unwrap_or_else(|| model.clone());
        let pf = crate::judge::preflight(&client, &pf_model, &task).await;
        if matches!(pf.decision, crate::judge::Decision::Stop)
            && !pf.summary.trim().is_empty()
            && !looks_actionable(&task)
            && !needs_runtime_info(&task)
        {
            send!(AgentEvent::Final(pf.summary));
            send!(AgentEvent::Done);
            return;
        }
        if !pf.guidance.trim().is_empty() {
            messages.push(Message::user(format!(
                "Before acting, here is the minimal plan — follow it and make ONLY the tool calls it \
                 requires, nothing extra (no redundant reads, no servers, no superfluous verification): {}",
                pf.guidance
            )));
        }
    }

    // Xtra thinking gets a slightly larger per-leg step budget for thoroughness.
    let max_steps = if matches!(think, crate::router::Think::Xtra) { MAX_STEPS + 6 } else { MAX_STEPS };
    let mut step = 0usize;
    let mut legs = 0usize;
    loop {
        step += 1;
        let mut req = ChatCompletionRequest::new(model.clone(), messages.clone());
        req.tools = Some(defs.clone());
        req.temperature = Some(0.0);
        req.max_tokens = Some(2048);

        // streaming completion: forward token deltas to the UI as they arrive
        let (ttx, mut trx) = mpsc::channel::<String>(64);
        let tx_fwd = tx.clone();
        let fwd = tokio::spawn(async move {
            while let Some(tok) = trx.recv().await {
                if tx_fwd.send(AgentEvent::Token(tok)).await.is_err() {
                    break;
                }
            }
        });
        let streamed = crate::stream::run(&client, req, ttx).await;
        let _ = fwd.await;

        let mut msg = Message::assistant(streamed.content);
        msg.tool_calls = (!streamed.tool_calls.is_empty()).then_some(streamed.tool_calls);

        let (calls, native) = extract::extract(&msg);
        if calls.is_empty() {
            // The model produced no tool call. If the text *looks* like a botched
            // tool call (common with small models), inject a corrective hint and
            // retry — escalating a tier if it keeps happening — instead of
            // treating the garbled output as a final answer.
            let content = msg.content.clone().unwrap_or_default();
            let flaws = crate::repair::diagnose(&content, &calls);
            if !flaws.is_empty() && empty_intent < 2 {
                empty_intent += 1;
                if let Some(hint) = crate::repair::repair_hint(&flaws) {
                    messages.push(Message::assistant(content));
                    if crate::router::should_escalate(0, empty_intent, errors) {
                        if let Some(bigger) = ladder.escalate(&model) {
                            send!(AgentEvent::Error(format!(
                                "small model couldn't form a tool call — escalating to {bigger}"
                            )));
                            model = bigger;
                        }
                    }
                    messages.push(Message::user(hint));
                    continue;
                }
            }
            // Autonomy guard: the agent must DO the work, not end its turn by
            // handing the user instructions or asking "would you like me to…".
            if looks_like_punt(&content) && punt_pushes < 2 {
                punt_pushes += 1;
                messages.push(Message::assistant(content.clone()));
                messages.push(Message::user(
                    "You are an autonomous agent. Do NOT give the user a list of commands to run, \
                     and do NOT ask the user questions or for permission — perform the work \
                     yourself by calling a tool. Take the next concrete action now.",
                ));
                empty_intent = 0;
                continue;
            }

            // No-op / punt guard: the model ended its turn without editing any file
            // and either CLAIMS it's done or returned an EMPTY response. Small models
            // punt on harder tasks both ways (a do-nothing "done" is how "Auto" on a
            // 1.5B looked like it "immediately stopped"). Escalate a tier and push back
            // forcefully instead of accepting a do-nothing finish.
            let did_nothing = content.trim().is_empty();
            if edits_made == 0 && !read_only && noop_pushes < 3
                && (claims_work_done(&content) || did_nothing)
            {
                noop_pushes += 1;
                messages.push(Message::assistant(
                    if did_nothing { "(no action taken)".to_string() } else { content.clone() },
                ));
                if let Some(bigger) = ladder.escalate(&model) {
                    send!(AgentEvent::Error(format!(
                        "no file edits yet — escalating to {bigger}"
                    )));
                    model = bigger;
                }
                messages.push(Message::user(
                    "STOP. You have NOT edited any file this session, so the task is NOT done and the \
                     code is unchanged. Do not summarize or claim success. Call write_file (or \
                     str_replace) RIGHT NOW to actually write the implementation, then run it to verify. \
                     If you truly believe no edit is needed, prove it by running code that exercises the \
                     required behavior.",
                ));
                empty_intent = 0;
                seen.clear();
                continue;
            }

            // Verify-before-finish: don't accept a final answer (often a
            // hallucinated "it works!" right after an escalation) when the most
            // recent action failed. Let the supervisor decide.
            if last_had_error && final_after_error < 2 {
                final_after_error += 1;
                messages.push(Message::assistant(content.clone()));
                let tail = transcript_tail(&messages, 16);
                let v = crate::judge::judge(
                    &client,
                    &model,
                    &task,
                    &tail,
                    "it is about to give a final answer, but its most recent action failed — confirm the task actually succeeded before reporting success",
                )
                .await;
                match v.decision {
                    crate::judge::Decision::Stop => {
                        let summary = if !v.summary.trim().is_empty() { v.summary } else { content };
                        send!(AgentEvent::Final(summary));
                        send!(AgentEvent::Done);
                        return;
                    }
                    crate::judge::Decision::Continue => {
                        send!(AgentEvent::Error(format!("supervisor: not done yet — {}", clip(&v.reason, 100))));
                        let g = if v.guidance.trim().is_empty() { "The last step failed; fix it before reporting success.".to_string() } else { v.guidance };
                        messages.push(Message::user(format!(
                            "Do not report success yet: your last action failed. {g}"
                        )));
                        empty_intent = 0;
                        continue;
                    }
                    crate::judge::Decision::Redirect => {
                        send!(AgentEvent::Error(format!("supervisor: change approach — {}", clip(&v.reason, 100))));
                        if let Some(bigger) = ladder.escalate(&model) {
                            model = bigger;
                        }
                        let g = if v.guidance.trim().is_empty() { "Try a different approach that stays inside the workspace.".to_string() } else { v.guidance };
                        messages.push(Message::user(format!(
                            "Your last action failed and your approach is not working. Change approach: {g}"
                        )));
                        seen.clear();
                        empty_intent = 0;
                        continue;
                    }
                }
            }
            send!(AgentEvent::Final(content));
            send!(AgentEvent::Done);
            return;
        }

        if let Some(c) = &msg.content {
            if !c.trim().is_empty() {
                send!(AgentEvent::Assistant(c.clone()));
            }
        }
        let mut am = Message::assistant(msg.content.clone().unwrap_or_default());
        am.tool_calls = msg.tool_calls.clone();
        messages.push(am);

        let mut observations = String::new();
        let mut max_repeat = 0usize;
        let mut had_error = false;
        for call in &calls {
            let sig = format!("{}::{}", call.name, call.args);
            let n = seen.entry(sig).or_insert(0);
            *n += 1;
            max_repeat = max_repeat.max(*n);

            send!(AgentEvent::ToolCall {
                name: call.name.clone(),
                args: call.args.clone(),
            });

            let is_mcp = mcp.has(&call.name);
            let is_task = call.name == "task";
            let is_task_batch = call.name == "task_batch";
            let is_special = is_mcp || is_task || is_task_batch;

            // 1) permission gate (allow / ask / deny) — MCP + subagent tools are opted-in
            let mut blocked: Option<String> = None;
            if !is_special {
                match perms.for_tool(&call.name) {
                    Permission::Deny => blocked = Some("blocked: permission denied".into()),
                    Permission::Ask => {
                        let (rtx, rrx) = oneshot::channel();
                        send!(AgentEvent::Approval {
                            desc: format!("{} {}", call.name, clip(&call.args, 70)),
                            resp: rtx,
                        });
                        if !rrx.await.unwrap_or(false) {
                            blocked = Some("DENIED by user".into());
                        }
                    }
                    Permission::Allow => {}
                }
            }

            // 2) before-tool command hooks (can deny or rewrite args)
            let mut eff_args = call.args.clone();
            if blocked.is_none() {
                match hooks.before_tool(&call.name, &eff_args) {
                    Decision::Allow => {}
                    Decision::Deny(r) => blocked = Some(format!("blocked by hook: {r}")),
                    Decision::Modify(a) => eff_args = a,
                }
            }

            let result = if let Some(b) = blocked {
                b
            } else if is_task {
                let v: serde_json::Value = serde_json::from_str(&eff_args).unwrap_or(serde_json::json!({}));
                let sub = v.get("subagent").and_then(|x| x.as_str()).unwrap_or("explore").to_string();
                let prompt = v.get("prompt").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let r = crate::delegate::delegate(&client, &model, tools.root.clone(), &sub, &prompt).await;
                hooks.fire("tool.execute.after", serde_json::json!({"tool": "task", "result": r}));
                r
            } else if is_task_batch {
                let v: serde_json::Value = serde_json::from_str(&eff_args).unwrap_or(serde_json::json!({}));
                let jobs: Vec<(String, String)> = v
                    .get("tasks")
                    .and_then(|t| t.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|j| {
                                let sub = j.get("subagent").and_then(|x| x.as_str()).unwrap_or("explore").to_string();
                                let p = j.get("prompt").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                (sub, p)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let r = crate::delegate::delegate_batch(&client, &model, tools.root.clone(), jobs).await;
                hooks.fire("tool.execute.after", serde_json::json!({"tool": "task_batch", "result": r}));
                r
            } else if is_mcp {
                let r = mcp.dispatch(&call.name, &eff_args).await;
                hooks.fire("tool.execute.after", serde_json::json!({"tool": call.name, "result": r}));
                r
            } else {
                let r = tools
                    .dispatch(&call.name, &eff_args)
                    .unwrap_or_else(|e| format!("error: {e}"));
                hooks.fire(
                    "tool.execute.after",
                    serde_json::json!({"tool": call.name, "result": r}),
                );
                r
            };
            let lower = result.trim_start().to_ascii_lowercase();
            // Failure signals: tool errors, blocks, a non-zero shell/python exit
            // (results start with "exit=N"), a no-match edit, or a timeout.
            let nonzero_exit = lower.starts_with("exit=") && !lower.starts_with("exit=0\n") && !lower.starts_with("exit=0 ");
            let failed = lower.starts_with("error")
                || lower.starts_with("blocked")
                || result.contains("DENIED")
                || nonzero_exit
                || lower.starts_with("no match for")
                || lower.contains("timed out after");
            if failed {
                had_error = true;
            } else if matches!(call.name.as_str(), "write_file" | "str_replace" | "apply_patch" | "multi_edit") {
                edits_made += 1; // an actual file-editing tool ran (not run_shell/run_python)
            }
            send!(AgentEvent::ToolResult {
                name: call.name.clone(),
                text: result.clone(),
            });
            push_result(&mut messages, &mut observations, native, call, result);
        }

        if !native {
            messages.push(Message::user(format!(
                "Tool results:\n{observations}\nContinue with the next step, or give your final answer."
            )));
        }

        // tier escalation: if the small model is repeating itself or erroring,
        // hand off to the next model up and give it a clean slate.
        errors = if had_error { errors + 1 } else { 0 };
        last_had_error = had_error;
        if crate::router::should_escalate(max_repeat, 0, errors) {
            if let Some(bigger) = ladder.escalate(&model) {
                send!(AgentEvent::Error(format!("model stuck — escalating to {bigger}")));
                model = bigger;
                seen.clear();
                errors = 0;
                continue;
            }
        }
        if max_repeat == 3 {
            messages.push(Message::user(
                "You are repeating the same tool call. Stop repeating it: try a different \
                 approach, or if the task is done, reply with a short final answer and NO tool call.",
            ));
        }

        // Supervisor checkpoint: instead of dead-stopping at the step budget or
        // when stuck, ask an LLM judge whether to stop, keep going, or redirect.
        let stuck = max_repeat >= 5;
        let over_budget = step >= max_steps;
        if stuck || over_budget {
            if legs >= MAX_LEGS {
                let last = last_assistant(&messages);
                let msg = if last.trim().is_empty() {
                    "Stopping after extended effort without finishing. The task may be blocked; please review.".to_string()
                } else {
                    format!("Stopping after extended effort. Latest status:\n{last}")
                };
                send!(AgentEvent::Final(msg));
                send!(AgentEvent::Done);
                return;
            }
            legs += 1;
            let trigger = if stuck {
                "it is repeating the same action without making progress"
            } else {
                "it reached its step budget for this leg"
            };
            let tail = transcript_tail(&messages, 16);
            let v = crate::judge::judge(&client, &model, &task, &tail, trigger).await;
            match v.decision {
                crate::judge::Decision::Stop => {
                    let summary = if !v.summary.trim().is_empty() {
                        v.summary
                    } else if !v.reason.trim().is_empty() {
                        v.reason
                    } else {
                        let last = last_assistant(&messages);
                        if last.trim().is_empty() { "The task is complete or cannot proceed further.".into() } else { last }
                    };
                    send!(AgentEvent::Final(summary));
                    send!(AgentEvent::Done);
                    return;
                }
                crate::judge::Decision::Continue => {
                    send!(AgentEvent::Error(format!("supervisor: keep going — {}", clip(&v.reason, 100))));
                    let g = if v.guidance.trim().is_empty() { "Continue toward the goal.".to_string() } else { v.guidance };
                    messages.push(Message::user(format!("Keep going. {g}")));
                    step = 0;
                    seen.clear();
                    errors = 0;
                }
                crate::judge::Decision::Redirect => {
                    send!(AgentEvent::Error(format!("supervisor: change approach — {}", clip(&v.reason, 100))));
                    if let Some(bigger) = ladder.escalate(&model) {
                        model = bigger;
                    }
                    let g = if v.guidance.trim().is_empty() {
                        "Try a different approach that stays inside the workspace.".to_string()
                    } else {
                        v.guidance
                    };
                    messages.push(Message::user(format!(
                        "Your current approach is not working. Change approach: {g} \
                         Do this yourself by calling tools — do not give the user instructions or ask questions."
                    )));
                    step = 0;
                    seen.clear();
                    errors = 0;
                }
            }
        }
    }
}

/// Heuristic: a final answer that defers to the user (gives them commands to
/// run, or asks "would you like me to…") instead of doing the work itself.
/// Heuristic: the task asks for an action (create/build/fix/run/…) rather than a
/// pure question. Used to guard the pre-flight "answer directly" skip so an
/// actionable task is never short-circuited into a no-op answer.
fn looks_actionable(task: &str) -> bool {
    let t = task.to_lowercase();
    const VERBS: &[&str] = &[
        "create", "build", "write", "add", "implement", "fix", "make", "run",
        "generate", "refactor", "rename", "delete", "remove", "install", "set up",
        "setup", "scaffold", "update", "modify", "edit", "replace", "convert",
        "migrate", "commit", "populate", "configure", "rewrite", "patch", "test",
    ];
    VERBS.iter().any(|v| t.contains(v))
}

/// Heuristic: the task's answer depends on live/system state (date, time, files,
/// env, versions) and therefore needs a tool — never answer it from memory.
fn needs_runtime_info(task: &str) -> bool {
    let t = task.to_lowercase();
    const CUES: &[&str] = &[
        "today", "what day", "day of the week", "what time", "what date",
        "current date", "current time", "right now", "what's the date",
        "whats the date", "this week", "what version", "installed", "what's in",
        "whats in", "contents of", "git status", "uname", "environment variable",
        "env var", "how many files", "list the files", "what os",
    ];
    CUES.iter().any(|c| t.contains(c))
}

fn looks_like_punt(content: &str) -> bool {
    let c = content.to_lowercase();
    const PHRASES: &[&str] = &[
        "would you like me to",
        "do you want me to",
        "shall i ",
        "should i ",
        "let me know if you",
        "let me know how",
        "here's how you can",
        "here is how you can",
        "you can run",
        "you should run",
        "you need to run",
        "you'll need to run",
        "please run",
        "if you prefer",
        "guide you through",
        "would you like",
        "do you want",
    ];
    PHRASES.iter().any(|p| c.contains(p))
}

/// Heuristic: the final answer asserts that work (an edit/implementation) was
/// actually performed — used to catch "I did it" claims with zero edits.
fn claims_work_done(content: &str) -> bool {
    let c = content.to_lowercase();
    const VERBS: &[&str] = &[
        "implemented", "created", "added", "fixed", "wrote", "written", "updated",
        "modified", "changed", "edited", "replaced", "refactored", "completed the",
        "has been created", "has been implemented", "has been fixed", "now works",
        "successfully",
    ];
    VERBS.iter().any(|v| c.contains(v))
}

/// The most recent non-empty assistant message content.
fn last_assistant(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant" && m.content.as_deref().map(|c| !c.trim().is_empty()).unwrap_or(false))
        .and_then(|m| m.content.clone())
        .unwrap_or_default()
}

/// A compact transcript of the last `n` messages for the judge.
fn transcript_tail(messages: &[Message], n: usize) -> String {
    let start = messages.len().saturating_sub(n);
    let mut out = String::new();
    for m in &messages[start..] {
        let mut line = format!("{}: ", m.role);
        if let Some(c) = &m.content {
            if !c.trim().is_empty() {
                line.push_str(c.trim());
            }
        }
        if let Some(tcs) = &m.tool_calls {
            for tc in tcs {
                line.push_str(&format!(" [call {}({})]", tc.function.name, clip(&tc.function.arguments, 80)));
            }
        }
        out.push_str(&clip(&line, 320));
        out.push('\n');
    }
    clip(&out, 4000)
}

fn push_result(
    messages: &mut Vec<Message>,
    observations: &mut String,
    native: bool,
    call: &extract::Call,
    result: String,
) {
    if native {
        messages.push(Message::tool(call.id.clone(), result));
    } else {
        observations.push_str(&format!("[{}] -> {}\n", call.name, result));
    }
}

pub fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}
