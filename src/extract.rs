//! Multi-format tool-call extractor — the piece that makes small models agentic.
//!
//! Prefers native `tool_calls`; otherwise parses Hermes `<tool_call>` tags,
//! fenced ```json blocks, or a bare top-level JSON object from the text content.
//! Everything normalizes to a `Call { id, name, args(JSON string) }`.

use liteforge::Message;

#[derive(Debug, Clone)]
pub struct Call {
    pub id: String,
    pub name: String,
    pub args: String,
}

/// Returns (calls, used_native_protocol).
pub fn extract(msg: &Message) -> (Vec<Call>, bool) {
    if let Some(tcs) = &msg.tool_calls {
        if !tcs.is_empty() {
            let calls = tcs
                .iter()
                .enumerate()
                .map(|(i, tc)| Call {
                    id: if tc.id.is_empty() { format!("call_{i}") } else { tc.id.clone() },
                    name: tc.function.name.clone(),
                    args: if tc.function.arguments.trim().is_empty() {
                        "{}".into()
                    } else {
                        tc.function.arguments.clone()
                    },
                })
                .collect();
            return (calls, true);
        }
    }
    let content = msg.content.clone().unwrap_or_default();
    (parse_text(&content), false)
}

fn parse_text(text: &str) -> Vec<Call> {
    let mut calls = Vec::new();

    // Some models (e.g. qwen-coder via Ollama) emit a tool call as text with a
    // Python-style triple-quoted string for a value, which is invalid JSON.
    // Repair those into proper JSON strings before parsing. (No-op when absent.)
    let repaired = fix_triple_quotes(text);
    let text = repaired.as_str();

    // 1) <tool_call> ... </tool_call> (Hermes / Qwen)
    let mut rest = text;
    while let Some(start) = rest.find("<tool_call>") {
        let after = &rest[start + "<tool_call>".len()..];
        let (inner, next) = match after.find("</tool_call>") {
            Some(end) => (&after[..end], &after[end + "</tool_call>".len()..]),
            None => (after, ""),
        };
        if let Some(c) = parse_call_json(inner, calls.len()) {
            calls.push(c);
        }
        rest = next;
    }
    if !calls.is_empty() {
        return calls;
    }

    // 2) fenced ```json / ```tool_code blocks
    for block in fenced_blocks(text) {
        if let Some(c) = parse_call_json(&block, calls.len()) {
            calls.push(c);
        }
    }
    if !calls.is_empty() {
        return calls;
    }

    // 3) a bare top-level JSON object containing "name"
    if let Some(obj) = first_json_object(text) {
        if let Some(c) = parse_call_json(&obj, 0) {
            calls.push(c);
        }
    }
    calls
}

fn parse_call_json(s: &str, idx: usize) -> Option<Call> {
    let s = s
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```tool_code")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let name = v.get("name").and_then(|n| n.as_str())?.to_string();
    let args = match v.get("arguments").or_else(|| v.get("parameters")) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "{}".to_string(),
    };
    Some(Call { id: format!("call_{idx}"), name, args })
}

fn fenced_blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(s) = rest.find("```") {
        let after = &rest[s + 3..];
        if let Some(e) = after.find("```") {
            out.push(after[..e].to_string());
            rest = &after[e + 3..];
        } else {
            break;
        }
    }
    out
}

/// Replace Python-style `"""triple-quoted"""` string values with proper
/// JSON-escaped string literals, so a tool call that uses them parses. No-op
/// when the text contains no `"""`.
fn fix_triple_quotes(s: &str) -> String {
    if !s.contains("\"\"\"") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("\"\"\"") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        match after.find("\"\"\"") {
            Some(end) => {
                let body = &after[..end];
                out.push_str(&serde_json::to_string(body).unwrap_or_else(|_| "\"\"".to_string()));
                rest = &after[end + 3..];
            }
            None => {
                // unterminated — leave the remainder untouched
                out.push_str("\"\"\"");
                out.push_str(after);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn first_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for i in start..text.len() {
        let c = bytes[i] as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else {
            match c {
                '"' => in_str = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(text[start..=i].to_string());
                    }
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_json_tool_call() {
        let c = parse_text(r#"{"name": "write_file", "arguments": {"path": "a.py", "content": "x"}}"#);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "write_file");
        assert!(c[0].args.contains("a.py"));
    }

    #[test]
    fn triple_quoted_content_is_repaired() {
        // qwen-coder style: a Python triple-quoted string inside the JSON
        let text = "{\"name\": \"write_file\", \"arguments\": {\"path\": \"r.py\", \"content\": \"\"\"\ndef f():\n    print(\"hi\")\n\"\"\"}}";
        let c = parse_text(text);
        assert_eq!(c.len(), 1, "should parse the repaired tool call");
        assert_eq!(c[0].name, "write_file");
        assert!(c[0].args.contains("def f()"), "args: {}", c[0].args);
    }

    #[test]
    fn fix_triple_quotes_noop_when_absent() {
        assert_eq!(fix_triple_quotes("no triples here"), "no triples here");
    }

    #[test]
    fn plain_prose_is_not_a_tool_call() {
        assert!(parse_text("I have finished the task successfully.").is_empty());
    }
}
