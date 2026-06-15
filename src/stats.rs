//! Per-session statistics for the smolcode coding agent.
//! Tracks tasks, model steps, tool calls (per-tool), tier escalations,
//! errors, and a rough running token estimate. Rendered via `/stats`.

use std::collections::BTreeMap;

/// Running counters for a single smolcode session.
#[derive(Clone, Default)]
pub struct Stats {
    /// User tasks submitted.
    pub tasks: usize,
    /// Agent model turns.
    pub steps: usize,
    /// Total tool invocations.
    pub tool_calls: usize,
    /// Per-tool invocation counts.
    pub by_tool: BTreeMap<String, usize>,
    /// Tier escalations.
    pub escalations: usize,
    /// Tool errors / agent errors.
    pub errors: usize,
    /// Running estimate of tokens seen.
    pub est_tokens: usize,
}

impl Stats {
    /// A fresh, zeroed `Stats`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a submitted user task.
    pub fn on_task(&mut self) {
        self.tasks += 1;
    }

    /// Record an agent model turn.
    pub fn on_step(&mut self) {
        self.steps += 1;
    }

    /// Record a tool invocation by name (bumps total + per-tool count).
    pub fn on_tool(&mut self, name: &str) {
        self.tool_calls += 1;
        *self.by_tool.entry(name.to_string()).or_insert(0) += 1;
    }

    /// Record a tier escalation.
    pub fn on_escalation(&mut self) {
        self.escalations += 1;
    }

    /// Record a tool or agent error.
    pub fn on_error(&mut self) {
        self.errors += 1;
    }

    /// Fold some text into the running token estimate via the (chars+3)/4 heuristic.
    pub fn add_tokens(&mut self, text: &str) {
        self.est_tokens += (text.chars().count() + 3) / 4;
    }

    /// Tools sorted by count descending, then by name ascending.
    fn ranked_tools(&self) -> Vec<(&String, usize)> {
        let mut v: Vec<(&String, usize)> = self.by_tool.iter().map(|(k, &c)| (k, c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        v
    }

    /// A compact multi-line summary for the `/stats` view: a headline of
    /// totals followed by a per-tool breakdown (highest count first).
    pub fn summary(&self) -> String {
        let mut out = format!(
            "tasks: {}  steps: {}  tools: {}  escalations: {}  errors: {}  ~tokens: {}",
            self.tasks,
            self.steps,
            self.tool_calls,
            self.escalations,
            self.errors,
            humanize(self.est_tokens),
        );
        for (name, count) in self.ranked_tools() {
            out.push_str(&format!("\n  {} x{}", name, count));
        }
        out
    }

    /// A one-line compact form for a status bar / footer.
    #[allow(dead_code)] // alt-format API (status footer)
    pub fn line(&self) -> String {
        format!(
            "{} tasks · {} tools · {} esc · ~{} tok",
            self.tasks,
            self.tool_calls,
            self.escalations,
            humanize(self.est_tokens),
        )
    }
}

/// Humanize a token count: >= 1000 gets a "k" suffix with at most one
/// decimal, trailing ".0" dropped (4200 -> "4.2k", 12000 -> "12k").
fn humanize(n: usize) -> String {
    if n < 1000 {
        return n.to_string();
    }
    // Tenths of a thousand, rounded.
    let tenths = (n + 50) / 100; // e.g. 4200 -> 42, 12000 -> 120
    let whole = tenths / 10;
    let frac = tenths % 10;
    if frac == 0 {
        format!("{}k", whole)
    } else {
        format!("{}.{}k", whole, frac)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate() {
        let mut s = Stats::new();
        for _ in 0..3 {
            s.on_task();
        }
        for _ in 0..11 {
            s.on_step();
        }
        for _ in 0..4 {
            s.on_tool("read_file");
        }
        for _ in 0..2 {
            s.on_tool("write_file");
        }
        s.on_escalation();
        s.add_tokens("the quick brown fox"); // 19 chars -> (19+3)/4 = 5

        assert_eq!(s.tasks, 3);
        assert_eq!(s.steps, 11);
        assert_eq!(s.tool_calls, 6);
        assert_eq!(s.by_tool["read_file"], 4);
        assert_eq!(s.by_tool["write_file"], 2);
        assert_eq!(s.escalations, 1);
        assert_eq!(s.est_tokens, 5);
    }

    #[test]
    fn summary_orders_by_count_desc() {
        let mut s = Stats::new();
        s.on_task();
        s.on_task();
        s.on_task();
        for _ in 0..4 {
            s.on_tool("read_file");
        }
        for _ in 0..2 {
            s.on_tool("write_file");
        }
        let out = s.summary();
        assert!(out.contains("tasks: 3"));
        assert!(out.contains("read_file"));
        assert!(out.contains("read_file x4"));
        assert!(out.contains("write_file x2"));
        let rf = out.find("read_file").unwrap();
        let wf = out.find("write_file").unwrap();
        assert!(rf < wf, "higher count must list first");
    }

    #[test]
    fn token_humanize_crosses_k() {
        let mut s = Stats::new();
        // 16800 chars -> (16800+3)/4 = 4200 tokens -> "4.2k"
        s.add_tokens(&"x".repeat(16800));
        assert_eq!(s.est_tokens, 4200);
        let out = s.summary();
        assert!(out.contains("4.2k"), "summary was: {out}");
        assert!(s.line().contains("4.2k"));
    }

    #[test]
    fn humanize_values() {
        assert_eq!(humanize(999), "999");
        assert_eq!(humanize(4200), "4.2k");
        assert_eq!(humanize(12000), "12k");
        assert_eq!(humanize(1000), "1k");
        assert_eq!(humanize(1500), "1.5k");
    }
}
