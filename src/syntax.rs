//! Per-token syntax highlighting of fenced code blocks via the `syntect` crate.
//!
//! Replaces the dependency-free `crate::highlight` line highlighter. The public
//! `highlight` fn takes a whole code block plus its fence info string and returns
//! one Vec of styled `(ratatui Color, String)` segments per input line, so the
//! TUI can map each segment to a `Span`.
//!
//! `SyntaxSet` and `ThemeSet` are loaded once (lazily) because building them is
//! expensive. Everything is best-effort: any error or edge case degrades to the
//! input rendered as plain, neutral-colored lines. Never panics.

use ratatui::style::Color;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Neutral fallback color used when highlighting is unavailable or fails.
const NEUTRAL: Color = Color::Gray;

/// Name of the bundled syntect theme to colorize with.
const THEME_NAME: &str = "base16-ocean.dark";

fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    static TS: OnceLock<ThemeSet> = OnceLock::new();
    TS.get_or_init(ThemeSet::load_defaults)
}

/// Each input line, returned as a single plain segment in the neutral color.
/// Used as the universal fallback when syntect cannot help.
fn plain(code: &str) -> Vec<Vec<(Color, String)>> {
    code.split('\n')
        .map(|line| vec![(NEUTRAL, line.to_string())])
        .collect()
}

/// Highlight one code block. Returns one Vec of styled segments per input line:
/// each segment is (ratatui::style::Color, String). `lang` is the fence info
/// string (e.g. "python", "rust", "" if none); map it to a syntect syntax by
/// token/extension, falling back to plain text. Never panics.
pub fn highlight(code: &str, lang: &str) -> Vec<Vec<(Color, String)>> {
    let ss = syntax_set();
    let ts = theme_set();

    // Pick a theme; if the named one is absent for any reason, degrade to plain.
    let theme = match ts.themes.get(THEME_NAME) {
        Some(t) => t,
        None => return plain(code),
    };

    // Resolve the syntax: by fence token, then by extension, then plain text.
    let lang = lang.trim();
    let syntax = ss
        .find_syntax_by_token(lang)
        .or_else(|| ss.find_syntax_by_extension(lang))
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let mut hl = HighlightLines::new(syntax, theme);
    let mut out: Vec<Vec<(Color, String)>> = Vec::new();

    for line in LinesWithEndings::from(code) {
        match hl.highlight_line(line, ss) {
            Ok(ranges) => {
                let mut segs: Vec<(Color, String)> = Vec::new();
                for (style, text) in ranges {
                    let text = text.trim_end_matches('\n');
                    if text.is_empty() {
                        continue;
                    }
                    let fg = style.foreground;
                    segs.push((Color::Rgb(fg.r, fg.g, fg.b), text.to_string()));
                }
                out.push(segs);
            }
            // On any per-line failure, fall back to the raw line, neutral-colored.
            Err(_) => out.push(vec![(NEUTRAL, line.trim_end_matches('\n').to_string())]),
        }
    }

    // Edge case: empty input (or no lines produced) → mirror plain() shape so the
    // caller always gets at least the lines it passed in.
    if out.is_empty() {
        return plain(code);
    }
    out
}
