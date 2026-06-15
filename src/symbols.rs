//! Dependency-free per-language symbol extractor + file/repo outline.
//!
//! Helps a small model orient by listing the functions/classes/etc. in a file
//! or across the repo, using simple line-based prefix/keyword scanning (no
//! tree-sitter, no regex). Dispatches on file extension; unknown extensions
//! yield nothing. Never panics — every fallible path returns an empty Vec or a
//! human/model-readable string.

use std::path::Path;

/// A single source symbol with its 1-based line number.
#[derive(Clone, Debug, PartialEq)]
pub struct Symbol {
    pub kind: String, // "fn" | "class" | "struct" | "enum" | "trait" | "impl" | "const" | "type" | "import"
    pub name: String,
    pub line: usize, // 1-based
}

impl Symbol {
    fn new(kind: &str, name: impl Into<String>, line: usize) -> Self {
        Symbol { kind: kind.into(), name: name.into(), line }
    }
}

/// Extract top-level symbols from a single file's source, dispatching on the
/// file extension. Unknown extensions return an empty Vec.
pub fn extract(path: &str, source: &str) -> Vec<Symbol> {
    match ext_of(path).as_str() {
        "rs" => rust(source),
        "py" => python(source),
        "js" | "ts" | "jsx" | "tsx" => jsts(source),
        "go" => go(source),
        _ => Vec::new(),
    }
}

/// A human-readable outline of one workspace file: `"<line>: <kind> <name>"`
/// lines. Reads the file under `root`. `"(no symbols found)"` if none; an error
/// string if the file can't be read.
pub fn outline(root: &Path, rel_file: &str) -> String {
    let full = root.join(rel_file);
    let source = match std::fs::read_to_string(&full) {
        Ok(s) => s,
        Err(e) => return format!("error reading {rel_file}: {e}"),
    };
    let syms = extract(rel_file, &source);
    if syms.is_empty() {
        return "(no symbols found)".to_string();
    }
    syms.iter()
        .map(|s| format!("{}: {} {}", s.line, s.kind, s.name))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Find a symbol by exact name across the repo (skipping heavy/binary dirs).
/// Returns `"relpath:line: <kind> <name>"` lines for every match, or
/// `"(symbol '<name>' not found)"`.
pub fn find(root: &Path, name: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    walk(root, root, name, &mut out);
    if out.is_empty() {
        format!("(symbol '{name}' not found)")
    } else {
        out.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Repo walk
// ---------------------------------------------------------------------------

const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_MATCHES: usize = 60;

fn walk(root: &Path, dir: &Path, name: &str, out: &mut Vec<String>) {
    if out.len() >= MAX_MATCHES {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_MATCHES {
            return;
        }
        let path = entry.path();
        let fname = entry.file_name();
        let fname = fname.to_string_lossy();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            if is_skip_dir(&fname) {
                continue;
            }
            walk(root, &path, name, out);
        } else if ft.is_file() {
            // size + binary guards
            match entry.metadata() {
                Ok(m) if m.len() <= MAX_FILE_BYTES => {}
                _ => continue,
            }
            let source = match read_text(&path) {
                Some(s) => s,
                None => continue,
            };
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().to_string();
            for s in extract(&rel, &source) {
                if s.name == name {
                    out.push(format!("{rel}:{}: {} {}", s.line, s.kind, s.name));
                    if out.len() >= MAX_MATCHES {
                        return;
                    }
                }
            }
        }
    }
}

fn is_skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | "__pycache__" | ".venv"
    ) || name.starts_with('.')
}

/// Read a file as UTF-8, rejecting binaries (NUL byte in the first 8KB).
fn read_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let probe = &bytes[..bytes.len().min(8 * 1024)];
    if probe.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

// ---------------------------------------------------------------------------
// Identifier scanning helpers
// ---------------------------------------------------------------------------

fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Find `keyword` as a whole word in `line`, then collect the following
/// identifier (skipping intervening whitespace). Returns the identifier.
fn ident_after(line: &str, keyword: &str) -> Option<String> {
    let pos = word_pos(line, keyword)?;
    let rest = &line[pos + keyword.len()..];
    collect_ident(rest)
}

/// Locate `keyword` as a whole word; returns its byte offset.
fn word_pos(line: &str, keyword: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(rel) = line[from..].find(keyword) {
        let start = from + rel;
        let end = start + keyword.len();
        let before_ok = start == 0 || !is_ident(bytes[start - 1] as char);
        let after_ok = end >= bytes.len() || !is_ident(bytes[end] as char);
        if before_ok && after_ok {
            return Some(start);
        }
        from = start + 1;
    }
    None
}

/// Skip leading whitespace, then collect a run of identifier characters.
fn collect_ident(s: &str) -> Option<String> {
    let s = s.trim_start();
    let ident: String = s.chars().take_while(|&c| is_ident(c)).collect();
    if ident.is_empty() {
        None
    } else {
        Some(ident)
    }
}

/// Strip leading visibility/qualifier keywords from a Rust line.
fn strip_rust_vis(line: &str) -> &str {
    let mut l = line.trim_start();
    loop {
        let trimmed = if let Some(r) = l.strip_prefix("pub(crate)") {
            r
        } else if let Some(r) = l.strip_prefix("pub(super)") {
            r
        } else if let Some(r) = l.strip_prefix("pub") {
            r
        } else if let Some(r) = l.strip_prefix("default") {
            r
        } else if let Some(r) = l.strip_prefix("unsafe") {
            r
        } else if let Some(r) = l.strip_prefix("async") {
            r
        } else {
            break;
        };
        // Only consume if it was a real token boundary.
        if trimmed.starts_with(|c: char| is_ident(c)) && !trimmed.starts_with(' ') {
            break;
        }
        l = trimmed.trim_start();
    }
    l
}

// ---------------------------------------------------------------------------
// Per-language extractors
// ---------------------------------------------------------------------------

fn rust(source: &str) -> Vec<Symbol> {
    let mut out = Vec::new();
    for (i, raw) in source.lines().enumerate() {
        let line = i + 1;
        let l = strip_rust_vis(raw);
        if l.starts_with("use ") {
            let path = l["use ".len()..]
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_string();
            if !path.is_empty() {
                out.push(Symbol::new("import", path, line));
            }
            continue;
        }
        if let Some(n) = ident_after(l, "fn") {
            if starts_kw(l, "fn") {
                out.push(Symbol::new("fn", n, line));
                continue;
            }
        }
        if starts_kw(l, "struct") {
            if let Some(n) = ident_after(l, "struct") {
                out.push(Symbol::new("struct", n, line));
                continue;
            }
        }
        if starts_kw(l, "enum") {
            if let Some(n) = ident_after(l, "enum") {
                out.push(Symbol::new("enum", n, line));
                continue;
            }
        }
        if starts_kw(l, "trait") {
            if let Some(n) = ident_after(l, "trait") {
                out.push(Symbol::new("trait", n, line));
                continue;
            }
        }
        if starts_kw(l, "impl") {
            // `impl ... for NAME` -> NAME, else `impl NAME` -> NAME.
            let name = ident_after(l, "for").or_else(|| ident_after(l, "impl"));
            if let Some(n) = name {
                out.push(Symbol::new("impl", n, line));
                continue;
            }
        }
        if starts_kw(l, "const") {
            if let Some(n) = ident_after(l, "const") {
                out.push(Symbol::new("const", n, line));
                continue;
            }
        }
        if starts_kw(l, "static") {
            if let Some(n) = ident_after(l, "static") {
                out.push(Symbol::new("const", n, line));
                continue;
            }
        }
        if starts_kw(l, "type") {
            if let Some(n) = ident_after(l, "type") {
                out.push(Symbol::new("type", n, line));
                continue;
            }
        }
    }
    out
}

/// True if `l` (after vis-stripping) begins with `kw` as a whole word.
fn starts_kw(l: &str, kw: &str) -> bool {
    match l.strip_prefix(kw) {
        Some(rest) => rest.is_empty() || !rest.starts_with(|c: char| is_ident(c)),
        None => false,
    }
}

fn python(source: &str) -> Vec<Symbol> {
    let mut out = Vec::new();
    for (i, raw) in source.lines().enumerate() {
        let line = i + 1;
        let l = raw.trim_start();
        if let Some(rest) = l.strip_prefix("async def ").or_else(|| l.strip_prefix("def ")) {
            if let Some(n) = collect_ident(rest) {
                out.push(Symbol::new("fn", n, line));
            }
        } else if let Some(rest) = l.strip_prefix("class ") {
            if let Some(n) = collect_ident(rest) {
                out.push(Symbol::new("class", n, line));
            }
        } else if let Some(rest) = l.strip_prefix("from ") {
            if let Some(n) = collect_dotted(rest) {
                out.push(Symbol::new("import", n, line));
            }
        } else if let Some(rest) = l.strip_prefix("import ") {
            if let Some(n) = collect_dotted(rest) {
                out.push(Symbol::new("import", n, line));
            }
        }
    }
    out
}

/// Collect a dotted module path (`a.b.c`), stopping at whitespace/comma.
fn collect_dotted(s: &str) -> Option<String> {
    let s = s.trim_start();
    let path: String = s
        .chars()
        .take_while(|&c| is_ident(c) || c == '.')
        .collect();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn jsts(source: &str) -> Vec<Symbol> {
    let mut out = Vec::new();
    for (i, raw) in source.lines().enumerate() {
        let line = i + 1;
        // Drop a leading `export ` (and `export default `) to expose the inner symbol.
        let mut l = raw.trim_start();
        if let Some(rest) = l.strip_prefix("export ") {
            l = rest.trim_start();
            if let Some(rest2) = l.strip_prefix("default ") {
                l = rest2.trim_start();
            }
        }

        if l.starts_with("import ") {
            // `import ... from 'mod'` -> the module specifier.
            if let Some(idx) = l.find(" from ") {
                let spec = l[idx + 6..]
                    .trim()
                    .trim_end_matches(';')
                    .trim()
                    .trim_matches(|c| c == '\'' || c == '"' || c == '`')
                    .to_string();
                if !spec.is_empty() {
                    out.push(Symbol::new("import", spec, line));
                    continue;
                }
            }
        }
        if starts_kw(l, "function") || starts_kw(l, "async") && l.contains("function") {
            if let Some(n) = ident_after(l, "function") {
                out.push(Symbol::new("fn", n, line));
                continue;
            }
        }
        if starts_kw(l, "class") {
            if let Some(n) = ident_after(l, "class") {
                out.push(Symbol::new("class", n, line));
                continue;
            }
        }
        // const/let/var NAME = (..)=>.. | function | arrow
        for kw in ["const", "let", "var"] {
            if starts_kw(l, kw) {
                if let Some(n) = ident_after(l, kw) {
                    if l.contains("=>") || l.contains("= function") || l.contains("=(") || l.contains("= (") {
                        out.push(Symbol::new("fn", n, line));
                    }
                }
                break;
            }
        }
    }
    out
}

fn go(source: &str) -> Vec<Symbol> {
    let mut out = Vec::new();
    for (i, raw) in source.lines().enumerate() {
        let line = i + 1;
        let l = raw.trim_start();
        if starts_kw(l, "func") {
            // `func NAME(` or `func (recv) NAME(`.
            let after = &l["func".len()..];
            let after = after.trim_start();
            let name = if after.starts_with('(') {
                // receiver present: skip to the closing paren.
                after.find(')').and_then(|p| collect_ident(&after[p + 1..]))
            } else {
                collect_ident(after)
            };
            if let Some(n) = name {
                out.push(Symbol::new("fn", n, line));
            }
            continue;
        }
        if starts_kw(l, "type") {
            if let Some(n) = ident_after(l, "type") {
                if l.contains("interface") {
                    out.push(Symbol::new("trait", n, line));
                } else if l.contains("struct") {
                    out.push(Symbol::new("struct", n, line));
                } else {
                    out.push(Symbol::new("type", n, line));
                }
            }
            continue;
        }
        if starts_kw(l, "import") {
            // single-line `import "pkg"` or `import alias "pkg"`.
            if let Some(start) = l.find('"') {
                if let Some(end) = l[start + 1..].find('"') {
                    let spec = &l[start + 1..start + 1 + end];
                    if !spec.is_empty() {
                        out.push(Symbol::new("import", spec.to_string(), line));
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_symbols() {
        let src = "\
use std::collections::HashMap;
pub struct Foo {
    x: i32,
}
enum Color { Red }
pub async fn run(n: usize) -> usize { n }
impl Display for Foo {}
const MAX: usize = 10;
type Alias = Foo;";
        let syms = extract("x.rs", src);
        assert_eq!(syms[0], Symbol::new("import", "std::collections::HashMap", 1));
        assert_eq!(syms[1], Symbol::new("struct", "Foo", 2));
        assert_eq!(syms[2], Symbol::new("enum", "Color", 5));
        assert_eq!(syms[3], Symbol::new("fn", "run", 6));
        assert_eq!(syms[4], Symbol::new("impl", "Foo", 7));
        assert_eq!(syms[5], Symbol::new("const", "MAX", 8));
        assert_eq!(syms[6], Symbol::new("type", "Alias", 9));
    }

    #[test]
    fn python_symbols() {
        let src = "\
import os
from typing import List
class Widget:
    def method(self):
        pass

def top(x):
    return x";
        let syms = extract("m.py", src);
        assert_eq!(syms[0], Symbol::new("import", "os", 1));
        assert_eq!(syms[1], Symbol::new("import", "typing", 2));
        assert_eq!(syms[2], Symbol::new("class", "Widget", 3));
        assert_eq!(syms[3], Symbol::new("fn", "method", 4));
        assert_eq!(syms[4], Symbol::new("fn", "top", 7));
    }

    #[test]
    fn unknown_extension_is_empty() {
        assert!(extract("data.bin", "anything here\nfn nope() {}").is_empty());
        assert!(extract("noext", "fn nope() {}").is_empty());
    }

    #[test]
    fn find_and_outline_roundtrip() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("smolcode_symbols_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();

        std::fs::write(dir.join("a.rs"), "pub fn alpha() {}\nstruct Beta;").unwrap();
        std::fs::write(dir.join("sub").join("b.py"), "def alpha():\n    pass").unwrap();

        let found = find(&dir, "alpha");
        assert!(found.contains("a.rs:1: fn alpha"), "got: {found}");
        assert!(found.contains("alpha"));
        assert!(find(&dir, "nope_nope").contains("not found"));

        let out = outline(&dir, "a.rs");
        assert!(out.contains("1: fn alpha"), "got: {out}");
        assert!(out.contains("2: struct Beta"), "got: {out}");
        assert_eq!(outline(&dir, "sub/b.py"), "1: fn alpha");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
