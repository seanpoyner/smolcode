//! Glyph sets for the TUI. Two variants: an ascii-safe set (plain Unicode
//! box/braille that renders in any modern font, the default) and a Nerd-Font
//! set (devicons/powerline). Selected at startup via `SMOLCODE_GLYPHS=nerd`.

#[derive(Clone, Copy)]
pub struct Glyphs {
    pub user: &'static str,
    pub assistant: &'static str,
    pub tool: &'static str,
    pub result: &'static str,
    pub final_: &'static str,
    pub error: &'static str,
    pub info: &'static str,
    pub branch: &'static str,
    pub dirty: &'static str,
    pub dir: &'static str,
    pub file: &'static str,
    /// Left accent bar for message cards.
    pub bar: &'static str,
    /// Powerline separators (only meaningful in nerd mode; empty otherwise).
    pub chip_l: &'static str,
    pub chip_r: &'static str,
    /// Spinner frames for the activity indicator.
    pub spinner: &'static [&'static str],
    /// Pulsing star frames for the "thinking" indicator (Claude-Code style).
    pub thinking: &'static [&'static str],
}

const BRAILLE_SPIN: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const STAR_PULSE: &[&str] = &["✶", "✸", "✹", "✺", "✹", "✷"];

impl Glyphs {
    /// Plain-Unicode, no Nerd Font required. The default.
    pub fn ascii() -> Self {
        Glyphs {
            user: "▌",
            assistant: "│",
            tool: "▸",
            result: "↳",
            final_: "●",
            error: "⚠",
            info: "·",
            branch: "⎇",
            dirty: "●",
            dir: "▾",
            file: " ",
            bar: "▌",
            chip_l: "",
            chip_r: "",
            spinner: BRAILLE_SPIN,
            thinking: STAR_PULSE,
        }
    }

    /// Nerd-Font devicons + powerline separators.
    pub fn nerd() -> Self {
        Glyphs {
            user: "",
            assistant: "",
            tool: "",
            result: "",
            final_: "",
            error: "",
            info: "",
            branch: "",
            dirty: "",
            dir: "",
            file: "",
            bar: "▌",
            chip_l: "",
            chip_r: "",
            spinner: BRAILLE_SPIN,
            thinking: STAR_PULSE,
        }
    }

    /// Pick the set from the `SMOLCODE_GLYPHS` env var (default ascii-safe).
    pub fn from_env() -> Self {
        match std::env::var("SMOLCODE_GLYPHS").ok().as_deref() {
            Some("nerd") => Self::nerd(),
            _ => Self::ascii(),
        }
    }

    /// Whether powerline separators are in use (nerd mode).
    #[allow(dead_code)] // API for nerd-mode / future tool chips
    pub fn powerline(&self) -> bool {
        !self.chip_l.is_empty()
    }
}
