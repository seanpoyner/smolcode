//! `format_file` tool: run the appropriate language formatter on a file in
//! place, workspace-confined, std-only (shells out via `std::process::Command`).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The formatter command for a file extension, if known.
///
/// Returns the program name plus its *fixed* (non-path) arguments. The caller
/// appends the absolute file path as the final argument. E.g. `"rs"` ->
/// `Some(("rustfmt", []))`, `"py"` -> `Some(("black", ["-q"]))`.
pub fn formatter_for(ext: &str) -> Option<(&'static str, Vec<String>)> {
    let mk = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match ext {
        "rs" => Some(("rustfmt", mk(&[]))),
        "py" => Some(("black", mk(&["-q"]))),
        "js" | "jsx" | "ts" | "tsx" | "json" | "css" | "md" => {
            Some(("prettier", mk(&["--write"])))
        }
        "go" => Some(("gofmt", mk(&["-w"]))),
        // `toml` intentionally has no formatter (would need `taplo`, not assumed).
        _ => None,
    }
}

/// Resolve `rel` under `root`, rejecting `..` escapes. Mirrors `tools.rs`.
fn resolve(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let p = canon_root.join(rel);
    let abs = if p.exists() {
        p.canonicalize().map_err(|e| format!("cannot resolve {rel}: {e}"))?
    } else {
        p
    };
    if !abs.starts_with(&canon_root) {
        return Err(format!("path escapes workspace: {rel}"));
    }
    Ok(abs)
}

/// Look up `prog` on `$PATH`, returning true if an executable file is found.
fn on_path(prog: &str) -> bool {
    // An explicit path (contains a separator) is checked directly.
    if prog.contains('/') {
        return is_executable(Path::new(prog));
    }
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(prog)))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

fn lower_ext(abs: &Path) -> Option<String> {
    abs.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Format the file at `path` (relative to `root`) with the right formatter for
/// its extension, in place. Returns a summary (formatter used + whether the
/// file changed). If no formatter is known/installed, returns a clear message
/// and does NOT modify the file. Never panics.
pub fn format_file(root: &Path, path: &str) -> String {
    let abs = match resolve(root, path) {
        Ok(a) => a,
        Err(e) => return format!("error: {e}"),
    };
    if !abs.is_file() {
        return format!("error: no such file: {path}");
    }

    let ext = match lower_ext(&abs) {
        Some(e) => e,
        None => return format!("(no formatter configured for {path}: no extension)"),
    };

    let (prog, fixed_args) = match formatter_for(&ext) {
        Some(f) => f,
        None => return format!("(no formatter configured for .{ext} files)"),
    };

    if !on_path(prog) {
        return format!("(formatter '{prog}' is not installed; skipped)");
    }

    let before = std::fs::read(&abs).unwrap_or_default();

    let output = Command::new(prog)
        .args(&fixed_args)
        .arg(&abs)
        .current_dir(root)
        .output();

    let output = match output {
        Ok(o) => o,
        Err(e) => return format!("formatter {prog} failed to launch: {e}"),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first: Vec<&str> = stderr.lines().take(3).collect();
        let msg = first.join(" | ");
        return format!("formatter {prog} failed: {msg}");
    }

    let after = std::fs::read(&abs).unwrap_or_default();
    let suffix = if before == after { " (no changes)" } else { " (reformatted)" };
    format!("formatted {path} with {prog}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_map_to_formatters() {
        assert!(matches!(formatter_for("rs"), Some(("rustfmt", _))));
        assert!(matches!(formatter_for("py"), Some(("black", _))));
        assert!(matches!(formatter_for("ts"), Some(("prettier", _))));
        assert!(matches!(formatter_for("go"), Some(("gofmt", _))));
        assert!(formatter_for("xyz").is_none());
        assert!(formatter_for("toml").is_none());
    }

    #[test]
    fn prettier_carries_write_flag() {
        let (prog, args) = formatter_for("json").unwrap();
        assert_eq!(prog, "prettier");
        assert_eq!(args, vec!["--write".to_string()]);
    }

    #[test]
    fn unknown_extension_leaves_file_untouched() {
        let dir = std::env::temp_dir().join(format!("smolfmt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = "sample.xyz";
        let file = dir.join(name);
        let contents = b"unformatted   junk\n\n";
        std::fs::write(&file, contents).unwrap();

        let result = format_file(&dir, name);
        assert!(
            result.contains("no formatter configured"),
            "unexpected result: {result}"
        );
        assert_eq!(std::fs::read(&file).unwrap(), contents);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn known_extension_yields_acceptable_message() {
        // rustfmt may or may not be installed; assert only that the outcome is
        // one of the acceptable, environment-independent messages.
        let dir = std::env::temp_dir().join(format!("smolfmt-rs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let name = "sample.rs";
        let file = dir.join(name);
        std::fs::write(&file, b"fn main(){}\n").unwrap();

        let result = format_file(&dir, name);
        let ok = result.starts_with("formatted ")
            || result.contains("is not installed")
            || result.contains("formatter rustfmt failed");
        assert!(ok, "unexpected result: {result}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
