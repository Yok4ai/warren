//! Color themes. `current()` returns the active theme; `cycle()` switches it at runtime. Each
//! theme carries a `bg` used when the solid-background toggle is on (otherwise the terminal's
//! own background shows through).

use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::style::Color;

pub struct Theme {
    pub name: &'static str,
    pub bg: Color,
    pub accent: Color,
    pub fg: Color,
    pub dim: Color,
    pub border: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    pub sel_bg: Color,
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
    pub diff_add_fg: Color,
    pub diff_del_fg: Color,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

pub const TOKYO_NIGHT: Theme = Theme {
    name: "Tokyo Night",
    bg: rgb(0x1a, 0x1b, 0x26),
    accent: rgb(0x7a, 0xa2, 0xf7),
    fg: rgb(0xc0, 0xca, 0xf5),
    dim: rgb(0x56, 0x5f, 0x89),
    border: rgb(0x3b, 0x42, 0x61),
    status_bg: rgb(0x1f, 0x23, 0x35),
    status_fg: rgb(0xa9, 0xb1, 0xd6),
    sel_bg: rgb(0x33, 0x46, 0x7c),
    diff_add_bg: rgb(0x1d, 0x35, 0x28),
    diff_del_bg: rgb(0x3a, 0x22, 0x28),
    diff_add_fg: rgb(0x9e, 0xd7, 0xa6),
    diff_del_fg: rgb(0xe6, 0xa0, 0xa6),
};

pub const GRUVBOX: Theme = Theme {
    name: "Gruvbox",
    bg: rgb(0x28, 0x28, 0x28),
    accent: rgb(0x83, 0xa5, 0x98),
    fg: rgb(0xeb, 0xdb, 0xb2),
    dim: rgb(0x92, 0x83, 0x74),
    border: rgb(0x50, 0x49, 0x45),
    status_bg: rgb(0x3c, 0x38, 0x36),
    status_fg: rgb(0xeb, 0xdb, 0xb2),
    sel_bg: rgb(0x45, 0x40, 0x3d),
    diff_add_bg: rgb(0x32, 0x36, 0x1a),
    diff_del_bg: rgb(0x3c, 0x1f, 0x1c),
    diff_add_fg: rgb(0xb8, 0xbb, 0x26),
    diff_del_fg: rgb(0xfb, 0x49, 0x34),
};

pub const LIGHT: Theme = Theme {
    name: "Light",
    bg: rgb(0xf5, 0xf5, 0xf5),
    accent: rgb(0x25, 0x63, 0xeb),
    fg: rgb(0x1f, 0x29, 0x37),
    dim: rgb(0x8a, 0x91, 0x9e),
    border: rgb(0xc6, 0xcc, 0xd6),
    status_bg: rgb(0xe5, 0xe7, 0xeb),
    status_fg: rgb(0x37, 0x41, 0x51),
    sel_bg: rgb(0xbf, 0xdb, 0xfe),
    diff_add_bg: rgb(0xdc, 0xfc, 0xe7),
    diff_del_bg: rgb(0xfe, 0xe2, 0xe2),
    diff_add_fg: rgb(0x16, 0x65, 0x34),
    diff_del_fg: rgb(0x99, 0x1b, 0x1b),
};

pub const CATPPUCCIN: Theme = Theme {
    name: "Catppuccin",
    bg: rgb(0x1e, 0x1e, 0x2e),
    accent: rgb(0x89, 0xb4, 0xfa),
    fg: rgb(0xcd, 0xd6, 0xf4),
    dim: rgb(0x6c, 0x70, 0x86),
    border: rgb(0x45, 0x47, 0x5a),
    status_bg: rgb(0x18, 0x18, 0x25),
    status_fg: rgb(0xba, 0xc2, 0xde),
    sel_bg: rgb(0x58, 0x5b, 0x70),
    diff_add_bg: rgb(0x20, 0x37, 0x2b),
    diff_del_bg: rgb(0x3a, 0x22, 0x2e),
    diff_add_fg: rgb(0xa6, 0xe3, 0xa1),
    diff_del_fg: rgb(0xf3, 0x8b, 0xa8),
};

pub const DRACULA: Theme = Theme {
    name: "Dracula",
    bg: rgb(0x28, 0x2a, 0x36),
    accent: rgb(0xbd, 0x93, 0xf9),
    fg: rgb(0xf8, 0xf8, 0xf2),
    dim: rgb(0x62, 0x72, 0xa4),
    border: rgb(0x44, 0x47, 0x5a),
    status_bg: rgb(0x21, 0x22, 0x2c),
    status_fg: rgb(0xf8, 0xf8, 0xf2),
    sel_bg: rgb(0x44, 0x47, 0x5a),
    diff_add_bg: rgb(0x1e, 0x3a, 0x2a),
    diff_del_bg: rgb(0x3a, 0x20, 0x2a),
    diff_add_fg: rgb(0x50, 0xfa, 0x7b),
    diff_del_fg: rgb(0xff, 0x55, 0x55),
};

pub const TOKYO_GLOW: Theme = Theme {
    name: "Tokyo Glow",
    bg: rgb(0x16, 0x16, 0x1e),
    accent: rgb(0x7d, 0xcf, 0xff),
    fg: rgb(0xc0, 0xca, 0xf5),
    dim: rgb(0x56, 0x5f, 0x89),
    border: rgb(0x29, 0x35, 0x5a),
    status_bg: rgb(0x1a, 0x1b, 0x26),
    status_fg: rgb(0xa9, 0xb1, 0xd6),
    sel_bg: rgb(0x2a, 0x3f, 0x76),
    diff_add_bg: rgb(0x1d, 0x35, 0x28),
    diff_del_bg: rgb(0x3a, 0x22, 0x28),
    diff_add_fg: rgb(0xb9, 0xf2, 0x7c),
    diff_del_fg: rgb(0xff, 0x9e, 0x9e),
};

pub static THEMES: &[&Theme] = &[
    &TOKYO_NIGHT,
    &TOKYO_GLOW,
    &CATPPUCCIN,
    &DRACULA,
    &GRUVBOX,
    &LIGHT,
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

pub fn current() -> &'static Theme {
    THEMES[CURRENT.load(Ordering::Relaxed) % THEMES.len()]
}

/// Switch to the next theme; returns its name.
pub fn cycle() -> &'static str {
    let next = (CURRENT.load(Ordering::Relaxed) + 1) % THEMES.len();
    CURRENT.store(next, Ordering::Relaxed);
    THEMES[next].name
}

/// Select a specific theme by index.
pub fn set(idx: usize) {
    if idx < THEMES.len() {
        CURRENT.store(idx, Ordering::Relaxed);
    }
}
