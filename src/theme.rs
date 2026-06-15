//! Color themes for the TUI.

use ratatui::style::Color;

#[derive(Clone)]
pub struct Theme {
    pub name: &'static str,
    pub accent: Color,
    pub fg: Color,
    pub dim: Color,
    pub user: Color,
    pub assistant: Color,
    pub tool: Color,
    pub ok: Color,
    pub warn: Color,
    pub border: Color,
    /// Mode badge backgrounds (edit = safe, auto = caution, plan = read-only).
    pub mode_edit: Color,
    pub mode_auto: Color,
    pub mode_plan: Color,
    /// Subtle panel/segment surface (headers, status chips, result blocks).
    pub bg_alt: Color,
}

impl Theme {
    /// Background color for the current working mode.
    pub fn mode_color(&self, read_only: bool, yolo: bool) -> Color {
        if read_only {
            self.mode_plan
        } else if yolo {
            self.mode_auto
        } else {
            self.mode_edit
        }
    }

    /// Short label for the current working mode.
    pub fn mode_label(read_only: bool, yolo: bool) -> &'static str {
        if read_only {
            "plan"
        } else if yolo {
            "auto"
        } else {
            "edit"
        }
    }

    /// Foreground color for a tool, by category (read/edit/shell/git/...).
    #[allow(dead_code)] // API for nerd-mode / future tool chips
    pub fn tool_color(&self, name: &str) -> Color {
        match name {
            "read_file" | "list_dir" | "search" | "outline" | "find_symbol" | "find_context"
            | "tree" | "repo_map" | "project_info" => self.tool,
            "write_file" | "str_replace" | "apply_patch" | "multi_edit" | "format_file" => self.warn,
            "run_shell" | "run_python" | "run_tests" | "web_fetch" => self.accent,
            n if n.starts_with("git_") => self.ok,
            "task" => self.assistant,
            _ => self.tool,
        }
    }
}

pub fn themes() -> Vec<Theme> {
    vec![
        Theme {
            name: "smol-dark",
            accent: Color::Magenta,
            fg: Color::Rgb(230, 233, 245),
            dim: Color::Rgb(120, 130, 160),
            user: Color::Green,
            assistant: Color::Rgb(200, 205, 225),
            tool: Color::Blue,
            ok: Color::Magenta,
            warn: Color::Yellow,
            border: Color::Rgb(60, 66, 100),
            mode_edit: Color::Rgb(86, 182, 255),
            mode_auto: Color::Rgb(255, 176, 64),
            mode_plan: Color::Rgb(187, 154, 247),
            bg_alt: Color::Rgb(28, 32, 48),
        },
        Theme {
            name: "tokyo",
            accent: Color::Rgb(125, 207, 255),
            fg: Color::Rgb(192, 202, 245),
            dim: Color::Rgb(86, 95, 137),
            user: Color::Rgb(158, 206, 106),
            assistant: Color::Rgb(169, 177, 214),
            tool: Color::Rgb(125, 207, 255),
            ok: Color::Rgb(187, 154, 247),
            warn: Color::Rgb(224, 175, 104),
            border: Color::Rgb(65, 72, 104),
            mode_edit: Color::Rgb(125, 207, 255),
            mode_auto: Color::Rgb(224, 175, 104),
            mode_plan: Color::Rgb(187, 154, 247),
            bg_alt: Color::Rgb(31, 35, 53),
        },
        Theme {
            name: "gruvbox",
            accent: Color::Rgb(254, 128, 25),
            fg: Color::Rgb(235, 219, 178),
            dim: Color::Rgb(146, 131, 116),
            user: Color::Rgb(184, 187, 38),
            assistant: Color::Rgb(213, 196, 161),
            tool: Color::Rgb(131, 165, 152),
            ok: Color::Rgb(250, 189, 47),
            warn: Color::Rgb(251, 73, 52),
            border: Color::Rgb(80, 73, 69),
            mode_edit: Color::Rgb(131, 165, 152),
            mode_auto: Color::Rgb(250, 189, 47),
            mode_plan: Color::Rgb(211, 134, 155),
            bg_alt: Color::Rgb(50, 48, 47),
        },
        Theme {
            name: "mono",
            accent: Color::White,
            fg: Color::Gray,
            dim: Color::DarkGray,
            user: Color::White,
            assistant: Color::Gray,
            tool: Color::Gray,
            ok: Color::White,
            warn: Color::Yellow,
            border: Color::DarkGray,
            mode_edit: Color::Rgb(120, 120, 120),
            mode_auto: Color::Rgb(210, 210, 210),
            mode_plan: Color::Rgb(80, 80, 80),
            bg_alt: Color::Rgb(22, 22, 22),
        },
    ]
}
