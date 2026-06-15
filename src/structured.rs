//! Structured-output / JSON-constraint helpers to nudge small models toward
//! well-formed tool calls. Pure: this module only BUILDS json values and
//! prompt strings; it never sends requests. The caller attaches the produced
//! values to an OpenAI-style `response_format` / Ollama `format` field when the
//! backend supports it, and otherwise falls back to the prompt-level hint.

use serde_json::{json, Value};

/// A JSON value suitable for an OpenAI-style `response_format` requesting a
/// strict JSON object (the generic "json_object" mode).
#[allow(dead_code)] // forward API: response_format when the client supports it
pub fn json_object_format() -> Value {
    json!({ "type": "json_object" })
}

/// Build a JSON Schema (as serde_json::Value) describing a single tool call:
/// `{ "name": <one of tool_names>, "arguments": <object> }`. Useful as a
/// `response_format: {type:"json_schema", json_schema:{...}}` payload for
/// backends that support schema-constrained decoding.
#[allow(dead_code)] // forward API: response_format when the client supports it
pub fn tool_call_schema(tool_names: &[&str]) -> Value {
    // Inject the allowed tool names as a JSON `enum` for the "name" field so a
    // schema-constrained decoder can only emit a valid tool name.
    let names: Vec<Value> = tool_names.iter().map(|n| json!(n)).collect();
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "tool_call",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "enum": names },
                    "arguments": { "type": "object" }
                },
                "required": ["name", "arguments"],
                "additionalProperties": false
            }
        }
    })
}

/// A concise system-prompt addendum telling a small model to emit exactly one
/// tool call as a JSON object with "name" and "arguments" keys, listing the
/// valid tool names. Use this as the prompt-level fallback when the backend
/// does not enforce a schema.
pub fn tool_protocol_hint(tool_names: &[&str]) -> String {
    let names = tool_names.join(", ");
    format!(
        "When you need to act, respond with EXACTLY ONE tool call as a JSON \
object: {{\"name\": <tool>, \"arguments\": {{...}}}}.\n\
Valid tools: {names}.\n\
The \"arguments\" value must be a JSON object (use {{}} if none).\n\
Do not add prose, markdown, or code fences around the JSON."
    )
}

/// Heuristic: should we even try structured output for this model? Returns
/// false for models known to do native tool-calling well (so we don't
/// over-constrain them), true for ones that often need help. Match on
/// substrings, case-insensitive.
pub fn wants_constraint(model: &str) -> bool {
    let m = model.to_lowercase();
    // Models with reliable native tool_calls: granite emits proper tool_calls
    // per project notes; hosted gpt-4* and claude families are strong at
    // function-calling. Don't over-constrain these — let them call natively.
    if m.contains("granite") || m.contains("gpt-4") || m.contains("claude") {
        return false;
    }
    // Known small/open models that frequently need the nudge.
    if m.contains("qwen")
        || m.contains("coder")
        || m.contains("llama")
        || m.contains("mistral")
        || m.contains("phi")
        || m.contains("gemma")
    {
        return true;
    }
    // Default for unknown (likely small) models: help them.
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_object_format_shape() {
        assert_eq!(json_object_format(), json!({"type": "json_object"}));
    }

    #[test]
    fn tool_call_schema_has_type_and_enum() {
        let v = tool_call_schema(&["read_file", "write_file"]);
        assert_eq!(v["type"], json!("json_schema"));
        let en = &v["json_schema"]["schema"]["properties"]["name"]["enum"];
        let arr = en.as_array().expect("enum should be an array");
        assert!(arr.contains(&json!("read_file")));
        assert!(arr.contains(&json!("write_file")));
    }

    #[test]
    fn tool_protocol_hint_lists_names_and_arguments() {
        let h = tool_protocol_hint(&["read_file", "write_file"]);
        assert!(h.contains("read_file"));
        assert!(h.contains("write_file"));
        assert!(h.contains("arguments"));
    }

    #[test]
    fn wants_constraint_heuristic() {
        assert!(!wants_constraint("granite4.1:8b"));
        assert!(wants_constraint("qwen2.5-coder:14b"));
        assert!(wants_constraint("some-unknown-3b"));
        assert!(!wants_constraint("gpt-4o"));
    }
}
