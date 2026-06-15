//! Token/context-window usage tracking for the smolcode TUI status bar.
//!
//! No tokenizer crate is available, so token counts are estimated with the
//! standard rough heuristic of `chars / 4`. We track the running conversation
//! size against a per-model context-window budget and render an opencode-style
//! label such as `45% (14.2k/32k)`.

use ratatui::style::Color;

/// Estimate the token count of `text` using the chars/4 approximation.
fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() + 3) / 4
}

/// Best-effort context window for known local model ids (granite/qwen/llama/etc),
/// default 8192 if unknown. Matches on substrings, case-insensitive.
pub fn model_context_window(model: &str) -> usize {
    let m = model.to_lowercase();
    // (substring, window) — checked in order, first match wins.
    const TABLE: &[(&str, usize)] = &[
        ("granite4.1", 131_072),
        ("granite-4.1", 131_072),
        ("qwen2.5-coder", 32_768),
        ("qwen-coder", 32_768),
        ("llama3", 8_192),
    ];
    for (needle, window) in TABLE {
        if m.contains(needle) {
            return *window;
        }
    }
    8_192
}

/// Humanize a token count, using a `k` suffix when >= 1000.
/// Shows one decimal only when it is not a whole `k`, dropping a trailing `.0`.
fn humanize(n: usize) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let thousands = n as f64 / 1000.0;
    let rounded = (thousands * 10.0).round() / 10.0;
    if (rounded.fract()).abs() < f64::EPSILON {
        format!("{}k", rounded as i64)
    } else {
        format!("{:.1}k", rounded)
    }
}

#[derive(Clone)]
pub struct Usage {
    /// Total token budget for the active model.
    pub context_window: usize,
    /// Estimated tokens currently in context.
    pub used_tokens: usize,
    /// Tokens in the most recent request (optional detail).
    pub last_prompt: usize,
}

impl Usage {
    /// New tracker; infer the window from the model id (see `model_context_window`).
    pub fn new(model: &str) -> Self {
        Usage {
            context_window: model_context_window(model),
            used_tokens: 0,
            last_prompt: 0,
        }
    }

    /// Re-estimate used tokens from the full set of conversation strings
    /// (system + each user/assistant turn). Call after each turn.
    pub fn set_from_texts(&mut self, texts: &[&str]) {
        self.used_tokens = texts.iter().map(|t| estimate_tokens(t)).sum();
        self.last_prompt = texts.last().map(|t| estimate_tokens(t)).unwrap_or(0);
    }

    /// Fraction used in 0.0..=1.0 (saturating).
    pub fn fraction(&self) -> f32 {
        if self.context_window == 0 {
            return 1.0;
        }
        (self.used_tokens as f32 / self.context_window as f32).min(1.0)
    }

    /// Compact status-bar label, e.g. "45% (14.2k/32k)".
    pub fn label(&self) -> String {
        let pct = (self.fraction() * 100.0).round() as u32;
        format!(
            "{}% ({}/{})",
            pct,
            humanize(self.used_tokens),
            humanize(self.context_window)
        )
    }

    /// A ratatui Color for the meter: green < 0.6, yellow < 0.85, red otherwise.
    pub fn color(&self) -> Color {
        let f = self.fraction();
        if f < 0.6 {
            Color::Green
        } else if f < 0.85 {
            Color::Yellow
        } else {
            Color::Red
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_windows() {
        assert_eq!(model_context_window("granite4.1:3b"), 131_072);
        assert_eq!(model_context_window("Qwen2.5-Coder-7B"), 32_768);
        assert_eq!(model_context_window("something-unknown"), 8_192);
    }

    #[test]
    fn label_formatting() {
        let u = Usage {
            context_window: 131_072,
            used_tokens: 14_200,
            last_prompt: 0,
        };
        assert_eq!(humanize(14_200), "14.2k");
        assert_eq!(humanize(32_768), "32.8k");
        assert_eq!(humanize(32_000), "32k");
        assert_eq!(humanize(999), "999");
        // 14200 / 131072 ~= 10.8% -> 11%
        assert_eq!(u.label(), "11% (14.2k/131.1k)");
    }

    #[test]
    fn fraction_saturates() {
        let u = Usage {
            context_window: 8_192,
            used_tokens: 100_000,
            last_prompt: 0,
        };
        assert_eq!(u.fraction(), 1.0);
        assert_eq!(u.color(), Color::Red);
    }
}
