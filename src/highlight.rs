//! Syntax highlighting via syntect. Syntaxes and theme are loaded once and reused; a file's
//! text is turned into owned ratatui `Line`s so the editor can render it without re-highlighting.

use std::path::Path;

use once_cell::sync::Lazy;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ropey::Rope;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SynStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxReference;
use syntect::parsing::SyntaxSet;

static SYNTAXES: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);
static THEME: Lazy<Theme> = Lazy::new(|| {
    ThemeSet::load_defaults().themes["base16-ocean.dark"].clone()
});

fn syntax_for(path: &Path) -> &'static SyntaxReference {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| SYNTAXES.find_syntax_by_extension(ext))
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text())
}

/// Highlight a rope line-by-line so the resulting `Line` count matches `rope.len_lines()`,
/// keeping render indices aligned with the editor's cursor/line coordinates.
pub fn highlight_rope(path: &Path, rope: &Rope) -> Vec<Line<'static>> {
    let mut h = HighlightLines::new(syntax_for(path), &THEME);
    let mut lines: Vec<Line<'static>> = rope
        .lines()
        .map(|slice| highlight_one(&mut h, &slice.to_string()))
        .collect();
    if lines.is_empty() {
        lines.push(Line::raw(""));
    }
    lines
}

fn highlight_one(h: &mut HighlightLines, line: &str) -> Line<'static> {
    match h.highlight_line(line, &SYNTAXES) {
        Ok(ranges) => Line::from(ranges.iter().map(|(s, t)| span(*s, t)).collect::<Vec<_>>()),
        Err(_) => Line::raw(strip_eol(line)),
    }
}

fn span(style: SynStyle, text: &str) -> Span<'static> {
    let mut s = Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));
    if style.font_style.contains(FontStyle::BOLD) {
        s = s.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        s = s.add_modifier(Modifier::UNDERLINED);
    }
    Span::styled(strip_eol(text), s)
}

fn strip_eol(s: &str) -> String {
    s.trim_end_matches('\n').trim_end_matches('\r').to_string()
}
