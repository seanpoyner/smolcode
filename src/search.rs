//! Recursive content search (grep-like), std-only, workspace-confined.

use std::path::Path;

const MAX_TOTAL: usize = 80;
const MAX_PER_FILE: usize = 20;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const SNIFF_BYTES: usize = 8192;

const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "__pycache__", ".venv"];

/// Recursively search files under `root` for lines containing `pattern`.
/// Returns a compact "relpath:line: <trimmed line>" listing, capped, with a
/// trailing "(N more matches)" note if truncated. Skips binary-looking files,
/// hidden dirs, and common heavy dirs (.git, target, node_modules, __pycache__, .venv).
pub fn search(root: &Path, pattern: &str, case_insensitive: bool) -> String {
    if pattern.is_empty() {
        return "search: empty pattern".to_string();
    }
    let needle = if case_insensitive { pattern.to_lowercase() } else { pattern.to_string() };
    let mut out: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    walk(root, root, &needle, case_insensitive, &mut out, &mut total, &mut truncated);

    if out.is_empty() {
        return format!("no matches for '{pattern}'");
    }
    let mut s = out.join("\n");
    if truncated && total > out.len() {
        s.push_str(&format!("\n({} more matches)", total - out.len()));
    }
    s
}

fn walk(
    root: &Path,
    dir: &Path,
    needle: &str,
    ci: bool,
    out: &mut Vec<String>,
    total: &mut usize,
    truncated: &mut bool,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_TOTAL {
            *truncated = true;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') && name != "." {
            // hidden file or dir
            if path.is_dir() {
                continue;
            }
        }
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) || name.starts_with('.') {
                continue;
            }
            walk(root, &path, needle, ci, out, total, truncated);
        } else if ft.is_file() {
            scan_file(root, &path, needle, ci, out, total, truncated);
        }
    }
}

fn scan_file(
    root: &Path,
    path: &Path,
    needle: &str,
    ci: bool,
    out: &mut Vec<String>,
    total: &mut usize,
    truncated: &mut bool,
) {
    if let Ok(meta) = path.metadata() {
        if meta.len() > MAX_FILE_BYTES {
            return;
        }
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return,
    };
    // binary sniff: NUL in first chunk
    let sniff = &bytes[..bytes.len().min(SNIFF_BYTES)];
    if sniff.contains(&0u8) {
        return;
    }
    let text = match std::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(_) => return,
    };
    let rel = path.strip_prefix(root).unwrap_or(path);
    let rel = rel.to_string_lossy();
    let mut per_file = 0usize;
    for (i, line) in text.lines().enumerate() {
        let hay = if ci { line.to_lowercase() } else { line.to_string() };
        if hay.contains(needle) {
            *total += 1;
            if per_file >= MAX_PER_FILE || out.len() >= MAX_TOTAL {
                *truncated = true;
                continue;
            }
            out.push(format!("{}:{}: {}", rel, i + 1, line.trim()));
            per_file += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("smolcode_search_{}_{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_a_match() {
        let d = tmp_dir("find");
        fs::write(d.join("a.txt"), "hello world\nfoo bar\n").unwrap();
        fs::write(d.join("b.txt"), "nothing here\n").unwrap();
        let res = search(&d, "foo", false);
        assert!(res.contains("a.txt:2: foo bar"), "got: {res}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn no_matches() {
        let d = tmp_dir("none");
        fs::write(d.join("a.txt"), "alpha\nbeta\n").unwrap();
        let res = search(&d, "zzzz", false);
        assert_eq!(res, "no matches for 'zzzz'");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn empty_pattern() {
        let d = tmp_dir("empty");
        let res = search(&d, "", false);
        assert_eq!(res, "search: empty pattern");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn case_insensitive_match() {
        let d = tmp_dir("ci");
        fs::write(d.join("c.txt"), "Hello World\n").unwrap();
        let res = search(&d, "hello", true);
        assert!(res.contains("c.txt:1: Hello World"), "got: {res}");
        let _ = fs::remove_dir_all(&d);
    }
}
