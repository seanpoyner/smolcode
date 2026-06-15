//! Dependency-free keyword retrieval over the workspace (TF-IDF-ish).
//!
//! Small models have tiny context windows, so the agent pulls only the most
//! relevant file chunks for a query instead of whole files. [`find_context`]
//! walks the repo, splits files into line-windowed chunks, and ranks them by
//! `tf * idf` over the query terms (plus a small filename/line bonus). Pure
//! std — no `regex`, no external index crate. Never panics.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const CHUNK_LINES: usize = 40;
const CHUNK_OVERLAP: usize = 5;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_CHUNK_LINES: usize = 60;
const MAX_CHUNK_CHARS: usize = 3000;
const MAX_TOTAL_CHARS: usize = 9000;
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "__pycache__",
    ".venv",
    "dist",
    "build",
];

/// A single line-window of one file.
struct Chunk {
    relpath: String,
    start: usize, // 1-based, inclusive
    end: usize,   // 1-based, inclusive
    text: String,
}

/// Lowercase and split on non-`[a-z0-9_]`, dropping empties and 1-char tokens.
///
/// Identifier-aware: `getFooBar` and `get_foo_bar` both contribute the run of
/// alphanumeric/underscore characters as tokens (we do not split camelCase
/// internally, but underscores and punctuation are boundaries).
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            cur.extend(ch.to_lowercase());
        } else if !cur.is_empty() {
            if cur.len() > 1 {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

/// True if the bytes look like text (valid UTF-8, no NUL in first 8KB).
fn looks_textual(bytes: &[u8]) -> bool {
    let probe = &bytes[..bytes.len().min(8 * 1024)];
    if probe.contains(&0) {
        return false;
    }
    std::str::from_utf8(bytes).is_ok()
}

/// Recursively collect candidate text files, skipping noise dirs and hidden dirs.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_files(&path, out);
        } else if ft.is_file() {
            match entry.metadata() {
                Ok(m) if m.len() <= MAX_FILE_BYTES => out.push(path),
                _ => continue,
            }
        }
    }
}

/// Split a file's text into overlapping line-windows.
fn chunk_file(relpath: &str, text: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let step = CHUNK_LINES.saturating_sub(CHUNK_OVERLAP).max(1);
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + CHUNK_LINES).min(lines.len());
        chunks.push(Chunk {
            relpath: relpath.to_string(),
            start: start + 1,
            end,
            text: lines[start..end].join("\n"),
        });
        if end == lines.len() {
            break;
        }
        start += step;
    }
    chunks
}

/// Retrieve the `k` most relevant chunks across the workspace for `query`.
/// Returns a formatted block: for each hit, a "relpath:start-end (score)"
/// header followed by the chunk text, separated by blank lines. Returns
/// "(no relevant context found)" when nothing scores > 0.
pub fn find_context(root: &Path, query: &str, k: usize) -> String {
    let k = k.clamp(1, 10);
    let q_tokens: Vec<String> = {
        let mut t = tokenize(query);
        t.sort();
        t.dedup();
        t
    };
    let raw_terms: Vec<String> = query
        .split_whitespace()
        .map(|s| s.to_lowercase())
        .filter(|s| s.len() > 1)
        .collect();
    if q_tokens.is_empty() {
        return "(no relevant context found)".to_string();
    }

    // 1. Gather and chunk every text file under root.
    let mut files = Vec::new();
    collect_files(root, &mut files);
    let mut chunks: Vec<Chunk> = Vec::new();
    for path in &files {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if !looks_textual(&bytes) {
            continue;
        }
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let rel = path.strip_prefix(root).unwrap_or(path);
        let relpath = rel.to_string_lossy().replace('\\', "/");
        chunks.extend(chunk_file(&relpath, &text));
    }
    let n_chunks = chunks.len();
    if n_chunks == 0 {
        return "(no relevant context found)".to_string();
    }

    // 2. Tokenize chunks once; compute df for query terms only.
    let chunk_tokens: Vec<Vec<String>> = chunks.iter().map(|c| tokenize(&c.text)).collect();
    let mut df: HashMap<&str, usize> = HashMap::new();
    for term in &q_tokens {
        df.insert(term, 0);
    }
    for toks in &chunk_tokens {
        let mut seen: Vec<&str> = Vec::new();
        for t in toks {
            if df.contains_key(t.as_str()) && !seen.contains(&t.as_str()) {
                seen.push(t.as_str());
            }
        }
        for t in seen {
            *df.get_mut(t).unwrap() += 1;
        }
    }
    let n = n_chunks as f64;
    let idf: HashMap<&str, f64> = df
        .iter()
        .map(|(t, &d)| (*t, (1.0 + n / (1.0 + d as f64)).ln()))
        .collect();

    // 3. Score each chunk: sum(tf * idf) + filename/line bonus.
    let mut scored: Vec<(f64, usize)> = Vec::with_capacity(n_chunks);
    for (i, toks) in chunk_tokens.iter().enumerate() {
        let mut tf: HashMap<&str, usize> = HashMap::new();
        for t in toks {
            if idf.contains_key(t.as_str()) {
                *tf.entry(t.as_str()).or_insert(0) += 1;
            }
        }
        let mut score = 0.0;
        for (term, &count) in &tf {
            score += count as f64 * idf[term];
        }
        if score > 0.0 {
            let relpath_l = chunks[i].relpath.to_lowercase();
            let text_l = chunks[i].text.to_lowercase();
            for raw in &raw_terms {
                if relpath_l.contains(raw.as_str()) {
                    score += 1.5;
                }
                if text_l.contains(raw.as_str()) {
                    score += 0.25;
                }
            }
        }
        scored.push((score, i));
    }

    // 4. Deterministic ordering: score desc, then relpath asc, then start asc.
    scored.retain(|(s, _)| *s > 0.0);
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| chunks[a.1].relpath.cmp(&chunks[b.1].relpath))
            .then_with(|| chunks[a.1].start.cmp(&chunks[b.1].start))
    });
    if scored.is_empty() {
        return "(no relevant context found)".to_string();
    }

    // 5. Emit the top-k, capping per-chunk and total size.
    let mut out = String::new();
    for (score, idx) in scored.into_iter().take(k) {
        let c = &chunks[idx];
        let header = format!("{}:{}-{} ({:.3})", c.relpath, c.start, c.end, score);
        let mut body: Vec<&str> = c.text.lines().take(MAX_CHUNK_LINES).collect();
        let mut truncated = body.len() < c.text.lines().count();
        let mut body_text = body.join("\n");
        if body_text.len() > MAX_CHUNK_CHARS {
            body_text.truncate(MAX_CHUNK_CHARS);
            truncated = true;
        }
        body.clear();
        let block = if truncated {
            format!("{}\n{}\n... (truncated)\n\n", header, body_text)
        } else {
            format!("{}\n{}\n\n", header, body_text)
        };
        if out.len() + block.len() > MAX_TOTAL_CHARS {
            break;
        }
        out.push_str(&block);
    }
    if out.is_empty() {
        return "(no relevant context found)".to_string();
    }
    out.truncate(out.trim_end().len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    fn temp_root() -> PathBuf {
        let mut dir = env::temp_dir();
        dir.push(format!("smolcode_rag_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn tokenize_splits_identifiers() {
        let toks = tokenize("getFooBar snake_case_thing 9px a");
        assert!(toks.contains(&"getfoobar".to_string()));
        assert!(toks.contains(&"snake_case_thing".to_string()));
        assert!(toks.contains(&"9px".to_string()));
        // 1-char "a" dropped.
        assert!(!toks.contains(&"a".to_string()));
    }

    #[test]
    fn finds_relevant_and_skips_irrelevant() {
        let root = temp_root();
        fs::write(
            root.join("payment.rs"),
            "fn process_payment() {\n    // charge the customer credit card\n    let invoice = build_invoice();\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("colors.rs"),
            "fn rainbow() {\n    let red = 1;\n    let green = 2;\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("notes.txt"),
            "the quick brown fox jumps over the lazy dog\n",
        )
        .unwrap();

        let out = find_context(&root, "payment invoice", 5);
        assert!(out.contains("payment.rs"), "got: {out}");
        assert!(!out.contains("colors.rs"), "got: {out}");
        assert!(!out.contains("notes.txt"), "got: {out}");

        let none = find_context(&root, "zzqqxx nonexistentterm", 5);
        assert_eq!(none, "(no relevant context found)");

        let _ = fs::remove_dir_all(&root);
    }
}
