//! Syntax highlighting via syntect. Syntaxes and theme are loaded once and reused; a file's
//! text is turned into owned ratatui `Line`s so the editor can render it without re-highlighting.

use std::path::Path;

use once_cell::sync::Lazy;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SynStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

static SYNTAXES: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);
static THEME: Lazy<Theme> = Lazy::new(|| {
    ThemeSet::load_defaults().themes["base16-ocean.dark"].clone()
});

/// Highlight `text` for the syntax inferred from `path`'s extension. Falls back to plain text.
pub fn highlight(path: &Path, text: &str) -> Vec<Line<'static>> {
    let syntax = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| SYNTAXES.find_syntax_by_extension(ext))
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());

    let mut h = HighlightLines::new(syntax, &THEME);
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(text) {
        match h.highlight_line(line, &SYNTAXES) {
            Ok(ranges) => lines.push(Line::from(
                ranges.iter().map(|(s, t)| span(*s, t)).collect::<Vec<_>>(),
            )),
            Err(_) => lines.push(Line::raw(strip_eol(line))),
        }
    }
    if lines.is_empty() {
        lines.push(Line::raw(""));
    }
    lines
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
