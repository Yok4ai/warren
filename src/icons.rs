//! File-type icons for the explorer/tabs/palette. Three styles (see `IconStyle`):
//! `Nerd` emits Nerd Font glyphs with per-language brand colors — the nvim-web-devicons look,
//! and the default; `Unicode` is the geometric-symbol fallback for terminals without a Nerd
//! Font; `None` drops glyphs entirely. Glyphs are written as `\u{...}` so the source stays
//! ASCII-clean; tweak any that render wrong in your terminal.

use ratatui::style::Color;

use crate::theme::current as dark;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconStyle {
    /// Nerd Font glyphs + brand colors (default).
    #[default]
    Nerd,
    /// Geometric Unicode that renders in any font.
    Unicode,
    /// No icons.
    None,
}

impl IconStyle {
    /// Parse the `icons` config value; unknown/absent → `Nerd`.
    pub fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("nerd").trim().to_ascii_lowercase().as_str() {
            "unicode" | "geometric" => IconStyle::Unicode,
            "none" | "off" | "" => IconStyle::None,
            _ => IconStyle::Nerd,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            IconStyle::Nerd => "nerd",
            IconStyle::Unicode => "unicode",
            IconStyle::None => "none",
        }
    }

    /// Next style in the round-robin nerd → unicode → none → nerd (for the palette toggle).
    pub fn cycle(self) -> Self {
        match self {
            IconStyle::Nerd => IconStyle::Unicode,
            IconStyle::Unicode => IconStyle::None,
            IconStyle::None => IconStyle::Nerd,
        }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// Icon + color for a tree row, chosen by the active `IconStyle`.
pub fn file_icon(name: &str, is_dir: bool, expanded: bool, style: IconStyle) -> (&'static str, Color) {
    match style {
        IconStyle::Nerd => nerd_icon(name, is_dir, expanded),
        IconStyle::Unicode => unicode_icon(name, is_dir, expanded),
        IconStyle::None => ("", dark().fg),
    }
}

/// The original geometric set — safe in any font.
fn unicode_icon(name: &str, is_dir: bool, expanded: bool) -> (&'static str, Color) {
    if is_dir {
        return (if expanded { "\u{25be}" } else { "\u{25b8}" }, dark().accent);
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "md" | "markdown" | "txt" | "rst" => ("\u{25c6}", dark().fg),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" => ("\u{25a3}", dark().accent),
        "toml" | "yaml" | "yml" | "json" | "ini" | "cfg" | "conf" | "lock" => ("\u{25c7}", dark().dim),
        _ => ("\u{25cf}", dark().fg),
    }
}

/// Nerd Font glyph + brand color. Matches by exact filename first (Cargo.toml, Dockerfile, …),
/// then by extension. Brand colors are intentionally theme-independent — that's what makes the
/// tree pop the way nvim-web-devicons does.
fn nerd_icon(name: &str, is_dir: bool, expanded: bool) -> (&'static str, Color) {
    if is_dir {
        // Default folder; a few well-known dirs get a recognizable glyph.
        let lower = name.to_ascii_lowercase();
        let col = dark().accent;
        return match lower.as_str() {
            ".git" => ("\u{e5fb}", rgb(0xf5, 0x4d, 0x27)), //
            "node_modules" => ("\u{e5fa}", rgb(0x8c, 0xc8, 0x4b)), //
            "src" | "lib" => (if expanded { "\u{e5fe}" } else { "\u{e5ff}" }, col),
            _ => (if expanded { "\u{f07c}" } else { "\u{f07b}" }, col), //  /
        };
    }

    let lower = name.to_ascii_lowercase();
    // Exact-filename special cases.
    match lower.as_str() {
        "cargo.toml" | "cargo.lock" => return ("\u{e7a8}", rgb(0xde, 0xa5, 0x84)), //  rust
        "package.json" | "package-lock.json" => return ("\u{e71e}", rgb(0xe8, 0x27, 0x4b)), //  npm
        "tsconfig.json" => return ("\u{e628}", rgb(0x51, 0x9a, 0xba)), //  ts
        "dockerfile" | ".dockerignore" => return ("\u{e7b0}", rgb(0x45, 0x8e, 0xe6)), //
        "makefile" => return ("\u{e673}", rgb(0x6d, 0x80, 0x86)),
        ".gitignore" | ".gitattributes" | ".gitmodules" => return ("\u{e702}", rgb(0xf5, 0x4d, 0x27)), //  git
        "readme" | "readme.md" => return ("\u{f02d}", rgb(0x51, 0x9a, 0xba)), //  book
        "license" | "license.md" | "copying" => return ("\u{f718}", rgb(0xcb, 0xcb, 0x41)),
        _ => {}
    }
    if lower.starts_with(".env") {
        return ("\u{f013}", rgb(0xfa, 0xf7, 0x43)); //  gear
    }

    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => ("\u{e7a8}", rgb(0xde, 0xa5, 0x84)),
        "lua" => ("\u{e620}", rgb(0x51, 0xa0, 0xcf)),
        "py" | "pyw" | "pyi" => ("\u{e606}", rgb(0xff, 0xbc, 0x03)),
        "js" | "mjs" | "cjs" => ("\u{e74e}", rgb(0xcb, 0xcb, 0x41)),
        "ts" => ("\u{e628}", rgb(0x51, 0x9a, 0xba)),
        "tsx" | "jsx" => ("\u{e7ba}", rgb(0x51, 0x9a, 0xba)),
        "json" | "jsonc" => ("\u{e60b}", rgb(0xcb, 0xcb, 0x41)),
        "html" | "htm" => ("\u{e736}", rgb(0xe4, 0x4d, 0x26)),
        "css" => ("\u{e749}", rgb(0x56, 0x3d, 0x7c)),
        "scss" | "sass" => ("\u{e603}", rgb(0xf5, 0x53, 0x85)),
        "c" => ("\u{e61e}", rgb(0x59, 0x9e, 0xff)),
        "cpp" | "cc" | "cxx" => ("\u{e61d}", rgb(0x51, 0x9a, 0xba)),
        "h" | "hpp" | "hxx" => ("\u{f0fd}", rgb(0xa0, 0x74, 0xc4)),
        "go" => ("\u{e627}", rgb(0x51, 0x9a, 0xba)),
        "rb" => ("\u{e739}", rgb(0x70, 0x15, 0x16)),
        "java" => ("\u{e738}", rgb(0xcc, 0x3e, 0x44)),
        "kt" | "kts" => ("\u{e634}", rgb(0x7f, 0x52, 0xff)),
        "swift" => ("\u{e699}", rgb(0xe3, 0x79, 0x33)),
        "php" => ("\u{e73d}", rgb(0xa0, 0x74, 0xc4)),
        "sh" | "bash" | "zsh" | "fish" => ("\u{f489}", rgb(0x89, 0xe0, 0x51)),
        "sql" => ("\u{e706}", rgb(0xda, 0xd8, 0xd8)),
        "md" | "markdown" => ("\u{e73e}", rgb(0x51, 0x9a, 0xba)),
        "rst" | "txt" | "text" => ("\u{f15c}", rgb(0x6d, 0x80, 0x86)),
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" => ("\u{f013}", rgb(0x6d, 0x80, 0x86)),
        "lock" => ("\u{f023}", rgb(0xbb, 0xbb, 0xbb)),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" => {
            ("\u{f1c5}", rgb(0xa0, 0x74, 0xc4))
        }
        "pdf" => ("\u{f1c1}", rgb(0xb3, 0x0b, 0x00)),
        "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" => ("\u{f1c6}", rgb(0xec, 0xa5, 0x17)),
        _ => ("\u{f15b}", dark().fg), //  generic file
    }
}
