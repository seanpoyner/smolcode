//! Transcript search logic for the smolcode TUI.
//!
//! Pure `std`, no rendering and no I/O. The TUI owns the message list (a
//! slice of `String`, one per visible transcript line) and calls into this
//! module to find a query across messages and navigate between matches.
//!
//! Matching is ASCII case-insensitive. To keep reported byte offsets valid
//! indices into the ORIGINAL (non-lowercased) message strings, we never
//! build a lowercased haystack (which could change byte lengths for some
//! Unicode). Instead we scan the original at char boundaries (`char_indices`)
//! and, at each boundary, compare the original tail against the needle one
//! `char` at a time under ASCII-case-insensitive equality. The recorded
//! `offset` is therefore always a real char boundary in the original string.

/// A match: which message index, and the byte offset where the query starts.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Match {
    pub msg: usize,
    pub offset: usize,
}

/// Returns true if `haystack` (starting at the current position) begins with
/// `needle`, comparing char-by-char under ASCII case-insensitivity.
///
/// `needle` must be non-empty. Comparison walks chars in lockstep; both must
/// match in order and `needle` must be fully consumed for a hit.
fn starts_with_ascii_ci(haystack: &str, needle: &str) -> bool {
    let mut h = haystack.chars();
    for nc in needle.chars() {
        match h.next() {
            Some(hc) if hc.eq_ignore_ascii_case(&nc) => {}
            _ => return false,
        }
    }
    true
}

/// Find all (case-insensitive) occurrences of `query` across `messages`,
/// in order (by message index, then offset). Empty query -> no matches.
///
/// Offsets are byte indices into the corresponding original message string
/// and always fall on a char boundary.
pub fn find_all(messages: &[String], query: &str) -> Vec<Match> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    for (msg, text) in messages.iter().enumerate() {
        for (offset, _) in text.char_indices() {
            if starts_with_ascii_ci(&text[offset..], query) {
                out.push(Match { msg, offset });
            }
        }
    }
    out
}

/// State for interactive find/next navigation over a result set.
/// (Public API for an interactive search overlay; /search uses find_all.)
#[allow(dead_code)]
#[derive(Clone, Default)]
pub struct Search {
    pub query: String,
    pub matches: Vec<Match>,
    pub current: usize, // index into matches
}

#[allow(dead_code)]
impl Search {
    /// An empty search (no query, no matches).
    pub fn new() -> Self {
        Self::default()
    }

    /// Recompute matches for `query` against `messages`; resets current to 0.
    pub fn run(&mut self, messages: &[String], query: &str) {
        self.query = query.to_string();
        self.matches = find_all(messages, query);
        self.current = 0;
    }

    /// Move to the next match (wraps). No-op if empty.
    pub fn next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current = (self.current + 1) % self.matches.len();
    }

    /// Move to the previous match (wraps). No-op if empty.
    pub fn prev(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len();
        self.current = (self.current + n - 1) % n;
    }

    /// The currently-selected match, if any.
    pub fn current_match(&self) -> Option<Match> {
        self.matches.get(self.current).copied()
    }

    /// A short status like "3/12" (1-based) or "(no matches)".
    pub fn status(&self) -> String {
        if self.matches.is_empty() {
            "(no matches)".to_string()
        } else {
            format!("{}/{}", self.current + 1, self.matches.len())
        }
    }

    /// True if there are matches.
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn find_all_three_matches_with_valid_offsets() {
        let messages = msgs(&["Hello World", "goodbye world", "WORLD peace"]);
        let matches = find_all(&messages, "world");
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].msg, 0);
        assert_eq!(matches[1].msg, 1);
        assert_eq!(matches[2].msg, 2);
        // Each offset is a valid index into the ORIGINAL string and the slice
        // there equals the query under ASCII case-insensitivity.
        for m in &matches {
            let orig = &messages[m.msg];
            let end = m.offset + "world".len();
            let slice = &orig[m.offset..end];
            assert!(slice.eq_ignore_ascii_case("world"), "slice was {slice:?}");
        }
        // Sanity: the matched chars are the 'w'/'W' at the start.
        assert_eq!(&messages[0][matches[0].offset..matches[0].offset + 1], "W");
        assert_eq!(&messages[1][matches[1].offset..matches[1].offset + 1], "w");
        assert_eq!(&messages[2][matches[2].offset..matches[2].offset + 1], "W");
    }

    #[test]
    fn empty_query_no_matches() {
        let messages = msgs(&["anything here"]);
        assert!(find_all(&messages, "").is_empty());
    }

    #[test]
    fn case_insensitive_uppercase_query() {
        let messages = msgs(&["the world", "a WORLD"]);
        let matches = find_all(&messages, "WORLD");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].msg, 0);
        assert_eq!(matches[1].msg, 1);
    }

    #[test]
    fn search_run_next_prev_wrap_and_status() {
        let messages = msgs(&["Hello World", "goodbye world", "WORLD peace"]);
        let mut s = Search::new();
        assert!(s.is_empty());
        assert_eq!(s.status(), "(no matches)");
        assert_eq!(s.current_match(), None);

        s.run(&messages, "world");
        assert!(!s.is_empty());
        assert_eq!(s.matches.len(), 3);
        assert_eq!(s.current, 0);
        assert_eq!(s.status(), "1/3");
        assert_eq!(s.current_match(), Some(Match { msg: 0, offset: 6 }));

        s.next();
        assert_eq!(s.status(), "2/3");
        assert_eq!(s.current_match().unwrap().msg, 1);

        s.next();
        assert_eq!(s.status(), "3/3");
        assert_eq!(s.current_match().unwrap().msg, 2);

        s.next(); // wraps to first
        assert_eq!(s.status(), "1/3");
        assert_eq!(s.current_match().unwrap().msg, 0);

        s.prev(); // wraps back to last
        assert_eq!(s.status(), "3/3");
        assert_eq!(s.current_match().unwrap().msg, 2);
    }

    #[test]
    fn navigation_noop_when_empty() {
        let messages = msgs(&["nothing to see"]);
        let mut s = Search::new();
        s.run(&messages, "absent");
        assert!(s.is_empty());
        s.next();
        s.prev();
        assert_eq!(s.current, 0);
        assert_eq!(s.current_match(), None);
    }

    #[test]
    fn multiple_matches_in_one_message_ordered_by_offset() {
        let messages = msgs(&["aXaXa"]);
        let matches = find_all(&messages, "a");
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].offset, 0);
        assert_eq!(matches[1].offset, 2);
        assert_eq!(matches[2].offset, 4);
    }
}
