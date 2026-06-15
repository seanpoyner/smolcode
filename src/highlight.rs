//! Tiny, dependency-free, language-agnostic code highlighter (per line).
//! Colors strings, comments, numbers, and a common keyword set.

use ratatui::style::Color;

pub struct Hl {
    pub kw: Color,
    pub string: Color,
    pub comment: Color,
    pub num: Color,
    pub base: Color,
}

const KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "const", "static", "return", "if", "else", "for", "while", "loop", "match",
    "struct", "enum", "impl", "trait", "pub", "use", "mod", "self", "Self", "where", "async",
    "await", "move", "ref", "as", "in", "break", "continue", "type", "dyn", "def", "class",
    "import", "from", "lambda", "yield", "with", "try", "except", "finally", "raise", "pass",
    "function", "var", "let", "const", "new", "this", "export", "default", "public", "private",
    "static", "void", "int", "str", "bool", "float", "None", "True", "False", "true", "false",
    "null", "undefined", "and", "or", "not", "is", "print", "println", "echo",
];

fn is_kw(w: &str) -> bool {
    KEYWORDS.contains(&w)
}

pub fn segments(line: &str, hl: &Hl) -> Vec<(Color, String)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<(Color, String)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // line comment
        if c == '#' || (c == '/' && i + 1 < chars.len() && chars[i + 1] == '/') {
            out.push((hl.comment, chars[i..].iter().collect()));
            break;
        }
        // string literal
        if c == '"' || c == '\'' || c == '`' {
            let q = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == q {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push((hl.string, chars[start..i.min(chars.len())].iter().collect()));
            continue;
        }
        // identifier / keyword / number
        if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let color = if word.chars().next().map_or(false, |c| c.is_ascii_digit()) {
                hl.num
            } else if is_kw(&word) {
                hl.kw
            } else {
                hl.base
            };
            out.push((color, word));
            continue;
        }
        out.push((hl.base, c.to_string()));
        i += 1;
    }
    out
}
