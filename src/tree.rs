//! ASCII directory-tree renderer — gives a small model a one-glance view of
//! project layout, like the unix `tree` command. Pure std, dirs-first, sorted,
//! depth-clamped, and entry-capped so it never floods the context window.

use std::fs;
use std::path::Path;

/// Directory names skipped wholesale, plus any hidden (dotfile/dotdir) entry.
const SKIP: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "__pycache__",
    ".venv",
    "dist",
    "build",
];

/// Maximum number of entries emitted before truncating.
const MAX_ENTRIES: usize = 400;

/// Render an ASCII tree of the workspace under `root`, descending at most
/// `max_depth` levels (clamp to 1..=8, default 3). Directories first then
/// files, each sorted alphabetically; uses ├──/└──/│ connectors. Skips
/// `.git`, `target`, `node_modules`, `__pycache__`, `.venv`, `dist`, `build`,
/// and hidden entries (dotfiles/dotdirs). Caps the total number of entries
/// emitted (~400) with a trailing "… (truncated)" note when exceeded.
pub fn tree(root: &Path, max_depth: usize) -> String {
    if !root.is_dir() {
        return "(not a directory)".to_string();
    }
    let depth = max_depth.clamp(1, 8);
    let mut out = String::from(".");
    let mut count = 0usize;
    let mut truncated = false;
    walk(root, "", depth, &mut count, &mut truncated, &mut out);
    if truncated {
        out.push_str("\n… (truncated)");
    }
    out
}

/// Should this entry be skipped (hidden, or on the skip list)?
fn excluded(name: &str) -> bool {
    name.starts_with('.') || SKIP.contains(&name)
}

/// Collect the visible children of `dir`, sorted dirs-first then alphabetically
/// (case-insensitive). Returns `(name, is_dir)` pairs. `None` on read error.
fn read_sorted(dir: &Path) -> Option<Vec<(String, bool)>> {
    let mut items: Vec<(String, bool)> = Vec::new();
    for entry in fs::read_dir(dir).ok()? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if excluded(&name) {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        items.push((name, is_dir));
    }
    items.sort_by(|a, b| {
        b.1.cmp(&a.1) // dirs (true) before files (false)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
            .then_with(|| a.0.cmp(&b.0))
    });
    Some(items)
}

/// Recurse into `dir`, appending one line per visible entry to `out`.
/// `prefix` is the accumulated indentation for this level. `depth` is the
/// remaining number of levels to descend.
fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    count: &mut usize,
    truncated: &mut bool,
    out: &mut String,
) {
    if depth == 0 || *truncated {
        return;
    }
    let items = match read_sorted(dir) {
        Some(v) => v,
        None => {
            out.push_str(" [unreadable]");
            return;
        }
    };
    let last = items.len().saturating_sub(1);
    for (i, (name, is_dir)) in items.iter().enumerate() {
        if *count >= MAX_ENTRIES {
            *truncated = true;
            return;
        }
        *count += 1;
        let is_last = i == last;
        let connector = if is_last { "└── " } else { "├── " };
        out.push('\n');
        out.push_str(prefix);
        out.push_str(connector);
        out.push_str(name);
        if *is_dir {
            out.push('/');
            let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
            walk(&dir.join(name), &child_prefix, depth - 1, count, truncated, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    fn scratch() -> PathBuf {
        env::temp_dir().join(format!("smolcode-tree-test-{}", std::process::id()))
    }

    #[test]
    fn renders_tree_excludes_and_limits_depth() {
        let root = scratch();
        let _ = fs::remove_dir_all(&root);
        // Layout:
        //   root/
        //     src/main.rs
        //     top.txt
        //     .git/HEAD          (excluded)
        //     .secret            (excluded)
        //     a/b/c/deep.txt     (depth-4 file)
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("main.rs"), "fn main(){}").unwrap();
        fs::write(root.join("top.txt"), "hi").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git").join("HEAD"), "ref").unwrap();
        fs::write(root.join(".secret"), "shh").unwrap();
        fs::create_dir_all(root.join("a").join("b").join("c")).unwrap();
        fs::write(root.join("a").join("b").join("c").join("deep.txt"), "x").unwrap();

        // Deep render: everything included except excluded entries.
        let full = tree(&root, 8);
        assert!(full.starts_with("."), "root '.' line first: {full}");
        assert!(full.contains("src/"), "subdir present: {full}");
        assert!(full.contains("main.rs"), "nested file present: {full}");
        assert!(full.contains("top.txt"), "top-level file present: {full}");
        assert!(full.contains("├── ") || full.contains("└── "), "connectors: {full}");
        assert!(!full.contains(".git"), ".git excluded: {full}");
        assert!(!full.contains(".secret"), ".secret excluded: {full}");
        assert!(full.contains("deep.txt"), "deep file at full depth: {full}");

        // Depth-limited render: deepest file must be absent.
        let shallow = tree(&root, 2);
        assert!(shallow.contains("a/"), "first level dir present: {shallow}");
        assert!(!shallow.contains("deep.txt"), "depth-4 file pruned: {shallow}");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn non_directory_returns_marker() {
        assert_eq!(tree(Path::new("/no/such/path/here"), 3), "(not a directory)");
    }
}
