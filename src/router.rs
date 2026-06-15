//! Model-tier router — start small, escalate only when the small model is stuck.
//!
//! smolcode's differentiator is keeping cheap local models reliable. This module
//! decides (a) which tier to START a task on from a cheap complexity heuristic,
//! and (b) when to ESCALATE up the ladder after the small model wedges (repeated
//! identical tool calls / empty tool calls / errors). Heuristics are intentionally
//! transparent and conservative: we prefer the smallest tier that plausibly fits.

/// A coarse complexity tier for a task.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Small,
    Medium,
    Large,
}

/// User-selected reasoning-effort level. `High`/`Xtra` force the top ladder tier
/// (the 30B/32B) up front; `Off`/`Low` leave the cheap complexity heuristic in
/// charge. `Off` is the default and preserves prior behavior.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Think {
    #[default]
    Off,
    Low,
    High,
    Xtra,
}

impl Think {
    pub fn label(self) -> &'static str {
        match self {
            Think::Off => "off",
            Think::Low => "low",
            Think::High => "high",
            Think::Xtra => "xtra",
        }
    }

    /// Cycle to the next level (for the keybinding).
    pub fn next(self) -> Think {
        match self {
            Think::Off => Think::Low,
            Think::Low => Think::High,
            Think::High => Think::Xtra,
            Think::Xtra => Think::Off,
        }
    }

    /// Parse a level name; falls back to `next(current)` for an empty/unknown arg
    /// so `/think` with no argument cycles.
    pub fn parse_or_cycle(s: &str, current: Think) -> Think {
        match s.trim().to_lowercase().as_str() {
            "off" => Think::Off,
            "low" => Think::Low,
            "high" => Think::High,
            "xtra" | "xtra-high" | "extra" | "max" => Think::Xtra,
            _ => current.next(),
        }
    }

    /// Whether this level forces a bigger "thinking" model regardless of complexity.
    pub fn forces_top(self) -> bool {
        matches!(self, Think::High | Think::Xtra)
    }
}

/// Parse the parameter size (in billions) from a model tag, e.g. `granite4.1:30b`
/// -> 30, `gpt-oss:120b` -> 120. Returns None for `:latest` / no numeric tag.
fn parse_size_b(model: &str) -> Option<u32> {
    let tag = model.split(':').nth(1)?;
    let digits: String = tag.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Parameter count in billions parsed from a model tag, as a float so 1.5b works
/// (the `:NNb` parser above is integer-only). Scans for the last `<n>b` group, e.g.
/// `granite4.1:30b` -> 30.0, `smolcode-coder-py-1.5b:tools` -> 1.5, unknown -> 0.0.
/// Mirrors engine/config.py parse_size_b — the Tiny-Titan <=32B display filter.
pub fn parse_size_b_f(model: &str) -> f32 {
    let lower = model.to_lowercase();
    let bytes = lower.as_bytes();
    let mut best = 0.0f32;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'b' {
                if let Ok(v) = lower[start..i].parse::<f32>() {
                    best = v; // last <n>b wins
                }
            }
        } else {
            i += 1;
        }
    }
    best
}

/// The 16 fine-tune specialties (must match engine/config.py _SPECIALTIES).
pub const SPECIALTIES: [&str; 16] = [
    "py", "js", "bash", "git", "dotnet", "csharp", "java", "powershell", "rust",
    "docker", "bsd", "go", "sql", "cpp", "terraform", "orchestrate",
];

/// True if `model` is a per-specialty fine-tune (smolcode-coder-<specialty>-...).
pub fn is_specialty_model(model: &str) -> bool {
    let m = model.to_lowercase();
    SPECIALTIES
        .iter()
        .any(|s| m.starts_with(&format!("smolcode-coder-{s}-")))
}

// Tie-break order for classify_specialty (earlier wins); `py` last (safe default).
const SPECIALTY_ORDER: [&str; 16] = [
    "orchestrate", "git", "terraform", "docker", "sql", "powershell", "bsd", "rust",
    "go", "cpp", "java", "dotnet", "csharp", "bash", "js", "py",
];

// Keyword cues per specialty (lowercased substrings). Dependency-free port of the
// regex cues in engine/router.py _SPECIALTY_HINTS — close enough for routing.
fn specialty_cues(specialty: &str) -> &'static [&'static str] {
    match specialty {
        "orchestrate" => &["in parallel", "fan out", "fanout", "concurrently",
            "task_batch", "orchestrat", "several independent", "multiple independent",
            "simultaneously", "batch of tasks", "batch of jobs"],
        "git" => &["git", "commit", "rebase", "cherry-pick", "cherrypick",
            "merge conflict", "stash", " branch", "pull request", " pr ", "revert",
            "bisect", "staged"],
        "terraform" => &["terraform", " hcl", ".tf", "provider", "resource block",
            "infrastructure as code", " iac", "tfstate"],
        "docker" => &["docker", "dockerfile", "docker-compose", "docker compose",
            "container image", " image", "build -t", "entrypoint"],
        "sql" => &["sql", "select ", "insert ", "update ", "delete ", "join",
            "schema", " table", " index", "migration", "postgres", "sqlite",
            "mysql", "query"],
        "powershell" => &["powershell", "pwsh", ".ps1", "cmdlet", "get-", "set-",
            "write-output"],
        "bsd" => &["freebsd", "openbsd", "netbsd", " bsd", "pf.conf", "rc.d",
            "pkg_add"],
        "rust" => &["rust", "cargo", "crate", "rustc", ".rs", "borrow checker",
            "tokio"],
        "go" => &["golang", "goroutine", "go mod", "go test", ".go", " go "],
        "cpp" => &["c++", "cpp", "g++", "clang", "std::", "cmake", ".cpp",
            "template"],
        "java" => &["java", "maven", "gradle", " jvm", "junit", ".java"],
        "dotnet" => &[".net", "dotnet", "nuget", "asp.net", ".csproj", "msbuild"],
        "csharp" => &["c#", "csharp", "linq", ".cs", "xunit"],
        "bash" => &["shell script", "bash", "zsh", "chmod", "grep", "sed", "awk",
            " pipe", "cron", "stdout", "stderr", "$path"],
        "js" => &["javascript", "typescript", "node", "npm", "react", "vue", "jsx",
            "tsx", "webpack", "vite", "eslint", "package.json"],
        "py" => &["python", "pytest", "pandas", "numpy", "django", "flask", "pip",
            "venv", "def ", "async def", "decorator"],
        _ => &[],
    }
}

// Map a fenced code language (```lang) to a specialty.
fn fence_specialty(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "python" | "py" | "pytest" => "py",
        "bash" | "sh" | "shell" | "zsh" | "console" => "bash",
        "powershell" | "ps1" | "pwsh" => "powershell",
        "sql" | "psql" | "sqlite" => "sql",
        "javascript" | "js" | "ts" | "typescript" | "jsx" | "tsx" | "node" => "js",
        "go" | "golang" => "go",
        "rust" | "rs" => "rust",
        "cpp" | "c++" | "cc" | "c" => "cpp",
        "java" => "java",
        "csharp" | "cs" => "csharp",
        "dockerfile" | "docker" => "docker",
        "hcl" | "terraform" | "tf" => "terraform",
        _ => return None,
    })
}

/// Pick the specialist family for a task. Fence language wins; else keyword-cue
/// scoring with SPECIALTY_ORDER tie-break; default `py`. Mirrors
/// engine/router.py classify_specialty (dependency-free string matching).
pub fn classify_specialty(task: &str) -> &'static str {
    let lower = task.to_lowercase();
    // A fenced code block (```lang) is the single most explicit signal.
    if let Some(idx) = lower.find("```") {
        let rest = &lower[idx + 3..];
        let lang: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '.'))
            .collect();
        if let Some(s) = fence_specialty(&lang) {
            return s;
        }
    }
    // Otherwise score keyword cues; ties broken by SPECIALTY_ORDER (earlier wins).
    let mut best: &'static str = "py";
    let mut best_score = 0usize;
    let mut best_rank = SPECIALTY_ORDER.len();
    for s in SPECIALTIES {
        let score = specialty_cues(s).iter().filter(|c| lower.contains(**c)).count();
        if score == 0 {
            continue;
        }
        let rank = SPECIALTY_ORDER.iter().position(|o| *o == s).unwrap_or(usize::MAX);
        if score > best_score || (score == best_score && rank < best_rank) {
            best = SPECIALTIES.iter().find(|x| **x == s).unwrap();
            best_score = score;
            best_rank = rank;
        }
    }
    best
}

/// Specialty for a task: the learned ONNX classifier when it's confident
/// (`crate::route_clf`), else the regex `classify_specialty`. With the route-clf
/// feature off (or no models) this is exactly the regex.
pub fn classify_specialty_smart(task: &str) -> String {
    crate::route_clf::predict_specialty(task)
        .filter(|s| SPECIALTIES.contains(&s.as_str()))
        .unwrap_or_else(|| classify_specialty(task).to_string())
}

/// Build a specialist size ladder for `specialty` from the served model tags:
/// served `smolcode-coder-<specialty>-<size>` rungs (<=32B, smallest first), then
/// the generic Granite tiers. Mirrors engine/config.py _build_ladder.
pub fn specialty_ladder(specialty: &str, available: &[String]) -> Ladder {
    let mut models: Vec<(f32, String)> = Vec::new();
    let prefix = format!("smolcode-coder-{specialty}-");
    for m in available {
        let ml = m.to_lowercase();
        if ml.starts_with(&prefix) {
            let sz = parse_size_b_f(m);
            if sz > 0.0 && sz <= 32.0 {
                models.push((sz, m.clone()));
            }
        }
    }
    models.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let mut tiers: Vec<String> = models.into_iter().map(|(_, m)| m).collect();
    // Generic <=32B fallback rungs every specialist escalates into.
    for g in ["granite4.1:8b", "granite4.1:30b"] {
        if available.iter().any(|m| m == g) || available.is_empty() {
            tiers.push(g.to_string());
        }
    }
    if tiers.is_empty() {
        return Ladder::default_local();
    }
    Ladder { models: tiers }
}

/// Pick the model to use for elevated thinking, given the user's base model and
/// the list of models the endpoint actually serves. Prefers the **largest model
/// in the same family** (e.g. `granite4.1:8b` -> `granite4.1:30b`); falls back to
/// a known big coder model if present, else the base model unchanged. Never
/// returns a model that isn't in `available` (when `available` is non-empty).
pub fn big_model(base: &str, available: &[String]) -> String {
    let family = base.split(':').next().unwrap_or(base);
    let base_size = parse_size_b(base).unwrap_or(0);
    // largest same-family model strictly bigger than the base
    let mut best: Option<(u32, &String)> = None;
    for m in available {
        if m.split(':').next() == Some(family) {
            if let Some(sz) = parse_size_b(m) {
                if sz > base_size && best.map_or(true, |(b, _)| sz > b) {
                    best = Some((sz, m));
                }
            }
        }
    }
    if let Some((_, m)) = best {
        return m.clone();
    }
    // fallback: a known big coder model if the endpoint has it
    for cand in ["granite4.1:30b", "qwen2.5-coder:32b", "qwen3-vl:32b"] {
        if available.iter().any(|m| m == cand) {
            return cand.to_string();
        }
    }
    base.to_string()
}

/// An ordered ladder of model ids, smallest first. Built from config or a default.
#[derive(Clone)]
pub struct Ladder {
    pub models: Vec<String>,
}

impl Ladder {
    /// Default local ladder (granite 8b -> qwen2.5-coder:14b -> qwen2.5-coder:32b).
    pub fn default_local() -> Self {
        Ladder {
            models: vec![
                "granite4.1:8b".to_string(),
                "qwen2.5-coder:14b".to_string(),
                "qwen2.5-coder:32b".to_string(),
            ],
        }
    }

    /// Build from an explicit list (e.g. config); empty falls back to default.
    #[allow(dead_code)] // public API: config-supplied ladders (not yet wired)
    pub fn from_models(models: Vec<String>) -> Self {
        if models.is_empty() {
            Ladder::default_local()
        } else {
            Ladder { models }
        }
    }

    /// The model id at a given tier (clamped to the ladder length).
    pub fn model_for(&self, tier: Tier) -> String {
        let last = self.models.len().saturating_sub(1);
        let idx = match tier {
            Tier::Small => 0,
            Tier::Medium => 1.min(last),
            Tier::Large => last,
        };
        self.models[idx].clone()
    }

    /// Given the current model id, the next bigger model id (None if already top).
    pub fn escalate(&self, current: &str) -> Option<String> {
        let pos = self.models.iter().position(|m| m == current)?;
        self.models.get(pos + 1).cloned()
    }
}

/// Cue words that pull a task up to at least [`Tier::Medium`].
const MEDIUM_CUES: &[&str] = &[
    "refactor",
    "across",
    "all files",
    "migrate",
    "design",
    "architecture",
    "debug",
    "multiple files",
    "implement",
    "integrate",
];

/// Cue phrases that pull a task straight to [`Tier::Large`].
const HARD_CUES: &[&str] = &[
    "refactor across",
    "migrate",
    "architecture",
    "design the",
    "multi-file",
    "entire codebase",
    "whole codebase",
    "rewrite the",
];

/// Count lines that look like an ordered/bulleted step (e.g. "1. ", "- ", "* ").
fn step_like_lines(task: &str) -> usize {
    task.lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("- ")
                || t.starts_with("* ")
                || t.starts_with("+ ")
                || t.chars()
                    .next()
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false)
                    && (t.contains(". ") || t.contains(") "))
        })
        .count()
}

/// Heuristically classify a task prompt's complexity from cheap signals:
/// length/word-count, multi-file or multi-step cues, code fences, and step lists,
/// vs simple cues. Conservative: prefer the smallest tier that fits — we WANT to
/// use small models.
pub fn classify(task: &str) -> Tier {
    let lower = task.to_lowercase();
    let word_count = task.split_whitespace().count();
    let steps = step_like_lines(task);
    let has_fence = task.contains("```");

    let hard = word_count > 120
        || steps >= 6
        || HARD_CUES.iter().any(|c| lower.contains(c));
    if hard {
        return Tier::Large;
    }

    let medium = word_count > 40
        || steps >= 3
        || has_fence
        || MEDIUM_CUES.iter().any(|c| lower.contains(c));
    if medium {
        return Tier::Medium;
    }

    Tier::Small
}

/// Starting tier for a task: the learned classifier when it's confident
/// ([`crate::route_clf`]), otherwise the transparent [`classify`] heuristic.
/// With the `route-clf` feature off (the default) this is exactly `classify`.
pub fn classify_start(task: &str) -> Tier {
    crate::route_clf::predict_tier(task).unwrap_or_else(|| classify(task))
}

/// Decide whether to escalate after a step, given signals from the agent loop.
/// Escalate when the small model is clearly stuck.
pub fn should_escalate(
    repeated_tool_calls: usize,
    empty_tool_calls: usize,
    consecutive_errors: usize,
) -> bool {
    repeated_tool_calls >= 3 || empty_tool_calls >= 2 || consecutive_errors >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_trivial_is_small() {
        assert_eq!(classify("fix typo in README"), Tier::Small);
        assert_eq!(classify("rename foo to bar"), Tier::Small);
        assert_eq!(classify("what is in src/main.rs"), Tier::Small);
    }

    #[test]
    fn classify_medium() {
        assert_eq!(classify("debug why the parser drops trailing commas"), Tier::Medium);
        let bulleted = "do this:\n- step one\n- step two\n- step three";
        assert_eq!(classify(bulleted), Tier::Medium);
        assert_eq!(classify("here is code:\n```rust\nfn x(){}\n```"), Tier::Medium);
    }

    #[test]
    fn classify_large() {
        assert_eq!(
            classify("refactor across the entire codebase to async"),
            Tier::Large
        );
        assert_eq!(classify("migrate the storage layer to sqlite"), Tier::Large);
        let long = "word ".repeat(130);
        assert_eq!(classify(&long), Tier::Large);
    }

    #[test]
    fn escalate_chain() {
        let l = Ladder::default_local();
        assert_eq!(l.escalate("granite4.1:8b").as_deref(), Some("qwen2.5-coder:14b"));
        assert_eq!(
            l.escalate("qwen2.5-coder:14b").as_deref(),
            Some("qwen2.5-coder:32b")
        );
        assert_eq!(l.escalate("qwen2.5-coder:32b"), None);
        assert_eq!(l.escalate("unknown-model"), None);
    }

    #[test]
    fn model_for_clamps_single_element() {
        let l = Ladder::from_models(vec!["granite4.1:3b".to_string()]);
        assert_eq!(l.model_for(Tier::Small), "granite4.1:3b");
        assert_eq!(l.model_for(Tier::Medium), "granite4.1:3b");
        assert_eq!(l.model_for(Tier::Large), "granite4.1:3b");
    }

    #[test]
    fn model_for_default_ladder() {
        let l = Ladder::default_local();
        assert_eq!(l.model_for(Tier::Small), "granite4.1:8b");
        assert_eq!(l.model_for(Tier::Medium), "qwen2.5-coder:14b");
        assert_eq!(l.model_for(Tier::Large), "qwen2.5-coder:32b");
    }

    #[test]
    fn from_models_empty_falls_back() {
        let l = Ladder::from_models(vec![]);
        assert_eq!(l.models, Ladder::default_local().models);
    }

    #[test]
    fn big_model_prefers_largest_same_family() {
        let avail = vec![
            "granite4.1:3b".to_string(),
            "granite4.1:8b".to_string(),
            "granite4.1:30b".to_string(),
            "qwen2.5-coder:32b".to_string(),
        ];
        // granite base -> the granite 30b (same family), not the qwen coder
        assert_eq!(big_model("granite4.1:8b", &avail), "granite4.1:30b");
        // a base with no bigger same-family model falls back to a known big coder
        let avail2 = vec!["llama3.3:latest".to_string(), "qwen2.5-coder:32b".to_string()];
        assert_eq!(big_model("llama3.3:latest", &avail2), "qwen2.5-coder:32b");
        // nothing available -> unchanged
        assert_eq!(big_model("granite4.1:8b", &[]), "granite4.1:8b");
    }

    #[test]
    fn think_forces_top_flags() {
        assert!(!Think::Off.forces_top());
        assert!(!Think::Low.forces_top());
        assert!(Think::High.forces_top());
        assert!(Think::Xtra.forces_top());
    }

    #[test]
    fn think_cycle_and_parse() {
        assert_eq!(Think::Off.next(), Think::Low);
        assert_eq!(Think::Xtra.next(), Think::Off);
        assert_eq!(Think::parse_or_cycle("high", Think::Off), Think::High);
        assert_eq!(Think::parse_or_cycle("", Think::Off), Think::Low); // cycles
        assert_eq!(Think::parse_or_cycle("xtra-high", Think::Off), Think::Xtra);
    }

    #[test]
    fn escalate_thresholds() {
        assert!(!should_escalate(0, 0, 0));
        assert!(!should_escalate(2, 1, 1));
        assert!(should_escalate(3, 0, 0));
        assert!(should_escalate(0, 2, 0));
        assert!(should_escalate(0, 0, 2));
    }

    #[test]
    fn parse_size_b_f_floats() {
        assert_eq!(parse_size_b_f("granite4.1:30b"), 30.0);
        assert_eq!(parse_size_b_f("smolcode-coder-py-1.5b:tools"), 1.5);
        assert_eq!(parse_size_b_f("qwen2.5-coder:32b"), 32.0);
        assert_eq!(parse_size_b_f("mystery:latest"), 0.0);
    }

    #[test]
    fn is_specialty_model_detects_finetunes() {
        assert!(is_specialty_model("smolcode-coder-rust-3b:tools"));
        assert!(!is_specialty_model("granite4.1:8b"));
        assert!(!is_specialty_model("smolcode-coder-1.5b:tools"));
    }

    #[test]
    fn classify_specialty_routes() {
        assert_eq!(classify_specialty("rebase the branch and fix the merge conflict"), "git");
        assert_eq!(classify_specialty("write a goroutine over a channel"), "go");
        assert_eq!(classify_specialty("create a terraform module for s3"), "terraform");
        assert_eq!(classify_specialty("reverse a string"), "py"); // default
        assert_eq!(classify_specialty("```rust\nfn x(){}\n```"), "rust"); // fence wins
    }

    #[test]
    fn specialty_ladder_builds_from_served() {
        let served = vec![
            "smolcode-coder-py-1.5b:tools".to_string(),
            "smolcode-coder-py-3b:tools".to_string(),
            "granite4.1:8b".to_string(),
            "granite4.1:30b".to_string(),
            "huge:70b".to_string(),
        ];
        let lad = specialty_ladder("py", &served);
        assert_eq!(
            lad.models,
            vec![
                "smolcode-coder-py-1.5b:tools",
                "smolcode-coder-py-3b:tools",
                "granite4.1:8b",
                "granite4.1:30b",
            ]
        );
        // a wholly-unserved specialty falls back to the generic local ladder
        let lad2 = specialty_ladder("rust", &served);
        assert!(lad2.models.iter().all(|m| parse_size_b_f(m) <= 32.0));
    }
}
