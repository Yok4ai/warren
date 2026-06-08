//! Command palette: a fuzzy finder over workspace files, plus a command mode entered by
//! starting the query with `>` (VS Code style). Fuzzy matching via `nucleo-matcher`.

use std::path::{Path, PathBuf};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

/// Commands runnable from the palette's `>` mode.
#[derive(Clone, Copy)]
pub enum Command {
    NewFile,
    NewTerminal,
    TogglePanel,
    ToggleSidebar,
    ToggleEditor,
    Save,
    SelectAll,
    ToggleScrollbar,
    ToggleAutosave,
    TogglePreview,
    ToggleSolidBg,
    CycleTheme,
    CycleIcons,
    Update,
    SetTheme(usize),
    /// Pick a syntax (code-coloring) theme by index into `crate::highlight::theme_names()`.
    SetSyntaxTheme(usize),
    /// Revert the syntax theme to follow the UI theme.
    SyntaxThemeFollowUi,
    Help,
    Quit,
}

/// The command list shown in `>` mode (label → command), including a selector entry per theme.
pub fn commands() -> Vec<(String, Command)> {
    use Command::*;
    let mut v: Vec<(String, Command)> = [
        ("New file", NewFile),
        ("New terminal", NewTerminal),
        ("Toggle terminal panel", TogglePanel),
        ("Toggle sidebar", ToggleSidebar),
        ("Toggle editor", ToggleEditor),
        ("Save", Save),
        ("Select all", SelectAll),
        ("Toggle scrollbar", ToggleScrollbar),
        ("Toggle auto-save", ToggleAutosave),
        ("Toggle markdown preview", TogglePreview),
        ("Toggle solid background", ToggleSolidBg),
        ("Cycle theme", CycleTheme),
        ("Cycle icons (nerd / unicode / none)", CycleIcons),
        ("Update warren", Update),
        ("Help", Help),
        ("Quit", Quit),
    ]
    .into_iter()
    .map(|(l, c)| (l.to_string(), c))
    .collect();
    for (i, t) in crate::theme::THEMES.iter().enumerate() {
        v.push((format!("Theme: {}", t.name), SetTheme(i)));
    }
    // Syntax-theme gallery: one entry per available code-coloring theme, plus a "follow UI" reset.
    v.push(("Syntax: follow UI theme".to_string(), SyntaxThemeFollowUi));
    for (i, name) in crate::highlight::theme_names().into_iter().enumerate() {
        v.push((format!("Syntax: {name}"), SetSyntaxTheme(i)));
    }
    v
}

/// What the user picked.
pub enum Choice {
    File(PathBuf),
    Command(Command),
}

pub struct Palette {
    pub input: String,
    pub cursor: usize,
    pub selected: usize,
    files: Vec<String>,
    /// Current filtered results (file paths, or command labels in `>` mode).
    pub results: Vec<String>,
    matcher: Matcher,
}

impl Palette {
    pub fn new(files: Vec<String>) -> Self {
        let mut p = Self {
            input: String::new(),
            cursor: 0,
            selected: 0,
            files,
            results: Vec::new(),
            matcher: Matcher::new(Config::DEFAULT),
        };
        p.refilter();
        p
    }

    pub fn command_mode(&self) -> bool {
        self.input.starts_with('>')
    }

    fn query(&self) -> &str {
        if self.command_mode() {
            self.input[1..].trim_start()
        } else {
            &self.input
        }
    }

    pub fn refilter(&mut self) {
        let pat = Pattern::parse(self.query(), CaseMatching::Ignore, Normalization::Smart);
        self.results = if self.command_mode() {
            let labels: Vec<String> = commands().into_iter().map(|(l, _)| l).collect();
            pat.match_list(labels, &mut self.matcher)
                .into_iter()
                .map(|(s, _)| s)
                .collect()
        } else {
            pat.match_list(self.files.clone(), &mut self.matcher)
                .into_iter()
                .map(|(s, _)| s)
                .take(200)
                .collect()
        };
        if self.selected >= self.results.len() {
            self.selected = self.results.len().saturating_sub(1);
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.results.len() {
            self.selected += 1;
        }
    }

    fn char_to_byte(&self, ci: usize) -> usize {
        self.input
            .char_indices()
            .nth(ci)
            .map(|(b, _)| b)
            .unwrap_or(self.input.len())
    }

    pub fn insert_char(&mut self, c: char) {
        let at = self.char_to_byte(self.cursor);
        self.input.insert(at, c);
        self.cursor += 1;
        self.refilter();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let start = self.char_to_byte(self.cursor - 1);
            let end = self.char_to_byte(self.cursor);
            self.input.replace_range(start..end, "");
            self.cursor -= 1;
            self.refilter();
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.input.chars().count() {
            self.cursor += 1;
        }
    }

    /// Resolve the current selection (files are joined onto `root`).
    pub fn choose(&self, root: &Path) -> Option<Choice> {
        let sel = self.results.get(self.selected)?;
        if self.command_mode() {
            commands()
                .into_iter()
                .find(|(l, _)| l == sel)
                .map(|(_, c)| Choice::Command(c))
        } else {
            Some(Choice::File(root.join(sel)))
        }
    }
}

/// Walk `root` collecting workspace-relative file paths, skipping noisy dirs, capped.
pub fn gather_files(root: &Path) -> Vec<String> {
    const IGNORED: &[&str] = &[".git", "target", "node_modules"];
    const CAP: usize = 10_000;
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            if out.len() >= CAP {
                out.sort();
                return out;
            }
            let path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !IGNORED.contains(&name.as_str()) {
                    stack.push(path);
                }
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    out
}
