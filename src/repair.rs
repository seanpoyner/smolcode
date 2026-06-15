//! Tool-call repair — detect malformed / missing tool calls from small models
//! and emit a concise corrective hint to inject back into the conversation.
//!
//! Small local models often emit broken tool calls (or none when one was
//! intended). `diagnose` classifies the failure after `crate::extract::extract`
//! has run; `repair_hint` turns the findings into a short, model-facing nudge.

use serde_json::Value;

/// The set of tools the agent exposes.
const KNOWN_TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "str_replace",
    "apply_patch",
    "search",
    "list_dir",
    "run_shell",
    "run_python",
    "repo_map",
    "task",
];

/// Required argument keys per tool. Tools absent here (or with an empty slice)
/// take no required arguments.
fn required_args(tool: &str) -> &'static [&'static str] {
    match tool {
        "read_file" => &["path"],
        "write_file" => &["path", "content"],
        "str_replace" => &["path", "old", "new"],
        "apply_patch" => &["patch"],
        "search" => &["pattern"],
        "run_shell" => &["command"],
        "run_python" => &["path"],
        "task" => &["subagent", "prompt"],
        // list_dir, repo_map: no required args.
        _ => &[],
    }
}

/// What went wrong with a model turn (if anything actionable).
#[derive(Clone, PartialEq, Debug)]
pub enum Flaw {
    /// The assistant text *looks like* it intended a tool call (mentions a tool
    /// name, shows a JSON-ish or <tool_call> fragment) but nothing parsed out.
    UnparsedToolCall,
    /// A parsed call names a tool that does not exist.
    UnknownTool(String),
    /// A parsed call is missing required arguments for its tool.
    BadArgs { tool: String, missing: Vec<String> },
}

/// Inspect an assistant turn AFTER extraction. `text` is the assistant content,
/// `parsed` are the calls extract() returned. Returns any flaws worth repairing.
/// (Empty Vec => the turn is fine / a genuine final answer.)
pub fn diagnose(text: &str, parsed: &[crate::extract::Call]) -> Vec<Flaw> {
    let mut flaws = Vec::new();

    if parsed.is_empty() {
        if looks_like_intended_tool_call(text) {
            flaws.push(Flaw::UnparsedToolCall);
        }
        return flaws;
    }

    for call in parsed {
        // A single call yields at most one flaw; prefer UnknownTool over BadArgs.
        if !KNOWN_TOOLS.contains(&call.name.as_str()) {
            flaws.push(Flaw::UnknownTool(call.name.clone()));
            continue;
        }
        let missing = missing_args(&call.name, &call.args);
        if !missing.is_empty() {
            flaws.push(Flaw::BadArgs { tool: call.name.clone(), missing });
        }
    }

    flaws
}

/// Required keys that are absent or empty-string in `args`. If `args` isn't
/// valid JSON for a tool that needs args, ALL required keys count as missing.
fn missing_args(tool: &str, args: &str) -> Vec<String> {
    let required = required_args(tool);
    if required.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Value>(args) {
        Ok(Value::Object(map)) => required
            .iter()
            .filter(|k| match map.get(**k) {
                None => true,
                Some(Value::String(s)) => s.is_empty(),
                Some(Value::Null) => true,
                Some(_) => false,
            })
            .map(|k| k.to_string())
            .collect(),
        // Not a JSON object => everything required is missing.
        _ => required.iter().map(|k| k.to_string()).collect(),
    }
}

/// Heuristic: does prose look like a botched/unparsed tool call?
fn looks_like_intended_tool_call(text: &str) -> bool {
    if text.contains("<tool_call>") || text.contains("</tool_call>") {
        return true;
    }

    // A JSON object mentioning a name plus an arguments-ish key.
    if text.contains("\"name\"")
        && (text.contains("\"arguments\"")
            || text.contains("\"args\"")
            || text.contains("\"parameters\""))
    {
        return true;
    }

    // A ```json fence with a brace-delimited object inside.
    if let Some(fence) = text.find("```json") {
        let after = &text[fence + "```json".len()..];
        if let Some(open) = after.find('{') {
            if after[open..].contains('}') {
                return true;
            }
        }
    }

    // A known tool name appearing as a standalone word.
    KNOWN_TOOLS.iter().any(|t| contains_word(text, t))
}

/// True if `needle` appears in `haystack` not flanked by identifier characters.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(pos) = haystack[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_ident(bytes[start - 1] as char);
        let after_ok = end >= bytes.len() || !is_ident(bytes[end] as char);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// A concise, model-facing corrective message for the given flaws: restate the
/// exact tool-call protocol the model must follow, name the specific problem,
/// and tell it to retry with a single well-formed tool call. Returns None if
/// there are no flaws.
pub fn repair_hint(flaws: &[Flaw]) -> Option<String> {
    if flaws.is_empty() {
        return None;
    }

    let mut lines = vec![
        "Call exactly one tool using the tool-call format with a JSON arguments object.".to_string(),
    ];

    let mut said_unparsed = false;
    let mut said_unknown = false;
    let mut said_badargs = false;

    for flaw in flaws {
        match flaw {
            Flaw::UnparsedToolCall if !said_unparsed => {
                said_unparsed = true;
                lines.push(
                    "Your previous reply looked like a tool call but was not valid; emit one well-formed tool call and nothing else."
                        .to_string(),
                );
            }
            Flaw::UnknownTool(name) => {
                said_unknown = true;
                lines.push(format!(
                    "Tool '{name}' does not exist; valid tools are: {}.",
                    KNOWN_TOOLS.join(", ")
                ));
            }
            Flaw::BadArgs { tool, missing } if !said_badargs => {
                said_badargs = true;
                lines.push(format!(
                    "Tool '{tool}' is missing required argument(s): {}.",
                    missing.join(", ")
                ));
            }
            _ => {}
        }
    }

    let _ = said_unknown; // each UnknownTool names its own tool; kept distinct.
    lines.push("Retry with a single, complete tool call.".to_string());

    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &str) -> crate::extract::Call {
        crate::extract::Call {
            id: String::new(),
            name: name.into(),
            args: args.into(),
        }
    }

    #[test]
    fn plain_prose_is_clean() {
        let flaws = diagnose("The function returns the sum of its two inputs.", &[]);
        assert!(flaws.is_empty());
    }

    #[test]
    fn intended_but_unparsed() {
        let flaws = diagnose("I'll call write_file {\"path\":\"a.py\"}", &[]);
        assert_eq!(flaws, vec![Flaw::UnparsedToolCall]);
    }

    #[test]
    fn unknown_tool_name() {
        let flaws = diagnose("", &[call("frobnicate", "{}")]);
        assert_eq!(flaws, vec![Flaw::UnknownTool("frobnicate".into())]);
    }

    #[test]
    fn missing_required_arg() {
        let flaws = diagnose("", &[call("write_file", "{\"path\":\"a.py\"}")]);
        assert_eq!(
            flaws,
            vec![Flaw::BadArgs {
                tool: "write_file".into(),
                missing: vec!["content".into()],
            }]
        );
    }

    #[test]
    fn hint_empty_and_nonempty() {
        assert!(repair_hint(&[]).is_none());
        let hint = repair_hint(&[Flaw::UnknownTool("frobnicate".into())]).unwrap();
        assert!(hint.contains("frobnicate"));
    }
}
