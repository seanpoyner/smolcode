//! Context management helpers: conversation compaction and a repo symbol map.
//!
//! Two independent utilities used to keep the model's context budget small:
//!
//! - [`compact`] folds prior `(user, assistant)` turns into a short factual
//!   "context so far" note via a single LiteForge call. Bind it to leader+c in
//!   the TUI to free context mid-session.
//! - [`repo_map`] line-scans the project for top-level definitions and emits a
//!   compact, per-file symbol index. Pure std (no LLM); inject it into the
//!   system prompt or expose it as a tool so the model can orient quickly.

use liteforge::{AsyncForgeClient, ChatCompletionRequest, Message};

/// System prompt steering the compaction summary.
const COMPACT_SYSTEM: &str = "You compress a coding session into a short \
factual context note: what was built/changed, key files, decisions, and open \
threads. Be terse, bullet points, no fluff.";

/// Cap on the rendered repo map (characters).
const MAP_CAP: usize = 4000;

/// Summarize prior (user, assistant) turns into a concise "context so far" note
/// via a single model call. Returns the summary text (empty string on error).
pub async fn compact(
    client: &AsyncForgeClient,
    model: &str,
    convo: &[(String, String)],
) -> String {
    if convo.is_empty() {
        return String::new();
    }

    let mut transcript = String::new();
    for (user, assistant) in convo {
        transcript.push_str("User: ");
        transcript.push_str(user.trim());
        transcript.push('\n');
        transcript.push_str("Assistant: ");
        transcript.push_str(assistant.trim());
        transcript.push_str("\n\n");
    }

    let messages = vec![
        Message::system(COMPACT_SYSTEM),
        Message::user(format!(
            "Compress this coding session into a context note:\n\n{transcript}"
        )),
    ];

    let mut req = ChatCompletionRequest::new(model.to_string(), messages);
    req.temperature = Some(0.2);
    req.max_tokens = Some(512);

    match client.chat_completions(req).await {
        Ok(resp) => resp
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// Build a compact symbol map of the project: walk source files
/// (.rs/.py/.js/.ts/.go/.java/.c/.cpp/.rb), line-scan for top-level definitions
/// (fn/def/class/struct/enum/impl/trait/function/interface/type), and produce:
///
/// ```text
/// path/to/file.rs
///   fn foo
///   struct Bar
/// ```
///
/// Skip junk dirs (.git, target, node_modules, __pycache__, dist, build, .venv).
/// Cap total output to ~4000 chars. Pure std, no LLM. Never panics.
pub fn repo_map(root: &std::path::Path) -> String {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    walk(root, &mut files);
    files.sort();

    let mut out = String::new();
    for path in &files {
        if out.len() >= MAP_CAP {
            break;
        }
        let symbols = scan_symbols(path);
        if symbols.is_empty() {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        let header = format!("{}\n", rel.to_string_lossy());
        if out.len() + header.len() > MAP_CAP {
            break;
        }
        out.push_str(&header);
        for sym in symbols {
            let line = format!("  {sym}\n");
            if out.len() + line.len() > MAP_CAP {
                return out;
            }
            out.push_str(&line);
        }
    }
    out
}

/// Whether a directory name should be skipped during the walk.
fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | "__pycache__" | "dist" | "build" | ".venv"
    )
}

/// Whether a file is a source file we know how to scan, by extension.
fn is_source(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs" | "py" | "js" | "ts" | "go" | "java" | "c" | "cpp" | "rb")
    )
}

/// Recursively collect source files under `dir` into `out`. Never panics.
fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => {
                let name = entry.file_name().to_string_lossy().to_string();
                if !skip_dir(&name) {
                    walk(&path, out);
                }
            }
            Ok(ft) if ft.is_file() => {
                if is_source(&path) {
                    out.push(path);
                }
            }
            _ => {}
        }
    }
}

/// Top-level definition keywords, longest-first so e.g. "interface" is matched
/// before any shorter prefix could collide.
const KEYWORDS: &[&str] = &[
    "interface", "function", "struct", "class", "trait", "impl", "enum", "type", "fn", "def",
];

/// Line-scan a file for top-level definitions and return `kw name` strings
/// (e.g. "fn foo", "struct Bar"). Returns empty on any read error.
fn scan_symbols(path: &std::path::Path) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut symbols = Vec::new();
    for raw in content.lines() {
        let mut line = raw.trim();
        // Allow common visibility/qualifier prefixes (pub, pub(crate), export,
        // async, public, etc.) ahead of the definition keyword.
        for prefix in [
            "pub(crate) ",
            "pub(super) ",
            "pub ",
            "export default ",
            "export ",
            "public ",
            "private ",
            "async ",
            "static ",
            "default ",
            "abstract ",
            "final ",
            "unsafe ",
        ] {
            if let Some(rest) = line.strip_prefix(prefix) {
                line = rest.trim_start();
            }
        }

        for kw in KEYWORDS {
            // Match "kw " as a prefix; require a following token.
            if let Some(rest) = line.strip_prefix(kw) {
                if rest.starts_with(|c: char| c.is_whitespace()) {
                    if let Some(name) = symbol_name(rest.trim_start()) {
                        symbols.push(format!("{kw} {name}"));
                    }
                    break;
                }
            }
        }
    }
    symbols
}

/// Extract the identifier token immediately after a definition keyword.
/// Stops at the first delimiter (whitespace, `(`, `<`, `{`, `:`, `;`, `=`, `,`).
/// Returns `None` if no usable identifier is present.
fn symbol_name(rest: &str) -> Option<String> {
    let name: String = rest
        .chars()
        .take_while(|c| {
            !c.is_whitespace()
                && !matches!(*c, '(' | '<' | '{' | ':' | ';' | '=' | ',' | '>' | '[' | '&')
        })
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}
