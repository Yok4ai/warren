//! Syntax highlighting via syntect, done **incrementally**: each line's parse + highlight state
//! is cached, so an edit only re-highlights from the changed line until the state re-converges
//! with the previous result. Whole-file re-highlight per keystroke would be far too slow.

use std::io::Cursor;
use std::path::Path;
use std::sync::RwLock;

use once_cell::sync::Lazy;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ropey::Rope;
use syntect::highlighting::{
    Color as SynColor, FontStyle, HighlightIterator, HighlightState, Highlighter,
    ScopeSelectors, Style as SynStyle, StyleModifier, Theme, ThemeItem, ThemeSettings, ThemeSet,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

// `bat`'s curated grammar + theme sets (via two-face): ~250 languages and a stack of polished
// themes, all as plain syntect sets so the rest of the pipeline is unchanged.
static SYNTAXES: Lazy<SyntaxSet> = Lazy::new(two_face::syntax::extra_newlines);
static THEMES: Lazy<EmbeddedLazyThemeSet> = Lazy::new(two_face::theme::extra);

// Official Tokyo Night variants (folke/tokyonight.nvim's sublime exports — two-face has no Tokyo
// Night). Bundled so the default theme is correct out of the box; they share an internal `name`,
// so we load each under our own key.
const TN_NIGHT: &str = include_str!("../assets/themes/tokyonight_night.tmTheme");
const TN_STORM: &str = include_str!("../assets/themes/tokyonight_storm.tmTheme");
const TN_MOON: &str = include_str!("../assets/themes/tokyonight_moon.tmTheme");
const TN_DAY: &str = include_str!("../assets/themes/tokyonight_day.tmTheme");

/// Bundled tmThemes plus any the user dropped in `~/.config/warren/themes/*.tmTheme` — the
/// "install a theme" path: grab an official Dracula/Catppuccin/etc. `.tmTheme` (or a converted
/// VS Code theme) and it becomes selectable by its own name.
static EXTRA: Lazy<ThemeSet> = Lazy::new(|| {
    let mut ts = ThemeSet::new();
    for (name, data) in [
        ("Tokyo Night", TN_NIGHT),
        ("Tokyo Storm", TN_STORM),
        ("Tokyo Moon", TN_MOON),
        ("Tokyo Day", TN_DAY),
    ] {
        if let Ok(t) = ThemeSet::load_from_reader(&mut Cursor::new(data.as_bytes())) {
            ts.themes.insert(name.to_string(), t);
        }
    }
    // Monochrome syntax variants — built from scratch (desaturating a real theme collapses tokens
    // to one grey, since hues differ but luminance doesn't). Token roles get distinct grey levels
    // plus bold/italic, so structure reads without color.
    ts.themes.insert("Mono".into(), mono_theme("Mono", 56, true));
    ts.themes.insert("Mono Soft".into(), mono_theme("Mono Soft", 30, false));
    ts.themes.insert("Mono Bold".into(), mono_theme("Mono Bold", 74, true));
    if let Some(dir) = dirs::config_dir().map(|d| d.join("warren").join("themes")) {
        let _ = std::fs::create_dir_all(&dir); // so the folder exists for the user to drop themes in
        let _ = ts.add_from_folder(&dir); // best-effort; each theme keyed by its own `name`
    }
    ts
});

/// Curated display name → two-face embedded theme, for the gallery and `set_named`.
static EMBEDDED: &[(&str, EmbeddedThemeName)] = {
    use EmbeddedThemeName as T;
    &[
        ("Dracula", T::Dracula),
        ("Nord", T::Nord),
        ("One Dark", T::TwoDark),
        ("One Half Dark", T::OneHalfDark),
        ("One Half Light", T::OneHalfLight),
        ("Catppuccin Mocha", T::CatppuccinMocha),
        ("Catppuccin Macchiato", T::CatppuccinMacchiato),
        ("Catppuccin Frappé", T::CatppuccinFrappe),
        ("Catppuccin Latte", T::CatppuccinLatte),
        ("Gruvbox Dark", T::GruvboxDark),
        ("Gruvbox Light", T::GruvboxLight),
        ("Monokai Extended", T::MonokaiExtended),
        ("Solarized Dark", T::SolarizedDark),
        ("Solarized Light", T::SolarizedLight),
        ("Zenburn", T::Zenburn),
        ("Sublime Snazzy", T::SublimeSnazzy),
        ("Dark Neon", T::DarkNeon),
        ("Coldark Dark", T::ColdarkDark),
        ("GitHub", T::Github),
    ]
};

/// Build a greyscale theme by role: each token category gets a grey level offset from a mid-grey
/// (`spread` controls how far bright/dim diverge) plus bold/italic emphasis (when `emphasize`),
/// so structure reads without any color. `(scope, offset, font_style)` — more-specific scopes win.
fn mono_theme(name: &str, spread: i32, emphasize: bool) -> Theme {
    const MID: i32 = 176;
    let g = |off: i32| {
        let v = (MID + off).clamp(72, 245) as u8;
        SynColor { r: v, g: v, b: v, a: 255 }
    };
    let bold = emphasize.then_some(FontStyle::BOLD);
    let rules: &[(&str, i32, Option<FontStyle>)] = &[
        ("comment", -spread, Some(FontStyle::ITALIC)),
        ("punctuation", -spread * 3 / 4, None),
        ("keyword.operator", -spread / 2, None),
        ("variable", 0, None),
        ("string", spread / 2, None),
        ("constant", spread * 3 / 4, None),
        ("constant.numeric", spread * 3 / 4, None),
        ("constant.language", spread * 3 / 4, bold),
        ("entity.name.function", spread, None),
        ("support.function", spread, None),
        ("entity.name.type", spread, bold),
        ("support.type", spread, bold),
        ("storage.type", spread, bold),
        ("entity.name.tag", spread, None),
        ("keyword", spread, bold),
        ("storage", spread, bold),
    ];
    let scopes = rules
        .iter()
        .filter_map(|(sc, off, fs)| {
            Some(ThemeItem {
                scope: sc.parse::<ScopeSelectors>().ok()?,
                style: StyleModifier {
                    foreground: Some(g(*off)),
                    background: None,
                    font_style: *fs,
                },
            })
        })
        .collect();
    Theme {
        name: Some(name.to_string()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(g(0)),
            ..ThemeSettings::default()
        },
        scopes,
    }
}

/// All selectable syntax-theme names (bundled + monochrome + user-installed + embedded), in a
/// stable order so the gallery's indices are consistent within a session.
pub fn theme_names() -> Vec<String> {
    let curated = [
        "Tokyo Night", "Tokyo Storm", "Tokyo Moon", "Tokyo Day", "Mono", "Mono Soft", "Mono Bold",
    ];
    let mut out: Vec<String> = curated
        .iter()
        .filter(|n| EXTRA.themes.contains_key(**n))
        .map(|n| n.to_string())
        .collect();
    // User-installed themes (anything in EXTRA not in the curated list), sorted.
    let mut user: Vec<String> = EXTRA
        .themes
        .keys()
        .filter(|k| !curated.contains(&k.as_str()))
        .cloned()
        .collect();
    user.sort();
    out.extend(user);
    out.extend(EMBEDDED.iter().map(|(n, _)| n.to_string()));
    out
}

/// Look up a theme by gallery name (bundled/user first, then embedded).
fn resolve(name: &str) -> Option<Theme> {
    if let Some(t) = EXTRA.themes.get(name) {
        return Some(t.clone());
    }
    EMBEDDED
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, e)| THEMES.get(*e).clone())
}

/// Set the active syntax theme by gallery name; returns whether it matched.
pub fn set_named(name: &str) -> bool {
    if let Some(theme) = resolve(name) {
        if let Ok(mut active) = ACTIVE.write() {
            *active = theme;
            return true;
        }
    }
    false
}

/// The syntect theme used for code colors. Swapped at runtime to track the warren UI theme
/// (see `set_syntax_theme`) so cycling the theme recolors code, not just the chrome.
static ACTIVE: Lazy<RwLock<Theme>> = Lazy::new(|| RwLock::new(theme_for("Tokyo Night")));

/// two-face's closest embedded theme for warren themes that have no bundled/user tmTheme.
fn embedded_for(warren_name: &str) -> EmbeddedThemeName {
    use EmbeddedThemeName as T;
    match warren_name {
        "Catppuccin" => T::CatppuccinMocha,
        "Dracula" | "Dracula Dark" => T::Dracula,
        "Gruvbox" => T::GruvboxDark,
        "Monochrome" => T::Zenburn,
        _ => T::TwoDark,
    }
}

fn theme_for(warren_name: &str) -> Theme {
    // Prefer a bundled/user tmTheme — by a mapped name, then by the UI theme's own name (so a
    // user can drop e.g. "Dracula.tmTheme" to override) — else two-face's closest embedded theme.
    let mapped = match warren_name {
        "Tokyo Night" => "Tokyo Night",
        "Tokyo Glow" => "Tokyo Storm",
        "Tokyo Glow Dark" => "Tokyo Night",
        "Monochrome" => "Mono",
        "Light" => "Tokyo Day",
        _ => warren_name,
    };
    if let Some(t) = EXTRA.themes.get(mapped).or_else(|| EXTRA.themes.get(warren_name)) {
        return t.clone();
    }
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
