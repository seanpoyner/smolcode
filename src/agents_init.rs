//! Generate a starter `AGENTS.md` by scanning a repository.
//!
//! Deterministic, offline, std-only. Backs a future `/init` command:
//! it derives project guidance (languages, layout, build/test commands)
//! from filesystem signals rather than asking the model.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Directories never descended into during the extension walk, and excluded
/// from the top-level layout listing.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    ".mypy_cache",
    ".pytest_cache",
];

/// File extensions worth counting as "source" (skips lockfiles/binaries).
const SOURCE_EXTS: &[&str] = &[
    "rs", "py", "js", "ts", "tsx", "jsx", "go", "java", "c", "h", "cpp", "hpp", "cc", "rb", "php",
    "sh", "md", "toml", "yaml", "yml", "json", "html", "css", "sql", "lua", "kt", "swift", "scala",
];

const MAX_FILES: usize = 5000;

/// Build the contents of a starter AGENTS.md by scanning `root`:
/// top-level languages (by file extension counts), key directories, detected
/// marker files (Cargo.toml/package.json/etc.), and a build/test section.
pub fn generate(root: &Path) -> String {
    let mut ext_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut scanned = 0usize;
    count_exts(root, &mut ext_counts, &mut scanned);

    let markers = detect_markers(root);
    let dirs = top_dirs(root);

    let mut out = String::new();
    out.push_str("# AGENTS.md\n\n");
    out.push_str("This file guides AI coding agents working in this repository.\n");

    // ## Project
    let langs = dominant_languages(&ext_counts);
    if !langs.is_empty() || !markers.is_empty() {
        out.push_str("\n## Project\n\n");
        let mut sentence = String::new();
        if !langs.is_empty() {
            let names: Vec<&str> = langs.iter().map(|(_, n)| *n).collect();
            sentence.push_str(&format!("A {} project", join_human(&names)));
        } else {
            sentence.push_str("A project");
        }
        if !markers.is_empty() {
            let m: Vec<&str> = markers.iter().map(|s| s.as_str()).collect();
            sentence.push_str(&format!(" (markers: {})", m.join(", ")));
        }
        sentence.push('.');
        out.push_str(&sentence);
        out.push('\n');
    }

    // ## Languages
    let top = top_exts(&ext_counts, 6);
    if !top.is_empty() {
        out.push_str("\n## Languages\n\n");
        for (ext, n) in &top {
            let unit = if *n == 1 { "file" } else { "files" };
            out.push_str(&format!("- {}: {} {}\n", ext, n, unit));
        }
    }

    // ## Layout
    if !dirs.is_empty() {
        out.push_str("\n## Layout\n\n");
        for d in &dirs {
            match dir_hint(d) {
                Some(h) => out.push_str(&format!("- {}/: {}\n", d, h)),
                None => out.push_str(&format!("- {}/\n", d)),
            }
        }
    }

    // ## Build & Test
    let cmds = build_commands(&markers);
    if !cmds.is_empty() {
        out.push_str("\n## Build & Test\n\n");
        for c in &cmds {
            out.push_str(&format!("- `{}`\n", c));
        }
    }

    // ## Conventions
    out.push_str("\n## Conventions\n\n");
    out.push_str("- Match the style of the surrounding code.\n");
    out.push_str("- Run the formatter before committing.\n");
    out.push_str("- Keep changes minimal and focused.\n");
    out.push_str("- Add or update tests when changing behavior.\n");

    out
}

/// Write the generated AGENTS.md to `root/AGENTS.md`. If it already exists,
/// do NOT overwrite: return Err with a message. Returns Ok(path-string) on
/// write. (The caller decides whether to force.)
pub fn write(root: &Path) -> Result<String, String> {
    let path = root.join("AGENTS.md");
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    let body = generate(root);
    fs::write(&path, body).map_err(|e| format!("failed to write {}: {}", path.display(), e))?;
    Ok(path.display().to_string())
}

/// Recursively count source-file extensions, skipping heavy/hidden dirs and
/// capping the total files scanned. Unreadable dirs are silently skipped.
fn count_exts(dir: &Path, counts: &mut BTreeMap<String, usize>, scanned: &mut usize) {
    if *scanned >= MAX_FILES {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if *scanned >= MAX_FILES {
            return;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            count_exts(&path, counts, scanned);
        } else if ft.is_file() {
            *scanned += 1;
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext = ext.to_lowercase();
                if SOURCE_EXTS.contains(&ext.as_str()) {
                    *counts.entry(ext).or_insert(0) += 1;
                }
            }
        }
    }
}

/// Detect well-known marker files at the repo root.
fn detect_markers(root: &Path) -> Vec<String> {
    const MARKERS: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "requirements.txt",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "Gemfile",
        "composer.json",
        "Makefile",
    ];
    MARKERS
        .iter()
        .filter(|m| root.join(m).exists())
        .map(|m| m.to_string())
        .collect()
}

/// Top-level non-hidden, non-skipped directories, sorted alphabetically.
fn top_dirs(root: &Path) -> Vec<String> {
    let mut dirs = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                if !ft.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                dirs.push(name);
            }
        }
    }
    dirs.sort();
    dirs
}

/// Map an extension to a human language name.
fn lang_name(ext: &str) -> &'static str {
    match ext {
        "rs" => "Rust",
        "py" => "Python",
        "js" | "jsx" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "go" => "Go",
        "java" => "Java",
        "c" | "h" => "C",
        "cpp" | "hpp" | "cc" => "C++",
        "rb" => "Ruby",
        "php" => "PHP",
        "kt" => "Kotlin",
        "swift" => "Swift",
        "scala" => "Scala",
        "lua" => "Lua",
        _ => "",
    }
}

/// Up to two dominant programming languages (by ext count), as (ext, name).
fn dominant_languages(counts: &BTreeMap<String, usize>) -> Vec<(String, &'static str)> {
    let mut langs: Vec<(String, usize, &'static str)> = counts
        .iter()
        .filter_map(|(ext, n)| {
            let name = lang_name(ext);
            if name.is_empty() {
                None
            } else {
                Some((ext.clone(), *n, name))
            }
        })
        .collect();
    langs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    langs
        .into_iter()
        .take(2)
        .map(|(ext, _, name)| (ext, name))
        .collect()
}

/// Top `limit` extensions by count desc, then name asc.
fn top_exts(counts: &BTreeMap<String, usize>, limit: usize) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(limit);
    v
}

/// A one-word hint for common directory names.
fn dir_hint(name: &str) -> Option<&'static str> {
    match name {
        "src" => Some("source"),
        "tests" | "test" => Some("tests"),
        "docs" | "doc" => Some("docs"),
        "scripts" | "bin" => Some("scripts"),
        "examples" => Some("examples"),
        "benches" => Some("benchmarks"),
        _ => None,
    }
}

/// Infer build/test commands from detected markers (no cross-module deps).
fn build_commands(markers: &[String]) -> Vec<String> {
    let has = |m: &str| markers.iter().any(|x| x == m);
    let mut cmds = Vec::new();
    if has("Cargo.toml") {
        cmds.push("cargo build".to_string());
        cmds.push("cargo test".to_string());
        cmds.push("cargo fmt".to_string());
    }
    if has("package.json") {
        cmds.push("npm install".to_string());
        cmds.push("npm test".to_string());
    }
    if has("pyproject.toml") || has("requirements.txt") {
        cmds.push("pytest".to_string());
    }
    if has("go.mod") {
        cmds.push("go build ./...".to_string());
        cmds.push("go test ./...".to_string());
    }
    cmds
}

/// Join names as "a", "a and b", or "a, b and c".
fn join_human(names: &[&str]) -> String {
    match names.len() {
        0 => String::new(),
        1 => names[0].to_string(),
        2 => format!("{} and {}", names[0], names[1]),
        _ => {
            let head = names[..names.len() - 1].join(", ");
            format!("{} and {}", head, names[names.len() - 1])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    fn fixture() -> std::path::PathBuf {
        let mut dir = env::temp_dir();
        dir.push(format!("smolcode_agents_init_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        fs::write(dir.join("src").join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("README.md"), "# x\n").unwrap();
        dir
    }

    #[test]
    fn generates_and_writes() {
        let dir = fixture();

        let md = generate(&dir);
        assert!(md.contains("# AGENTS.md"), "title present");
        assert!(md.contains("cargo test"), "cargo test inferred");
        assert!(md.contains("src"), "src layout present");
        assert!(md.contains("rs:"), "rust language line present");

        let first = write(&dir);
        assert!(first.is_ok(), "first write succeeds: {:?}", first);
        let second = write(&dir);
        assert!(second.is_err(), "second write refuses to overwrite");

        let _ = fs::remove_dir_all(&dir);
    }
}
