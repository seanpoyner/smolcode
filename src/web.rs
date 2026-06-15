//! `web_fetch` tool: fetch a URL and return readable text.
//!
//! No HTTP-client crate is available, so this shells out to the `curl`
//! binary (assumed installed). HTML bodies are reduced to plain text;
//! other bodies pass through. Output is size-capped. Pure std + `curl`.

use std::process::Command;

const MAX_TEXT_CHARS: usize = 8000;

/// Fetch `url` and return readable text. HTML is reduced to plain text
/// (tags stripped, scripts/styles removed, entities decoded, whitespace
/// collapsed); non-HTML bodies are returned as-is. Output is size-capped.
/// Returns an error-prefixed string ("error: ...") on failure.
pub fn fetch(url: &str) -> String {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return "error: only http(s) URLs are supported".to_string();
    }

    let output = Command::new("curl")
        .args([
            "-s",
            "-L",
            "--max-time",
            "20",
            "--max-filesize",
            "5000000",
            "-A",
            "smolcode/0.1",
            url,
        ])
        .output();

    let output = match output {
        Ok(o) => o,
        Err(_) => return "error: curl not available".to_string(),
    };

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first = stderr.lines().next().unwrap_or("").trim();
        return format!("error: fetch failed (curl exit {code}): {first}");
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let text = if looks_like_html(&body) {
        html_to_text(&body)
    } else {
        body.into_owned()
    };

    let text = cap_chars(&text, MAX_TEXT_CHARS);
    format!("# {url}\n\n{text}")
}

/// Sniff the first non-whitespace bytes for HTML markers.
fn looks_like_html(body: &str) -> bool {
    let head: String = body.chars().take(1024).collect::<String>().to_lowercase();
    head.contains("<!doctype html") || head.contains("<html") || head.contains("<body")
}

/// Truncate to `max` chars on a char boundary, appending a note if cut.
fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n...[truncated]");
    out
}

/// Reduce an HTML document to readable plain text.
fn html_to_text(html: &str) -> String {
    let mut s = strip_block(html, "script");
    s = strip_block(&s, "style");

    // Turn block-ending tags into newlines before stripping the rest.
    for tag in [
        "<br>", "<br/>", "<br />", "</p>", "</div>", "</li>", "</tr>", "</h1>", "</h2>",
        "</h3>", "</h4>", "</h5>", "</h6>",
    ] {
        s = replace_ci(&s, tag, "\n");
    }

    // Strip all remaining tags via a char scan.
    let mut text = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }

    let text = decode_entities(&text);
    collapse_whitespace(&text)
}

/// Remove `<tag>...</tag>` blocks (case-insensitive), including nested text.
fn strip_block(html: &str, tag: &str) -> String {
    let lower = html.to_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut i = 0usize;
    while i < html.len() {
        if let Some(rel) = lower[i..].find(&open) {
            let start = i + rel;
            out.push_str(&html[i..start]);
            // Find the matching close tag after the open.
            if let Some(crel) = lower[start..].find(&close) {
                i = start + crel + close.len();
            } else {
                // No close tag; drop the rest.
                i = html.len();
            }
        } else {
            out.push_str(&html[i..]);
            break;
        }
    }
    out
}

/// Case-insensitive literal replace of `needle` with `repl`.
fn replace_ci(haystack: &str, needle: &str, repl: &str) -> String {
    let lower = haystack.to_lowercase();
    let needle_l = needle.to_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0usize;
    while i < haystack.len() {
        if let Some(rel) = lower[i..].find(&needle_l) {
            let start = i + rel;
            out.push_str(&haystack[i..start]);
            out.push_str(repl);
            i = start + needle.len();
        } else {
            out.push_str(&haystack[i..]);
            break;
        }
    }
    out
}

/// Decode the common HTML entities.
fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// Trim trailing spaces per line, collapse 3+ spaces to one, and reduce
/// runs of blank lines to a single blank line.
fn collapse_whitespace(s: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut blank_run = 0usize;
    for raw in s.lines() {
        let line = squeeze_spaces(raw.trim_end());
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                lines.push(String::new());
            }
        } else {
            blank_run = 0;
            lines.push(line);
        }
    }
    lines.join("\n").trim().to_string()
}

/// Collapse runs of 3+ spaces to a single space.
fn squeeze_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = 0usize;
    for c in s.chars() {
        if c == ' ' {
            run += 1;
            if run < 3 {
                out.push(' ');
            }
        } else {
            run = 0;
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_scheme() {
        assert_eq!(
            fetch("ftp://x"),
            "error: only http(s) URLs are supported"
        );
    }

    #[test]
    fn html_reduced_to_text() {
        let html = "\
<html><head><style>.a{color:red}</style></head>\
<body><script>var x = 1 < 2;</script>\
<p>Hello &amp; welcome</p><br>\
<p>1 &lt; 2 and&nbsp;done</p></body></html>";
        let out = html_to_text(html);
        assert!(!out.contains("color:red"), "style content leaked: {out}");
        assert!(!out.contains("var x"), "script content leaked: {out}");
        // HTML tags must be gone. (A bare '<' can legitimately remain after
        // decoding &lt; — e.g. "1 < 2" — so check for actual tags, not '<'.)
        for tag in ["<p>", "<br", "<html", "<body", "<script", "<style", "</p>"] {
            assert!(!out.contains(tag), "tag {tag} not stripped: {out}");
        }
        assert!(out.contains("Hello & welcome"), "entity/text missing: {out}");
        assert!(out.contains("1 < 2 and done"), "entity/text missing: {out}");
    }

    #[test]
    fn plain_text_passes_through() {
        let plain = "just some plain text, no markup here";
        assert_eq!(html_to_text(plain), plain);
        assert!(!looks_like_html(plain));
    }

    #[test]
    fn html_is_detected() {
        assert!(looks_like_html("<!DOCTYPE html><html>"));
        assert!(looks_like_html("  <BODY>"));
    }
}
