//! Project-type detection: sniffs marker files in the workspace root so the
//! agent knows how to build/test/lint/format the current repo. Non-recursive,
//! never panics (unreadable files count as absent), and prefers the most
//! specific marker when several are present (Cargo.toml > package.json >
//! python > go.mod). Pure std + serde_json for package.json scripts.

use std::path::Path;

/// What we detected about the repo under the workspace root, plus the canonical
/// command for each common task. `None` means "no sensible default for this
/// project type".
#[derive(Clone, Debug, Default)]
pub struct ProjectInfo {
    pub kind: String,         // "rust" | "node" | "python" | "go" | "unknown"
    pub markers: Vec<String>, // files that identified it (Cargo.toml, ...)
    pub build: Option<String>,
    pub test: Option<String>,
    pub lint: Option<String>,
    pub format: Option<String>,
    pub run: Option<String>,
}

/// `true` if `root/name` exists.
fn has(root: &Path, name: &str) -> bool {
    root.join(name).exists()
}

/// Collect every recognized marker file actually present in `root`.
fn collect_markers(root: &Path) -> Vec<String> {
    const CANDIDATES: &[&str] = &[
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "setup.py",
        "requirements.txt",
        "setup.cfg",
        "tox.ini",
        "go.mod",
    ];
    CANDIDATES
        .iter()
        .filter(|m| has(root, m))
        .map(|m| (*m).to_string())
        .collect()
}

/// Build a node `ProjectInfo` by reading `package.json` "scripts" defensively.
/// Any parse failure falls back to npm defaults (`npm test`).
fn detect_node(root: &Path, markers: Vec<String>) -> ProjectInfo {
    let mut info = ProjectInfo {
        kind: "node".into(),
        markers,
        test: Some("npm test".into()),
        ..Default::default()
    };

    let has_script = |name: &str| -> bool {
        std::fs::read_to_string(root.join("package.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("scripts").cloned())
            .and_then(|s| s.get(name).cloned())
            .is_some()
    };

    if has_script("build") {
        info.build = Some("npm run build".into());
    }
    if has_script("lint") {
        info.lint = Some("npm run lint".into());
    }
    if has_script("format") {
        info.format = Some("npm run format".into());
    }
    if has_script("start") {
        info.run = Some("npm start".into());
    }
    info
}

/// Python: test via pytest, lint via ruff, format via black. Builds/runs have
/// no universal default. Poetry projects use `poetry run pytest`.
fn detect_python(root: &Path, markers: Vec<String>) -> ProjectInfo {
    let poetry = std::fs::read_to_string(root.join("pyproject.toml"))
        .map(|s| s.contains("[tool.poetry]"))
        .unwrap_or(false);
    ProjectInfo {
        kind: "python".into(),
        markers,
        test: Some(if poetry { "poetry run pytest" } else { "pytest" }.into()),
        lint: Some("ruff check .".into()),
        format: Some("black .".into()),
        ..Default::default()
    }
}

/// Detect the project under `root` from marker files (non-recursive: only the
/// root dir). When multiple markers exist, prefer the most specific
/// (Cargo.toml > package.json > pyproject/requirements/setup.py > go.mod).
pub fn detect(root: &Path) -> ProjectInfo {
    let markers = collect_markers(root);

    if has(root, "Cargo.toml") {
        return ProjectInfo {
            kind: "rust".into(),
            markers,
            build: Some("cargo build".into()),
            test: Some("cargo test".into()),
            lint: Some("cargo clippy".into()),
            format: Some("cargo fmt".into()),
            run: Some("cargo run".into()),
        };
    }
    if has(root, "package.json") {
        return detect_node(root, markers);
    }
    if has(root, "pyproject.toml")
        || has(root, "setup.py")
        || has(root, "requirements.txt")
        || has(root, "setup.cfg")
        || has(root, "tox.ini")
    {
        return detect_python(root, markers);
    }
    if has(root, "go.mod") {
        return ProjectInfo {
            kind: "go".into(),
            markers,
            build: Some("go build ./...".into()),
            test: Some("go test ./...".into()),
            lint: Some("go vet ./...".into()),
            format: Some("gofmt -w .".into()),
            run: Some("go run .".into()),
        };
    }

    ProjectInfo {
        kind: "unknown".into(),
        markers,
        ..Default::default()
    }
}

/// A short human summary for the model/UI, e.g.
/// "rust project (Cargo.toml) — build: cargo build, test: cargo test".
pub fn summary(info: &ProjectInfo) -> String {
    if info.kind == "unknown" {
        return "unknown project type (no build/test markers found)".into();
    }

    let markers = if info.markers.is_empty() {
        String::new()
    } else {
        format!(" ({})", info.markers.join(", "))
    };

    let mut parts = Vec::new();
    for (label, cmd) in [
        ("build", &info.build),
        ("test", &info.test),
        ("lint", &info.lint),
        ("format", &info.format),
        ("run", &info.run),
    ] {
        if let Some(c) = cmd {
            parts.push(format!("{label}: {c}"));
        }
    }

    if parts.is_empty() {
        format!("{} project{}", info.kind, markers)
    } else {
        format!("{} project{}: {}", info.kind, markers, parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// A unique-per-case temp dir under the system tmp, seeded by PID + tag.
    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("smolcode-proj-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn detects_rust() {
        let dir = tmp_dir("rust");
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let info = detect(&dir);
        assert_eq!(info.kind, "rust");
        assert_eq!(info.build.as_deref(), Some("cargo build"));
        assert_eq!(info.test.as_deref(), Some("cargo test"));
        assert_eq!(info.lint.as_deref(), Some("cargo clippy"));
        assert_eq!(info.format.as_deref(), Some("cargo fmt"));
        assert_eq!(info.run.as_deref(), Some("cargo run"));
        assert!(info.markers.contains(&"Cargo.toml".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_node_with_scripts() {
        let dir = tmp_dir("node");
        fs::write(
            dir.join("package.json"),
            r#"{"scripts":{"test":"jest","build":"webpack"}}"#,
        )
        .unwrap();
        let info = detect(&dir);
        assert_eq!(info.kind, "node");
        assert_eq!(info.test.as_deref(), Some("npm test"));
        assert_eq!(info.build.as_deref(), Some("npm run build"));
        assert_eq!(info.run, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_python() {
        let dir = tmp_dir("py");
        fs::write(dir.join("requirements.txt"), "pytest\n").unwrap();
        let info = detect(&dir);
        assert_eq!(info.kind, "python");
        assert_eq!(info.test.as_deref(), Some("pytest"));
        assert_eq!(info.lint.as_deref(), Some("ruff check ."));
        assert_eq!(info.format.as_deref(), Some("black ."));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_when_empty() {
        let dir = tmp_dir("empty");
        let info = detect(&dir);
        assert_eq!(info.kind, "unknown");
        assert!(info.markers.is_empty());
        assert!(info.build.is_none() && info.test.is_none());
        assert_eq!(summary(&info), "unknown project type (no build/test markers found)");
        let _ = fs::remove_dir_all(&dir);
    }
}
