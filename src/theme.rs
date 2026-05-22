//! The single built-in dark theme. Centralizes colors so the whole UI stays consistent
//! and a future theming phase has one place to vary.

use ratatui::style::Color;

pub struct Theme {
    pub accent: Color,
    pub fg: Color,
    pub dim: Color,
    pub border: Color,
    pub status_bg: Color,
    pub status_fg: Color,
}

pub const DARK: Theme = Theme {
    accent: Color::Rgb(0x7a, 0xa2, 0xf7),
    fg: Color::Rgb(0xc0, 0xca, 0xf5),
    dim: Color::Rgb(0x56, 0x5f, 0x89),
    border: Color::Rgb(0x3b, 0x42, 0x61),
    status_bg: Color::Rgb(0x1f, 0x23, 0x35),
    status_fg: Color::Rgb(0xa9, 0xb1, 0xd6),
};
