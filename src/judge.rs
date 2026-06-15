//! LLM supervisor ("judge"): when the agent reaches its step budget or looks
//! stuck, a separate model call reviews the task + recent transcript and decides
//! whether to STOP (done or genuinely blocked), CONTINUE (making progress, grant
//! more steps), or REDIRECT (the current approach is failing, try a new one).
//! This replaces the old behavior of simply giving up at the step cap.

use liteforge::{AsyncForgeClient, ChatCompletionRequest, Message};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Decision {
    Stop,
    Continue,
    Redirect,
}

pub struct Verdict {
    pub decision: Decision,
    pub reason: String,
    /// Next-step or new-approach guidance (for continue / redirect).
    pub guidance: String,
    /// A user-facing summary (for stop).
    pub summary: String,
}

const SYSTEM: &str = "You are the supervisor of an autonomous coding agent. The agent works ONLY inside \
a workspace directory: it CANNOT read or edit files outside it (for example files in the user's home \
directory like ~/.npmrc), and it cannot run interactive or never-exiting commands. The agent has paused \
and needs your decision.\n\n\
Review the TASK and the RECENT TRANSCRIPT, then choose exactly one decision:\n\
- \"stop\": the task is essentially complete, OR it is genuinely blocked / impossible within the \
workspace (e.g. it requires editing files outside the workspace, or missing credentials). Give a short \
user-facing \"summary\" of what was done and what (if anything) the user must do.\n\
- \"continue\": the agent is making real progress and just needs more steps. Give brief \"guidance\" for \
the immediate next step.\n\
- \"redirect\": the agent is repeating a failing approach. Give \"guidance\" describing a DIFFERENT, \
concrete approach that stays inside the workspace (e.g. set an env var inline in the command instead of \
editing a home-directory file).\n\n\
Respond with ONLY a JSON object and nothing else:\n\
{\"decision\":\"stop|continue|redirect\",\"reason\":\"<one line>\",\"guidance\":\"<next step or new \
approach>\",\"summary\":\"<user-facing summary, for stop>\"}";

/// Consult the judge. Never panics; on any error defaults to a safe Stop.
pub async fn judge(
    client: &AsyncForgeClient,
    model: &str,
    task: &str,
    transcript_tail: &str,
    trigger: &str,
) -> Verdict {
    let user = format!(
        "TASK:\n{task}\n\nThe agent paused because: {trigger}.\n\nRECENT TRANSCRIPT (most recent last):\n{transcript_tail}\n\nDecide now."
    );
    let messages = vec![Message::system(SYSTEM), Message::user(&user)];
    let mut req = ChatCompletionRequest::new(model.to_string(), messages);
    req.temperature = Some(0.0);
    req.max_tokens = Some(400);

    let text = match client.chat_completions(req).await {
        Ok(resp) => resp
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default(),
        Err(e) => {
            return Verdict {
                decision: Decision::Stop,
                reason: format!("judge call failed: {e}"),
                guidance: String::new(),
                summary: String::new(),
            }
        }
    };
    parse(&text)
}

const PREFLIGHT_SYSTEM: &str = "You triage a coding TASK for an autonomous agent BEFORE it acts. \
The agent runs inside a workspace and tends to OVER-use tools (extra reads, redundant commands, \
starting servers to 'verify'). Decide the minimal path:\n\
- If the task is conversational — a greeting, small talk, or a request to SAY/reply with a short \
message in chat (e.g. \"say hello\" -> \"Hello!\", \"introduce yourself\", \"thanks\") — choose \"stop\" \
and put the spoken reply in \"summary\". Do NOT create files or run commands for a greeting or chat \
message. BUT if the request names a programming language or asks to PRODUCE CODE (e.g. \"say hello in \
rust\", \"hello world in python\", \"print hello in C\"), that is a CODING task — choose \"continue\", \
NOT stop.\n\
- Choose \"stop\" ONLY if the task is a pure question answerable from general knowledge with NO file \
changes and NO commands AND the answer does NOT depend on live/system state. Put the complete answer \
in \"summary\". NEVER answer real-time or environment facts from memory — the current date/time, day of \
the week, what's in a file, the OS, installed versions, env vars, git status, etc. ALL require a \
command, so for those choose \"continue\".\n\
- Otherwise choose \"continue\" and put a SHORT minimal plan in \"guidance\": the fewest tool steps \
needed and nothing more. Do NOT include extra verification, do NOT start servers, do NOT re-read files \
you just wrote unless it's required.\n\n\
Respond with ONLY a JSON object:\n\
{\"decision\":\"stop|continue\",\"reason\":\"<one line>\",\"guidance\":\"<minimal plan, for continue>\",\"summary\":\"<direct answer, for stop>\"}";

/// Pre-flight triage (non-tool): runs once BEFORE the agent loop. Returns Stop (with
/// a direct answer in `summary`) when the task needs no tools, else Continue (with a
/// minimal plan in `guidance`). On any error defaults to Continue so work still runs.
pub async fn preflight(client: &AsyncForgeClient, model: &str, task: &str) -> Verdict {
    let user = format!("TASK:\n{task}\n\nTriage now.");
    let messages = vec![Message::system(PREFLIGHT_SYSTEM), Message::user(&user)];
    let mut req = ChatCompletionRequest::new(model.to_string(), messages);
    req.temperature = Some(0.0);
    req.max_tokens = Some(400);
    let text = match client.chat_completions(req).await {
        Ok(resp) => resp
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default(),
        Err(_) => {
            return Verdict {
                decision: Decision::Continue, // never skip work on a failed triage
                reason: "preflight call failed".into(),
                guidance: String::new(),
                summary: String::new(),
            }
        }
    };
    parse(&text)
}

/// Parse the judge's JSON reply leniently (it may wrap the object in prose).
fn parse(text: &str) -> Verdict {
    let obj = extract_json(text);
    let get = |k: &str| -> String {
        obj.as_ref()
            .and_then(|v| v.get(k))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let decision_raw = get("decision").to_lowercase();
    let decision = if decision_raw.contains("redirect") {
        Decision::Redirect
    } else if decision_raw.contains("continue") {
        Decision::Continue
    } else if decision_raw.contains("stop") {
        Decision::Stop
    } else {
        // Fall back to a keyword scan of the whole reply.
        let l = text.to_lowercase();
        if l.contains("redirect") {
            Decision::Redirect
        } else if l.contains("continue") || l.contains("keep going") {
            Decision::Continue
        } else {
            Decision::Stop
        }
    };
    Verdict {
        decision,
        reason: get("reason"),
        guidance: get("guidance"),
        summary: get("summary"),
    }
}

/// Find the first balanced `{...}` JSON object in `s` and parse it.
fn extract_json(s: &str) -> Option<serde_json::Value> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        match b {
            b'"' if !esc => in_str = !in_str,
            b'\\' if in_str => {
                esc = !esc;
                continue;
            }
            b'{' if !in_str => depth += 1,
            b'}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&s[start..=i]).ok();
                }
            }
            _ => {}
        }
        esc = false;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let v = parse(r#"{"decision":"redirect","reason":"blocked","guidance":"set the env var inline","summary":""}"#);
        assert_eq!(v.decision, Decision::Redirect);
        assert_eq!(v.guidance, "set the env var inline");
    }

    #[test]
    fn parses_json_wrapped_in_prose() {
        let v = parse("Here is my decision:\n{\"decision\": \"stop\", \"summary\": \"all done\"}\nThanks");
        assert_eq!(v.decision, Decision::Stop);
        assert_eq!(v.summary, "all done");
    }

    #[test]
    fn falls_back_on_garbage() {
        let v = parse("I think we should continue working on this.");
        assert_eq!(v.decision, Decision::Continue);
    }

    #[test]
    fn defaults_to_stop_when_empty() {
        assert_eq!(parse("").decision, Decision::Stop);
    }
}
