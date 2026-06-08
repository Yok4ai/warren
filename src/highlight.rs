//! Syntax highlighting via syntect, done **incrementally**: each line's parse + highlight state
//! is cached, so an edit only re-highlights from the changed line until the state re-converges
//! with the previous result. Whole-file re-highlight per keystroke would be far too slow.

use std::path::Path;
use std::sync::RwLock;

use once_cell::sync::Lazy;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ropey::Rope;
use syntect::highlighting::{
    FontStyle, HighlightIterator, HighlightState, Highlighter, Style as SynStyle, Theme,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

// `bat`'s curated grammar + theme sets (via two-face): ~250 languages and a stack of polished
// themes, all as plain syntect sets so the rest of the pipeline is unchanged.
static SYNTAXES: Lazy<SyntaxSet> = Lazy::new(two_face::syntax::extra_newlines);
static THEMES: Lazy<EmbeddedLazyThemeSet> = Lazy::new(two_face::theme::extra);
/// The syntect theme used for code colors. Swapped at runtime to track the warren UI theme
/// (see `set_syntax_theme`) so cycling the theme recolors code, not just the chrome.
static ACTIVE: Lazy<RwLock<Theme>> = Lazy::new(|| RwLock::new(theme_for("Tokyo Night")));

/// Pick the bundled theme closest to a warren UI theme. (No native Tokyo Night in the set, so it
/// maps to TwoDark — a clean, VS-Code-ish dark scheme.)
fn embedded_for(warren_name: &str) -> EmbeddedThemeName {
    use EmbeddedThemeName as T;
    match warren_name {
        "Tokyo Glow" => T::Nord,
        "Catppuccin" => T::CatppuccinMocha,
        "Dracula" => T::Dracula,
        "Gruvbox" => T::GruvboxDark,
        "Monochrome" => T::Zenburn,
        "Light" => T::Github,
        _ => T::TwoDark, // Tokyo Night + fallback
    }
}

fn theme_for(warren_name: &str) -> Theme {
    THEMES.get(embedded_for(warren_name)).clone()
}

/// Point the highlighter at the theme matching the given warren UI theme. The editor must
/// re-highlight open buffers afterward (`Editor::rehighlight_all`) for the change to show.
pub fn set_syntax_theme(warren_name: &str) {
    if let Ok(mut active) = ACTIVE.write() {
        *active = theme_for(warren_name);
    }
}

/// Parse + highlight state captured *before* a given line.
pub type LineState = (ParseState, HighlightState);

pub fn syntax_for(path: &Path) -> &'static SyntaxReference {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| SYNTAXES.find_syntax_by_extension(ext))
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text())
}

/// Highlight an entire rope from scratch, returning the rendered lines plus the per-line states
/// (`states[i]` is the state before line `i`; `states.len() == lines.len() + 1`).
pub fn full(syntax: &SyntaxReference, rope: &Rope) -> (Vec<Line<'static>>, Vec<LineState>) {
    let active = ACTIVE.read().expect("highlight theme lock poisoned");
    let hl = Highlighter::new(&active);
    let mut ps = ParseState::new(syntax);
    let mut hs = HighlightState::new(&hl, ScopeStack::new());
    let mut lines = Vec::with_capacity(rope.len_lines());
    let mut states = Vec::with_capacity(rope.len_lines() + 1);
    states.push((ps.clone(), hs.clone()));
    for line in rope.lines() {
        lines.push(highlight_one(&mut ps, &mut hs, &line.to_string(), &hl));
        states.push((ps.clone(), hs.clone()));
    }
    if lines.is_empty() {
        lines.push(Line::raw(""));
        states.push((ps, hs));
    }
    (lines, states)
}

/// Re-highlight after an edit. If the line count changed (newline/join/paste) we rebuild from
/// scratch; otherwise we re-highlight from `from` and stop as soon as a line's resulting state
/// matches what it was before — so typing a character costs ~one line, not the whole file.
pub fn incremental(
    syntax: &SyntaxReference,
    lines: &mut Vec<Line<'static>>,
    states: &mut Vec<LineState>,
    rope: &Rope,
    from: usize,
) {
    let total = rope.len_lines();
    if total != lines.len() || states.len() != total + 1 {
        let (l, s) = full(syntax, rope);
        *lines = l;
        *states = s;
        return;
    }
    let active = ACTIVE.read().expect("highlight theme lock poisoned");
    let hl = Highlighter::new(&active);
    let from = from.min(total.saturating_sub(1));
    let (mut ps, mut hs) = states[from].clone();
    let mut i = from;
    while i < total {
        lines[i] = highlight_one(&mut ps, &mut hs, &rope.line(i).to_string(), &hl);
        let next = (ps.clone(), hs.clone());
        let converged = states[i + 1] == next;
        states[i + 1] = next;
        i += 1;
        if converged {
            break;
        }
    }
}

fn highlight_one(
    ps: &mut ParseState,
    hs: &mut HighlightState,
    line: &str,
    hl: &Highlighter,
) -> Line<'static> {
    let ops = ps.parse_line(line, &SYNTAXES).unwrap_or_default();
    let spans: Vec<Span<'static>> = HighlightIterator::new(hs, &ops, line, hl)
        .map(|(style, text)| span(style, text))
        .collect();
    if spans.is_empty() {
        Line::raw("")
    } else {
        Line::from(spans)
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
