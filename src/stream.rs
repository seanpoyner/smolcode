//! Streaming chat-completion helper.
//!
//! Wraps [`liteforge::AsyncForgeClient::chat_completions_stream`] and turns the
//! SSE chunk stream into two things at once:
//!
//!   1. **Live tokens** — each assistant text-content delta is forwarded on an
//!      mpsc channel so the TUI can render output token-by-token.
//!   2. **A fully-accumulated message** — the complete content string plus any
//!      tool calls, reassembled from their piecemeal streaming deltas, shaped so
//!      `agent.rs` can consume it exactly like the non-streaming `Message`.
//!
//! Streaming tool calls arrive as partial deltas spread across many chunks: a
//! given call is identified by its `index`, and its `function.arguments` is sent
//! in fragments that must be concatenated. `id` and `function.name` typically
//! appear once (in the first delta for that index) but we tolerate them arriving
//! at any point. We key partial state by index and materialize at the end.
//!
//! Everything here is defensive: a transport/decode error mid-stream ends the
//! stream early and returns whatever was accumulated so far, never panicking.

use std::collections::BTreeMap;

use liteforge::{AsyncForgeClient, ChatCompletionRequest, ToolCall};
use tokio::sync::mpsc::Sender;

// `chat_completions_stream` returns a `futures::Stream`; we need `StreamExt::next`.
use futures::StreamExt;

/// A streamed completion result: the fully-accumulated assistant message.
pub struct Streamed {
    /// The concatenation of every text-content delta.
    pub content: String,
    /// The reassembled tool calls (each with id, type="function", name, arguments).
    pub tool_calls: Vec<ToolCall>,
}

/// In-progress accumulation for a single tool call, keyed externally by index.
#[derive(Default)]
struct PartialCall {
    id: Option<String>,
    name: Option<String>,
    call_type: Option<String>,
    arguments: String,
}

/// Run a streaming chat completion.
///
/// As text content deltas arrive, each delta string is sent on `token_tx`
/// (best-effort — send errors, e.g. a dropped receiver, are ignored and do not
/// abort accumulation). The full content and any tool calls are accumulated;
/// tool-call deltas arrive piecemeal keyed by `index`, so `function.arguments`
/// fragments are appended in order and `id`/`function.name` are filled in when
/// present. Returns the assembled result.
///
/// On any error (failure to open the stream, or a decode error mid-stream) the
/// function returns whatever has been accumulated so far, which may be empty.
/// It never panics.
pub async fn run(
    client: &AsyncForgeClient,
    request: ChatCompletionRequest,
    token_tx: Sender<String>,
) -> Streamed {
    // `chat_completions_stream` forces `stream = Some(true)` itself, but set it
    // here too so intent is explicit and correct regardless of API changes.
    let mut request = request;
    request.stream = Some(true);

    let mut content = String::new();
    // BTreeMap keyed by the delta `index` so the final ordering is stable and
    // matches the order the model emitted the calls in.
    let mut partials: BTreeMap<u32, PartialCall> = BTreeMap::new();

    // Opening the stream can fail transiently (rate limit, 5xx, reset); retry
    // with bounded backoff. Mid-stream errors are not retried.
    let policy = crate::retry::Policy::default_policy();
    let opened = crate::retry::retry_async(policy, || {
        let req = request.clone();
        async { client.chat_completions_stream(req).await }
    })
    .await;
    let mut stream = match opened {
        Ok(s) => s,
        // Could not even open the stream: nothing accumulated yet.
        Err(_) => {
            return Streamed {
                content,
                tool_calls: Vec::new(),
            }
        }
    };

    while let Some(item) = stream.next().await {
        let chunk = match item {
            Ok(c) => c,
            // Mid-stream transport/decode error: stop and return what we have.
            Err(_) => break,
        };

        for choice in chunk.choices {
            let delta = choice.delta;

            // 1) Text-content delta: accumulate and forward live.
            if let Some(text) = delta.content {
                if !text.is_empty() {
                    content.push_str(&text);
                    // Best-effort; ignore send errors (receiver may be gone).
                    let _ = token_tx.send(text).await;
                }
            }

            // 2) Tool-call deltas: piecemeal, keyed by `index`.
            if let Some(tcs) = delta.tool_calls {
                for tc in tcs {
                    // Fall back to 0 when the provider omits index (common when
                    // only a single tool call is being streamed).
                    let idx = tc.index.unwrap_or(0);
                    let entry = partials.entry(idx).or_default();

                    if !tc.id.is_empty() {
                        entry.id = Some(tc.id);
                    }
                    if !tc.call_type.is_empty() {
                        entry.call_type = Some(tc.call_type);
                    }
                    if !tc.function.name.is_empty() {
                        entry.name = Some(tc.function.name);
                    }
                    // Arguments always append — they stream as fragments.
                    entry.arguments.push_str(&tc.function.arguments);
                }
            }
        }
    }

    // Materialize accumulated partials into concrete ToolCalls.
    let tool_calls = partials
        .into_iter()
        .enumerate()
        .map(|(i, (_idx, p))| {
            let id = p.id.unwrap_or_else(|| format!("call_{i}"));
            let name = p.name.unwrap_or_default();
            // ToolCall::new sets call_type = "function"; preserve a non-default
            // type if the provider sent one.
            let mut call = ToolCall::new(id, name, p.arguments);
            if let Some(t) = p.call_type {
                if !t.is_empty() {
                    call.call_type = t;
                }
            }
            call
        })
        .collect();

    Streamed {
        content,
        tool_calls,
    }
}
