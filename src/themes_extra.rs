//! Extra TUI color palettes for the smolcode TUI.
//!
//! These are well-known dark palettes (Catppuccin Mocha, Nord, Dracula,
//! Solarized Dark, and Rosé Pine) appended to the built-in theme set
//! (smol-dark, tokyo, gruvbox, mono). RGB values are derived directly from
//! each project's published hex codes.

use ratatui::style::Color;

use crate::theme::Theme;

/// Extra TUI palettes appended to the built-in set.
pub fn extra_themes() -> Vec<Theme> {
    vec![
        // Catppuccin Mocha — https://catppuccin.com/palette
        // mauve #cba6f7, text #cdd6f4, overlay0 #6c7086, green #a6e3a1,
        // subtext1 #bac2de, blue #89b4fa, lavender #b4befe, red #f38ba8,
        // surface1 #45475a.
        Theme {
            name: "catppuccin",
            accent: Color::Rgb(203, 166, 247),
            fg: Color::Rgb(205, 214, 244),
            dim: Color::Rgb(108, 112, 134),
            user: Color::Rgb(166, 227, 161),
            assistant: Color::Rgb(186, 194, 222),
            tool: Color::Rgb(137, 180, 250),
            ok: Color::Rgb(180, 190, 254),
            warn: Color::Rgb(243, 139, 168),
            border: Color::Rgb(69, 71, 90),
            mode_edit: Color::Rgb(137, 180, 250),
            mode_auto: Color::Rgb(250, 179, 135),
            mode_plan: Color::Rgb(203, 166, 247),
            bg_alt: Color::Rgb(49, 50, 68),
        },
        // Nord — https://www.nordtheme.com/docs/colors-and-palettes
        // nord8 #88c0d0 (accent), nord6 #eceff4 (fg), nord3 #4c566a (dim),
        // nord14 #a3be8c (green), nord4 #d8dee9 (assistant),
        // nord9 #81a1c1 (blue/tool), nord15 #b48ead (purple/ok),
        // nord11 #bf616a (red/warn), nord1 #3b4252 (surface/border).
        Theme {
            name: "nord",
            accent: Color::Rgb(136, 192, 208),
            fg: Color::Rgb(236, 239, 244),
            dim: Color::Rgb(76, 86, 106),
            user: Color::Rgb(163, 190, 140),
            assistant: Color::Rgb(216, 222, 233),
            tool: Color::Rgb(129, 161, 193),
            ok: Color::Rgb(180, 142, 173),
            warn: Color::Rgb(191, 97, 106),
            border: Color::Rgb(59, 66, 82),
            mode_edit: Color::Rgb(136, 192, 208),
            mode_auto: Color::Rgb(235, 203, 139),
            mode_plan: Color::Rgb(180, 142, 173),
            bg_alt: Color::Rgb(46, 52, 64),
        },
        // Dracula — https://draculatheme.com/contribute
        // purple #bd93f9 (accent), foreground #f8f8f2 (fg),
        // comment #6272a4 (dim), green #50fa7b (user),
        // cyan #8be9fd (tool), pink #ff79c6 (ok), red #ff5555 (warn),
        // current line #44475a (border).
        Theme {
            name: "dracula",
            accent: Color::Rgb(189, 147, 249),
            fg: Color::Rgb(248, 248, 242),
            dim: Color::Rgb(98, 114, 164),
            user: Color::Rgb(80, 250, 123),
            assistant: Color::Rgb(248, 248, 242),
            tool: Color::Rgb(139, 233, 253),
            ok: Color::Rgb(255, 121, 198),
            warn: Color::Rgb(255, 85, 85),
            border: Color::Rgb(68, 71, 90),
            mode_edit: Color::Rgb(139, 233, 253),
            mode_auto: Color::Rgb(255, 184, 108),
            mode_plan: Color::Rgb(189, 147, 249),
            bg_alt: Color::Rgb(40, 42, 54),
        },
        // Solarized Dark — https://ethanschoonover.com/solarized
        // blue #268bd2 (accent), base0 #839496 (fg),
        // base01 #586e75 (dim), green #859900 (user),
        // base1 #93a1a1 (assistant), cyan #2aa198 (tool),
        // violet #6c71c4 (ok), red #dc322f (warn), base02 #073642 (border).
        Theme {
            name: "solarized",
            accent: Color::Rgb(38, 139, 210),
            fg: Color::Rgb(131, 148, 150),
            dim: Color::Rgb(88, 110, 117),
            user: Color::Rgb(133, 153, 0),
            assistant: Color::Rgb(147, 161, 161),
            tool: Color::Rgb(42, 161, 152),
            ok: Color::Rgb(108, 113, 196),
            warn: Color::Rgb(220, 50, 47),
            border: Color::Rgb(7, 54, 66),
            mode_edit: Color::Rgb(38, 139, 210),
            mode_auto: Color::Rgb(181, 137, 0),
            mode_plan: Color::Rgb(108, 113, 196),
            bg_alt: Color::Rgb(0, 43, 54),
        },
        // Rosé Pine — https://rosepinetheme.com/palette
        // iris #c4a7e7 (accent), text #e0def4 (fg), muted #6e6a86 (dim),
        // pine #31748f used for green-ish but rose green-leaning is foam;
        // foam #9ccfd8 (tool/cyan), rose #ebbcba (warn-ish),
        // here: user = foam-green pine? Use real roles below.
        // base #191724, surface #1f1d2e, overlay #26233a (border),
        // love #eb6f92 (red/warn), gold #f6c177.
        Theme {
            name: "rose-pine",
            accent: Color::Rgb(196, 167, 231),
            fg: Color::Rgb(224, 222, 244),
            dim: Color::Rgb(110, 106, 134),
            user: Color::Rgb(49, 116, 143),
            assistant: Color::Rgb(144, 140, 170),
            tool: Color::Rgb(156, 207, 216),
            ok: Color::Rgb(196, 167, 231),
            warn: Color::Rgb(235, 111, 146),
            border: Color::Rgb(38, 35, 58),
            mode_edit: Color::Rgb(156, 207, 216),
            mode_auto: Color::Rgb(246, 193, 119),
            mode_plan: Color::Rgb(196, 167, 231),
            bg_alt: Color::Rgb(31, 29, 46),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn extra_themes_are_five_with_unique_nonempty_names() {
        let themes = extra_themes();
        assert_eq!(themes.len(), 5, "expected exactly 5 extra themes");

        let mut seen = HashSet::new();
        for theme in &themes {
            assert!(!theme.name.is_empty(), "theme name must be non-empty");
            assert!(
                seen.insert(theme.name),
                "theme name must be unique: {}",
                theme.name
            );
        }
    }
}
