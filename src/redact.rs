//! Secret redaction for the smolcode coding agent.
//!
//! Replaces likely API keys, tokens, and credentials with `[REDACTED]` before
//! tool output, transcripts, or JSONL traces are persisted. Detection is
//! deliberately conservative (high precision) so ordinary code and prose are
//! left untouched. Implemented with manual scanning over whitespace/token
//! boundaries — no `regex` crate, std-only.

const REDACTED: &str = "[REDACTED]";

/// Token prefixes whose presence (with a long-enough tail) marks a secret.
const PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "AKIA",
    "AIza",
    "glpat-",
];

/// Substrings (case-insensitive) of a key name that mark its value sensitive.
const SENSITIVE_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "token",
    "secret",
    "password",
    "passwd",
    "access_key",
    "client_secret",
    "private_key",
];

/// Return a copy of `text` with likely secrets replaced by "[REDACTED]".
/// Conservative: only redacts high-confidence token shapes so normal code/
/// output is untouched.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut first = true;
    for line in text.split_inclusive('\n') {
        // split_inclusive keeps the trailing '\n'; separate it so line logic
        // operates on the bare content.
        let (content, nl) = match line.strip_suffix('\n') {
            Some(c) => (c, "\n"),
            None => (line, ""),
        };
        let _ = first;
        first = false;
        out.push_str(&redact_line(content));
        out.push_str(nl);
    }
    out
}

/// True if `text` contains something that looks like a secret (same detection
/// as redact, without rewriting).
#[allow(dead_code)] // companion API
pub fn has_secret(text: &str) -> bool {
    redact(text) != text
}

fn redact_line(line: &str) -> String {
    // 1) Authorization header: keep the label, redact the value.
    if let Some(rewritten) = redact_authorization(line) {
        return rewritten;
    }

    // 2) Sensitive `key: value` / `key = value` pairs (incl. JSON), which the
    //    tokenizer below would otherwise split across pieces.
    let kv = redact_sensitive_kv(line);
    let base: &str = kv.as_deref().unwrap_or(line);

    // 3) Token-by-token for prefixes, JWTs, bare key=value, and blobs.
    let mut out = String::with_capacity(base.len());
    for piece in split_keep_delims(base) {
        match piece {
            Piece::Delim(d) => out.push_str(d),
            Piece::Token(t) => out.push_str(&redact_token(t)),
        }
    }
    out
}

fn is_keychar(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Redact the values of sensitive `key: value` / `key = value` pairs on a line
/// (quoted or unquoted). Keeps keys, quotes, and structure. Returns Some when
/// at least one value was redacted.
fn redact_sensitive_kv(line: &str) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let lower: Vec<char> = line.to_ascii_lowercase().chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;
    let mut any = false;
    while i < n {
        // sensitive key starting at a word boundary?
        let mut klen = 0;
        if i == 0 || !is_keychar(chars[i - 1]) {
            for key in SENSITIVE_KEYS {
                let kl = key.chars().count();
                if i + kl <= n
                    && lower[i..i + kl].iter().collect::<String>() == *key
                    && (i + kl >= n || !is_keychar(chars[i + kl]))
                {
                    klen = kl;
                    break;
                }
            }
        }
        if klen == 0 {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // emit the key, then walk the separator: optional `"`/ws, then `:`/`=`,
        // then ws, then optional opening quote.
        for k in i..i + klen {
            out.push(chars[k]);
        }
        let mut j = i + klen;
        let mut saw_sep = false;
        let mut opened_quote = false;
        while j < n {
            match chars[j] {
                ':' | '=' => {
                    out.push(chars[j]);
                    j += 1;
                    while j < n && (chars[j] == ' ' || chars[j] == '\t') {
                        out.push(chars[j]);
                        j += 1;
                    }
                    if j < n && (chars[j] == '"' || chars[j] == '\'') {
                        out.push(chars[j]);
                        opened_quote = true;
                        j += 1;
                    }
                    saw_sep = true;
                    break;
                }
                '"' | '\'' | ' ' | '\t' => {
                    out.push(chars[j]);
                    j += 1;
                }
                _ => break,
            }
        }
        if !saw_sep {
            i = j;
            continue;
        }
        // value runs to the closing quote, or to a structural delimiter.
        let vs = j;
        while j < n {
            let c = chars[j];
            if opened_quote {
                if c == '"' || c == '\'' {
                    break;
                }
            } else if matches!(c, ',' | '}' | ']' | ';' | ' ' | '\t') {
                break;
            }
            j += 1;
        }
        let value: String = chars[vs..j].iter().collect();
        if value_is_nontrivial(&value) {
            out.push_str(REDACTED);
            any = true;
        } else {
            out.push_str(&value);
        }
        i = j;
    }
    if any {
        Some(out)
    } else {
        None
    }
}

/// Redact the value of an `Authorization:` header line (case-insensitive),
/// keeping the label. Returns `None` if the line has no such header.
fn redact_authorization(line: &str) -> Option<String> {
    let label = "authorization:";
    let lower = line.to_ascii_lowercase();
    let pos = lower.find(label)?;
    let value_start = pos + label.len();
    let before = &line[..value_start];
    let value = &line[value_start..];
    let trimmed = value.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    // Preserve the leading whitespace between the label and the value.
    let ws_len = value.len() - trimmed.len();
    let ws = &value[..ws_len];
    Some(format!("{before}{ws}{REDACTED}"))
}

/// Decide how a single whitespace-delimited token should be rendered.
fn redact_token(token: &str) -> String {
    // key=value form (handles api_key=XXXX and JSON "api_key": "XXXX").
    if let Some(rewritten) = redact_key_value(token) {
        return rewritten;
    }
    if is_secretish_standalone(token) {
        return REDACTED.to_string();
    }
    token.to_string()
}

/// Handle `key=value` and `"key": "value"` shapes. Returns `None` when the
/// token is not a sensitive key/value pair.
fn redact_key_value(token: &str) -> Option<String> {
    // Find the first separator: '=' or ':'.
    let sep_idx = token.char_indices().find_map(|(i, c)| {
        if c == '=' || c == ':' {
            Some(i)
        } else {
            None
        }
    })?;
    let sep = &token[sep_idx..sep_idx + 1];
    let key_raw = &token[..sep_idx];
    let val_raw = &token[sep_idx + 1..];

    // Normalize the key: strip surrounding quotes/spaces for the name check.
    let key_name = key_raw.trim().trim_matches('"').trim_matches('\'');
    if key_name.is_empty() {
        return None;
    }
    let key_lower = key_name.to_ascii_lowercase();
    if !SENSITIVE_KEYS.iter().any(|k| key_lower.contains(k)) {
        return None;
    }

    // Inspect the value: it may be quoted and/or have leading space.
    let val_trimmed = val_raw.trim();
    // Detect a wrapping pair of quotes.
    let (open_q, inner, close_q) = strip_quotes(val_trimmed);
    let inner_clean = inner.trim();

    if !value_is_nontrivial(inner_clean) {
        return None;
    }

    // Preserve any leading whitespace after the separator (JSON `: "v"`).
    let lead_ws_len = val_raw.len() - val_raw.trim_start().len();
    let lead_ws = &val_raw[..lead_ws_len];

    Some(format!(
        "{key_raw}{sep}{lead_ws}{open_q}{REDACTED}{close_q}",
    ))
}

/// Split off a single pair of matching surrounding quotes.
/// Returns (opening_quote, inner, closing_quote).
fn strip_quotes(s: &str) -> (&str, &str, &str) {
    let bytes = s.as_bytes();
    if s.len() >= 2 {
        let first = bytes[0];
        let last = bytes[s.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return (&s[..1], &s[1..s.len() - 1], &s[s.len() - 1..]);
        }
    }
    ("", s, "")
}

fn value_is_nontrivial(v: &str) -> bool {
    if v.len() < 6 {
        return false;
    }
    let lower = v.to_ascii_lowercase();
    !matches!(lower.as_str(), "..." | "null" | "none" | "<redacted>")
}

/// Standalone-token checks: known prefixes, JWTs, and high-entropy blobs.
fn is_secretish_standalone(token: &str) -> bool {
    // Strip surrounding quotes/punctuation that frequently bound a token.
    let core = token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | '(' | ')'));
    if core.is_empty() {
        return false;
    }

    // Known prefixes with a long-enough secret tail.
    for p in PREFIXES {
        if core.starts_with(p) {
            let tail_len = core.len() - p.len();
            if tail_len >= 16 {
                return true;
            }
        }
    }

    if is_jwt(core) {
        return true;
    }

    if is_high_entropy_blob(core) {
        return true;
    }

    false
}

/// `xxxxx.yyyyy.zzzzz` with base64url-ish parts and overall length >= 40.
fn is_jwt(s: &str) -> bool {
    if s.len() < 40 {
        return false;
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts.iter().all(|p| {
        !p.is_empty() && p.chars().all(is_base64url_char)
    })
}

/// A standalone token that is all [A-Za-z0-9_\-+/=], length >= 40, and mixes
/// letters and digits.
///
/// Tradeoff: a 40-char git SHA is all-hex (digits+letters) and would otherwise
/// match. To preserve such SHAs we require the blob to be either NOT
/// all-lowercase-hex, or longer than 44 chars. This keeps ordinary 40-char
/// git object hashes intact while still catching most real secrets, which are
/// either mixed-case, contain base64 punctuation, or are longer.
fn is_high_entropy_blob(s: &str) -> bool {
    if s.len() < 40 {
        return false;
    }
    if !s.chars().all(is_blob_char) {
        return false;
    }
    let has_alpha = s.chars().any(|c| c.is_ascii_alphabetic());
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    if !(has_alpha && has_digit) {
        return false;
    }
    // Git-SHA preservation: skip all-lowercase-hex blobs up to 44 chars.
    let all_lower_hex = s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    if all_lower_hex && s.len() <= 44 {
        return false;
    }
    true
}

fn is_base64url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn is_blob_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '=')
}

/// A piece of a line: either a run of delimiters or a token.
enum Piece<'a> {
    Token(&'a str),
    Delim(&'a str),
}

/// Token delimiters. We split on whitespace plus a few structural characters
/// so `"api_key":"value"` and `key=value,next` tokenize usefully while
/// keeping the delimiters for faithful reconstruction.
fn is_delim(c: char) -> bool {
    c.is_whitespace()
}

/// Split a line into tokens and delimiter runs, preserving everything so the
/// concatenation of all pieces equals the input.
fn split_keep_delims(line: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let mut idx = 0;
    let bytes_len = line.len();
    let mut iter = line.char_indices().peekable();
    let mut run_start = 0;
    let mut in_delim = false;
    let mut started = false;

    while let Some((i, c)) = iter.next() {
        let this_delim = is_delim(c);
        if !started {
            started = true;
            in_delim = this_delim;
            run_start = i;
        } else if this_delim != in_delim {
            let slice = &line[run_start..i];
            if in_delim {
                pieces.push(Piece::Delim(slice));
            } else {
                pieces.push(Piece::Token(slice));
            }
            in_delim = this_delim;
            run_start = i;
        }
        idx = i + c.len_utf8();
    }
    let _ = bytes_len;
    if started {
        let slice = &line[run_start..idx];
        if in_delim {
            pieces.push(Piece::Delim(slice));
        } else {
            pieces.push(Piece::Token(slice));
        }
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_openai_key() {
        let key = "sk-abcdef0123456789ABCDEF0123";
        let out = redact(key);
        assert!(out.contains(REDACTED), "out = {out}");
        assert!(!out.contains(key), "out still contains key: {out}");
    }

    #[test]
    fn redacts_authorization_header() {
        let line = "Authorization: Bearer abcdefghijklmnop0123456789";
        let out = redact(line);
        assert!(out.starts_with("Authorization:"), "label dropped: {out}");
        assert!(out.contains(REDACTED), "out = {out}");
        assert!(!out.contains("abcdefghijklmnop0123456789"), "leak: {out}");
    }

    #[test]
    fn redacts_key_eq_value() {
        let line = "api_key=SUPERSECRETVALUE123456";
        let out = redact(line);
        assert!(out.starts_with("api_key="), "key name dropped: {out}");
        assert!(out.contains(REDACTED), "out = {out}");
        assert!(!out.contains("SUPERSECRETVALUE123456"), "leak: {out}");
    }

    #[test]
    fn redacts_json_token_value() {
        let line = "\"token\": \"abcdef123456789012\"";
        let out = redact(line);
        assert!(out.contains("\"token\""), "key name dropped: {out}");
        assert!(out.contains(REDACTED), "out = {out}");
        assert!(!out.contains("abcdef123456789012"), "leak: {out}");
        // Quotes preserved around the redacted value.
        assert!(out.contains("\"[REDACTED]\""), "quotes dropped: {out}");
    }

    #[test]
    fn redacts_jwt() {
        let jwt = "aaaaaaaaaa.bbbbbbbbbb.cccccccccccccccccccc";
        assert!(jwt.len() >= 40, "test jwt too short");
        let out = redact(jwt);
        assert!(out.contains(REDACTED), "out = {out}");
        assert!(!out.contains(jwt), "leak: {out}");
    }

    #[test]
    fn ignores_ordinary_prose() {
        let prose = "the quick brown fox jumps over the lazy dog";
        assert_eq!(redact(prose), prose);
    }

    #[test]
    fn ignores_short_identifier() {
        assert_eq!(redact("user_id=42"), "user_id=42");
        assert_eq!(redact("hello"), "hello");
    }

    #[test]
    fn preserves_git_sha() {
        let sha = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
        assert_eq!(sha.len(), 40, "sha length sanity");
        assert_eq!(redact(sha), sha, "git SHA must be preserved");
        assert!(!has_secret(sha));
    }

    #[test]
    fn has_secret_matches_redact() {
        assert!(has_secret("sk-abcdef0123456789ABCDEF0123"));
        assert!(has_secret("Authorization: Bearer abcdefghijklmnop0123456789"));
        assert!(!has_secret("the quick brown fox"));
        assert!(!has_secret("da39a3ee5e6b4b0d3255bfef95601890afd80709"));
    }

    #[test]
    fn multiline_preserves_structure() {
        let input = "name: example\napi_key=SUPERSECRETVALUE123456\ndone";
        let out = redact(input);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "name: example");
        assert!(lines[1].starts_with("api_key=") && lines[1].contains(REDACTED));
        assert_eq!(lines[2], "done");
    }
}
