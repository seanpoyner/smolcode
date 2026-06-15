//! Auto-compaction bookkeeping for the smolcode agent's conversation.
//!
//! Decides WHEN the running conversation should be summarized (compacted) to
//! stay inside a small model's context window, and prepares the mechanical
//! pieces: estimating context size, splitting older vs recent turns, building
//! the summarization prompt, and rebuilding the trimmed conversation around a
//! produced synopsis. No LLM calls happen here (the caller drives the model);
//! token counts use the same `(chars + 3) / 4` heuristic as `crate::usage`.

/// Estimate the token count of `text` using the chars/4 approximation.
fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() + 3) / 4
}

/// Estimate the token size of a set of conversation turns plus system prompt.
pub fn estimate_context(system: &str, turns: &[(String, String)]) -> usize {
    let mut total = estimate_tokens(system);
    for (user, assistant) in turns {
        total += estimate_tokens(user);
        total += estimate_tokens(assistant);
    }
    total
}

/// Decide whether compaction should trigger. `used_tokens` is the current
/// estimated context size, `context_window` the model's budget. Triggers when
/// the usage fraction is at or above `threshold` (e.g. 0.8) AND there are more
/// turns than `keep_recent` (so compaction can actually reclaim space).
pub fn should_compact(
    used_tokens: usize,
    context_window: usize,
    threshold: f32,
    turns: usize,
    keep_recent: usize,
) -> bool {
    if context_window == 0 || turns <= keep_recent {
        return false;
    }
    let threshold = threshold.clamp(0.1, 0.99);
    let fraction = used_tokens as f32 / context_window as f32;
    fraction >= threshold
}

/// Split a conversation into (older, recent): the last `keep_recent` turns are
/// kept verbatim; the rest are returned as `older` to be summarized. Never
/// panics; if `turns.len() <= keep_recent`, `older` is empty.
#[allow(dead_code)] // explicit-rebuild compaction path (trigger uses should_compact)
pub fn split_for_compaction(
    turns: &[(String, String)],
    keep_recent: usize,
) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let split = turns.len().saturating_sub(keep_recent);
    let older = turns[..split].to_vec();
    let recent = turns[split..].to_vec();
    (older, recent)
}

/// Clip `text` to at most `max` chars, appending an ellipsis marker when cut.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head} (...)")
}

/// Build the prompt text asking a model to summarize the `older` turns into a
/// compact synopsis that preserves decisions, file paths, and open tasks.
/// The caller sends this to the model; this module only builds the string.
#[allow(dead_code)] // explicit-rebuild compaction path (trigger uses should_compact)
pub fn summarize_prompt(older: &[(String, String)]) -> String {
    let mut s = String::new();
    s.push_str(
        "Summarize the earlier part of this coding conversation into a terse synopsis. \
         Preserve, concisely: the user's goals, the decisions made, files created or edited \
         (with their paths), commands that were run, and what still remains to do. \
         Write plain text (no markdown headers needed). Do not invent details that are not \
         present below.\n\nEarlier conversation:\n\n",
    );
    for (user, assistant) in older {
        s.push_str("User: ");
        s.push_str(&clip(user, 1500));
        s.push_str("\nAssistant: ");
        s.push_str(&clip(assistant, 1500));
        s.push_str("\n\n");
    }
    s.push_str("Synopsis:");
    s
}

/// Given a produced `synopsis`, build the replacement conversation: a single
/// synthetic turn carrying the synopsis, followed by the `recent` turns.
#[allow(dead_code)] // explicit-rebuild compaction path (trigger uses should_compact)
pub fn rebuild(synopsis: &str, recent: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(recent.len() + 1);
    out.push((
        "(earlier conversation summarized)".to_string(),
        synopsis.to_string(),
    ));
    out.extend_from_slice(recent);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turns(n: usize) -> Vec<(String, String)> {
        (0..n)
            .map(|i| (format!("user {i}"), format!("assistant {i}")))
            .collect()
    }

    #[test]
    fn should_compact_triggers_when_full_and_enough_turns() {
        assert!(should_compact(8500, 10_000, 0.8, 10, 4));
    }

    #[test]
    fn should_compact_false_under_threshold() {
        assert!(!should_compact(5000, 10_000, 0.8, 10, 4));
    }

    #[test]
    fn should_compact_false_when_not_enough_turns() {
        assert!(!should_compact(9999, 10_000, 0.8, 4, 4));
        assert!(!should_compact(9999, 10_000, 0.8, 3, 4));
    }

    #[test]
    fn should_compact_false_when_window_zero() {
        assert!(!should_compact(100, 0, 0.8, 10, 4));
    }

    #[test]
    fn estimate_context_is_monotonic() {
        let base = turns(2);
        let mut more = base.clone();
        more.push(("a longer new turn".to_string(), "with a reply".to_string()));
        assert!(estimate_context("sys", &more) > estimate_context("sys", &base));
    }

    #[test]
    fn split_keeps_last_n_verbatim() {
        let t = turns(5);
        let (older, recent) = split_for_compaction(&t, 2);
        assert_eq!(older.len(), 3);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].0, "user 3");
        assert_eq!(recent[1].0, "user 4");
    }

    #[test]
    fn split_older_empty_when_keep_exceeds_len() {
        let t = turns(2);
        let (older, recent) = split_for_compaction(&t, 5);
        assert!(older.is_empty());
        assert_eq!(recent.len(), 2);
    }

    #[test]
    fn rebuild_places_synopsis_first() {
        let recent = turns(2);
        let out = rebuild("done X, todo Y", &recent);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].0, "(earlier conversation summarized)");
        assert_eq!(out[0].1, "done X, todo Y");
        assert_eq!(out[1].0, "user 0");
    }

    #[test]
    fn summarize_prompt_includes_older_text() {
        let older = vec![
            ("build a parser".to_string(), "created parser.rs".to_string()),
            ("add tests".to_string(), "added test module".to_string()),
        ];
        let p = summarize_prompt(&older);
        assert!(p.contains("build a parser"));
        assert!(p.contains("created parser.rs"));
        assert!(p.contains("added test module"));
    }
}
