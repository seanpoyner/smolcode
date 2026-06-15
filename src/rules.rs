//! Always-on project rules. Markdown files under `~/.config/smolcode/rules/`
//! (user) and `<root>/.smolcode/rules/` (project) are loaded and layered into
//! the system prompt alongside AGENTS.md. Project rules come after user rules.
//!
//! An optional `--- … ---` YAML-ish frontmatter block is stripped from the body;
//! a `description:` line in it is surfaced for `/rules`.

use std::path::Path;

/// Total character budget for rule text injected into one system prompt. A
/// backstop against rule sprawl, not a tight cap — relevance (globs) does the
/// real context-saving, so this is generous enough that a normal curated set of
/// applicable rules never gets dropped, while still bounding pathological cases.
pub const RULE_BUDGET: usize = 4000;

#[derive(Debug, Clone)]
pub struct Rule {
    /// File stem (e.g. `style` for `style.md`).
    pub name: String,
    /// "user" or "project" — where the rule came from.
    pub scope: &'static str,
    /// Optional one-line description from frontmatter.
    pub description: Option<String>,
    /// Optional relevance triggers from frontmatter `globs:` (e.g. `*.py`, `src/`).
    /// Empty = the rule is always applied; non-empty = applied only when the task
    /// context matches one of these, so we don't waste context on irrelevant rules.
    pub globs: Vec<String>,
    /// The rule text (frontmatter stripped).
    pub body: String,
}

impl Rule {
    /// Whether this rule is relevant to `ctx` (a lowercased task string). Rules
    /// with no globs always apply; a globbed rule applies only when a token
    /// derived from one of its globs appears in `ctx`.
    pub fn applies(&self, ctx_lower: &str) -> bool {
        if self.globs.is_empty() {
            return true;
        }
        self.globs.iter().any(|g| {
            for tok in glob_tokens(g) {
                if !tok.is_empty() && ctx_lower.contains(&tok) {
                    return true;
                }
            }
            false
        })
    }
}

/// Derive match tokens from a glob: the bare extension (`*.py` -> `.py`, `py`)
/// plus the language word for common extensions, or the literal minus `*`.
fn glob_tokens(glob: &str) -> Vec<String> {
    let g = glob.trim().to_lowercase();
    let mut toks = Vec::new();
    if let Some(ext) = g.strip_prefix("*.") {
        toks.push(format!(".{ext}"));
        toks.push(ext.to_string());
        let lang = match ext {
            "py" => "python",
            "js" | "jsx" | "mjs" | "cjs" => "javascript",
            "ts" | "tsx" => "typescript",
            "rs" => "rust",
            "go" => "golang",
            "sh" | "bash" => "shell",
            "md" => "markdown",
            other => other,
        };
        toks.push(lang.to_string());
    } else {
        toks.push(g.trim_matches('*').to_string());
    }
    toks
}

/// Select the rules to inject for a task, in order: relevant ones first, capped
/// to [`RULE_BUDGET`] characters. Returns the chosen rules and how many were
/// dropped (filtered out or over budget).
pub fn select<'a>(rules: &'a [Rule], task: &str, budget: usize) -> (Vec<&'a Rule>, usize) {
    let ctx = task.to_lowercase();
    let mut chosen: Vec<&Rule> = Vec::new();
    let mut used = 0usize;
    let mut dropped = 0usize;
    for r in rules {
        if !r.applies(&ctx) {
            dropped += 1;
            continue;
        }
        let cost = r.body.trim().len() + 2;
        if used + cost > budget && !chosen.is_empty() {
            dropped += 1;
            continue;
        }
        used += cost;
        chosen.push(r);
    }
    (chosen, dropped)
}

/// The two rule roots, user first so project rules win / append last.
fn rule_dirs(root: &Path) -> Vec<(std::path::PathBuf, &'static str)> {
    let mut v = Vec::new();
    if let Some(c) = dirs::config_dir() {
        v.push((c.join("smolcode").join("rules"), "user"));
    }
    v.push((root.join(".smolcode").join("rules"), "project"));
    v
}

/// Load all rules: built-in `[system]` defaults first, then `[user]`
/// (`~/.config/smolcode/rules/`), then `[project]` (`<root>/.smolcode/rules/`).
/// A later scope overrides an earlier one by name, so a user or project rule can
/// replace — or, with an empty body, disable — a system rule. Sorted by name.
pub fn load(root: &Path) -> Vec<Rule> {
    let mut out: Vec<Rule> = system_rules();
    for (dir, scope) in rule_dirs(root) {
        let mut here = read_dir_rules(&dir, scope);
        for r in here.drain(..) {
            out.retain(|x| x.name != r.name); // later scope wins
            out.push(r);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Read `*.md` rules from one directory, sorted by name. Missing dir -> empty.
fn read_dir_rules(dir: &Path, scope: &'static str) -> Vec<Rule> {
    let mut here: Vec<Rule> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map_or(false, |x| x == "md") {
                if let (Some(stem), Ok(raw)) = (
                    p.file_stem().map(|s| s.to_string_lossy().to_string()),
                    std::fs::read_to_string(&p),
                ) {
                    let (description, globs, body) = parse_meta(&raw);
                    here.push(Rule { name: stem, scope, description, globs, body });
                }
            }
        }
    }
    here.sort_by(|a, b| a.name.cmp(&b.name));
    here
}

/// One built-in system rule: `(name, description, globs, body)`.
type SysRule = (&'static str, &'static str, &'static [&'static str], &'static str);

/// smolcode's built-in agentic-discipline + language rules. These ship in the
/// binary so every install behaves well out of the box; users/projects can
/// override them by name. Kept terse — they ride in every relevant prompt.
const SYSTEM: &[SysRule] = &[
    ("no-hallucinated-apis", "never reference code that isn't confirmed to exist", &[],
     "Never reference a function, method, import, file path, CLI flag, or config key you have not confirmed exists in THIS codebase. Before you use a symbol, verify it with find_symbol, search, or read_file. If you cannot confirm it, check first — do not guess signatures, argument names, return types, or module paths."),
    ("edit-surgically", "edit existing files in place, don't blind-rewrite them", &[],
     "To modify an existing file, change only the relevant lines with str_replace, multi_edit, or apply_patch. Reserve write_file for files you are creating from scratch or deliberately rewriting in full. Never overwrite a whole file just to change a few lines — you will drop or corrupt the rest."),
    ("update-all-callers", "keep multi-file changes consistent", &[],
     "When you rename or change the signature or behavior of a function, type, constant, or exported name, first search the whole repo for every place it is used, and update all of them in the same task. A change that works in one file but breaks its callers is not done."),
    ("minimal-scope", "do exactly what was asked, nothing extra", &[],
     "Do exactly what was asked and no more. Do not add features, abstractions, dependencies, configuration, or files that were not requested, and do not refactor unrelated code. If you notice something else worth doing, mention it in one line at the end instead of doing it."),
    ("search-then-broaden", "don't conclude something is absent after one narrow search", &[],
     "If a search or grep returns nothing, do not conclude the thing is absent. Broaden the query: try shorter or partial terms, synonyms, different casing, and use repo_map / tree / list_dir to orient. Only state that something does not exist after a genuinely broad look."),
    ("python", "Python project conventions", &["*.py"],
     "Python: prefer the standard library; add a third-party dependency only when clearly necessary. If the project has a .venv/venv, use its interpreter (run_python already does). Match the project's existing style and type-hint public functions. If a formatter/linter is configured (ruff, black, flake8), keep the code passing it. Write tests with the framework already in use (pytest or unittest), not a new one."),
    ("javascript", "JavaScript/TypeScript project conventions", &["*.js", "*.jsx", "*.ts", "*.tsx", "*.mjs", "*.cjs"],
     "JavaScript/TypeScript: detect the package manager from the lockfile (package-lock.json -> npm, yarn.lock -> yarn, pnpm-lock.yaml -> pnpm) and use it; do not switch managers or add dependencies without need. Respect the existing module system (ESM import/export vs CommonJS require). Match the project's TypeScript strictness and its existing test runner (jest, vitest, node --test)."),
    ("rust", "Rust project conventions", &["*.rs"],
     "Rust: keep it compiling — run cargo build (and cargo clippy if available) after edits and fix warnings you introduce. Match the project's error handling (anyhow / thiserror / plain Result) and avoid adding crates unless necessary. Run cargo test for the affected area. Prefer small, idiomatic changes over large rewrites."),
];

/// The built-in `[system]` rules as [`Rule`]s.
pub fn system_rules() -> Vec<Rule> {
    SYSTEM
        .iter()
        .map(|(name, desc, globs, body)| Rule {
            name: (*name).to_string(),
            scope: "system",
            description: (!desc.is_empty()).then(|| (*desc).to_string()),
            globs: globs.iter().map(|g| (*g).to_string()).collect(),
            body: (*body).to_string(),
        })
        .collect()
}

/// Strip a leading `--- … ---` frontmatter block (if present) and return its
/// `description:` value plus the remaining body. No YAML dependency — a tiny
/// hand parse that only understands `key: value` lines in the block.
pub fn parse_frontmatter(raw: &str) -> (Option<String>, String) {
    let (description, _globs, body) = parse_meta(raw);
    (description, body)
}

/// Like [`parse_frontmatter`] but also extracts `globs:` (space/comma-separated
/// relevance triggers). Returns `(description, globs, body)`.
pub fn parse_meta(raw: &str) -> (Option<String>, Vec<String>, String) {
    let trimmed = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if let Some(rest) = trimmed.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            let block = &rest[..end];
            let after = &rest[end + 4..];
            let body = after.strip_prefix('\n').unwrap_or(after).to_string();
            let mut description = None;
            let mut globs = Vec::new();
            for line in block.lines() {
                let l = line.trim();
                if let Some(v) = l.strip_prefix("description:") {
                    description = Some(v.trim().trim_matches('"').to_string());
                } else if let Some(v) = l.strip_prefix("globs:") {
                    globs = v
                        .split([',', ' '])
                        .map(|s| s.trim().trim_matches('"').to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
            return (description, globs, body);
        }
    }
    (None, Vec::new(), raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_extracted_and_stripped() {
        let raw = "---\ndescription: be terse\n---\nAlways write short code.\n";
        let (desc, body) = parse_frontmatter(raw);
        assert_eq!(desc.as_deref(), Some("be terse"));
        assert_eq!(body, "Always write short code.\n");
    }

    #[test]
    fn no_frontmatter_is_passthrough() {
        let (desc, body) = parse_frontmatter("just a rule");
        assert!(desc.is_none());
        assert_eq!(body, "just a rule");
    }

    #[test]
    fn globs_parsed_and_applies_matches_by_keyword() {
        let (_d, globs, _b) = parse_meta("---\nglobs: *.py, *.js\n---\nbody");
        assert_eq!(globs, vec!["*.py", "*.js"]);
        let r = Rule { name: "h".into(), scope: "user", description: None, globs, body: "b".into() };
        assert!(r.applies("create a python file")); // "python" from *.py
        assert!(r.applies("edit main.py please")); // ".py"
        assert!(!r.applies("what is 2 + 2"));
    }

    #[test]
    fn no_globs_always_applies() {
        let r = Rule { name: "g".into(), scope: "user", description: None, globs: vec![], body: "b".into() };
        assert!(r.applies("anything at all"));
    }

    #[test]
    fn select_filters_and_caps_budget() {
        let always = Rule { name: "a".into(), scope: "user", description: None, globs: vec![], body: "x".repeat(50) };
        let py = Rule { name: "p".into(), scope: "user", description: None, globs: vec!["*.py".into()], body: "y".repeat(50) };
        let rules = vec![always.clone(), py.clone()];
        // python task: both apply
        let (chosen, dropped) = select(&rules, "write a python script", 10_000);
        assert_eq!(chosen.len(), 2);
        assert_eq!(dropped, 0);
        // non-python task: only the always-on rule applies
        let (chosen, dropped) = select(&rules, "say hello", 10_000);
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].name, "a");
        assert_eq!(dropped, 1);
        // tiny budget: first fits, second dropped
        let (chosen, dropped) = select(&rules, "write a python script", 60);
        assert_eq!(chosen.len(), 1);
        assert_eq!(dropped, 1);
    }

    #[test]
    fn system_rules_present_and_overridable() {
        let sys = system_rules();
        assert!(sys.iter().all(|r| r.scope == "system"));
        assert!(sys.iter().any(|r| r.name == "no-hallucinated-apis"));
        assert!(sys.iter().any(|r| r.name == "python" && r.globs == vec!["*.py"]));

        // a project rule with the same name overrides the system one
        let root = std::env::temp_dir().join("smolcode_sysrule_test");
        let rdir = root.join(".smolcode").join("rules");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::create_dir_all(&rdir);
        let _ = std::fs::write(rdir.join("minimal-scope.md"), "PROJECT override of minimal-scope");
        let loaded = load(&root);
        let ms: Vec<&Rule> = loaded.iter().filter(|r| r.name == "minimal-scope").collect();
        assert_eq!(ms.len(), 1, "override should replace, not duplicate");
        assert_eq!(ms[0].scope, "project"); // project wins over user and system
        assert_eq!(ms[0].body, "PROJECT override of minimal-scope");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_reads_and_sorts() {
        let dir = std::env::temp_dir().join("smolcode_rules_test");
        let rdir = dir.join(".smolcode").join("rules");
        let _ = std::fs::create_dir_all(&rdir);
        let _ = std::fs::write(rdir.join("zeta.md"), "z rule");
        let _ = std::fs::write(rdir.join("alpha.md"), "---\ndescription: first\n---\na rule");
        let rules = load(&dir);
        let names: Vec<&str> = rules.iter().filter(|r| r.scope == "project").map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
        let alpha = rules.iter().find(|r| r.name == "alpha").unwrap();
        assert_eq!(alpha.description.as_deref(), Some("first"));
        assert_eq!(alpha.body, "a rule");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
