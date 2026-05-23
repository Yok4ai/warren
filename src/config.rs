//! Configuration: a keymap loaded from `~/.config/warren/config.toml`. On first run the
//! default config is written to disk so the user has something to edit. Unknown/omitted
//! keys fall back to the built-in defaults.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;

/// A key combination, e.g. `ctrl+q`, `alt+left`, `f12`, `ctrl+shift+e`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyChord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyChord {
    pub const fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        Self { code, mods }
    }

    /// True if a crossterm key event matches this chord (ignoring NONE-vs-absent nuances).
    pub fn matches(&self, code: KeyCode, mods: KeyModifiers) -> bool {
        self.code == code && self.mods == mods
    }
}

impl FromStr for KeyChord {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut mods = KeyModifiers::NONE;
        let mut code = None;
        for part in s.split('+').map(|p| p.trim().to_ascii_lowercase()) {
            match part.as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "option" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                "super" | "cmd" | "meta" => mods |= KeyModifiers::SUPER,
                token => {
                    code = Some(parse_code(token)?);
                }
            }
        }
        code.map(|code| KeyChord { code, mods })
            .ok_or_else(|| format!("no key in chord: {s:?}"))
    }
}

fn parse_code(token: &str) -> Result<KeyCode, String> {
    Ok(match token {
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "backspace" | "bs" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        f if f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
            KeyCode::F(f[1..].parse().unwrap())
        }
        c if c.chars().count() == 1 => KeyCode::Char(c.chars().next().unwrap()),
        other => return Err(format!("unknown key: {other:?}")),
    })
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            write!(f, "ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            write!(f, "alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            write!(f, "shift+")?;
        }
        match self.code {
            KeyCode::Char(' ') => write!(f, "space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "f{n}"),
            other => write!(f, "{}", format!("{other:?}").to_ascii_lowercase()),
        }
    }
}

/// Resolved keybindings used across the app. Fields are added as phases need them.
#[derive(Debug, Clone)]
pub struct Keymap {
    pub quit: KeyChord,
    pub command_palette: KeyChord,
    pub toggle_sidebar: KeyChord,
    pub toggle_editor: KeyChord,
    pub toggle_scm: KeyChord,
    pub new_terminal: KeyChord,
    pub focus_next: KeyChord,
    pub next_tab: KeyChord,
    pub prev_tab: KeyChord,
    pub close_tab: KeyChord,
    pub save: KeyChord,
    pub new_file: KeyChord,
    pub toggle_scrollbar: KeyChord,
    pub select_all: KeyChord,
    pub toggle_autosave: KeyChord,
    pub toggle_panel: KeyChord,
    pub help: KeyChord,
}

impl Default for Keymap {
    fn default() -> Self {
        use KeyCode::*;
        let ctrl = KeyModifiers::CONTROL;
        Self {
            quit: KeyChord::new(Char('q'), ctrl),
            command_palette: KeyChord::new(Char('p'), ctrl),
            toggle_sidebar: KeyChord::new(Char('b'), ctrl),
            toggle_editor: KeyChord::new(Char('e'), KeyModifiers::ALT),
            toggle_scm: KeyChord::new(Char('g'), ctrl),
            new_terminal: KeyChord::new(Char('t'), ctrl),
            focus_next: KeyChord::new(Char('w'), ctrl),
            next_tab: KeyChord::new(PageDown, ctrl),
            prev_tab: KeyChord::new(PageUp, ctrl),
            close_tab: KeyChord::new(Char('x'), ctrl),
            save: KeyChord::new(Char('s'), ctrl),
            new_file: KeyChord::new(Char('n'), ctrl),
            toggle_scrollbar: KeyChord::new(Char('s'), KeyModifiers::ALT),
            select_all: KeyChord::new(Char('a'), ctrl),
            toggle_autosave: KeyChord::new(Char('a'), KeyModifiers::ALT),
            toggle_panel: KeyChord::new(Char('`'), ctrl),
            help: KeyChord::new(F(1), KeyModifiers::NONE),
        }
    }
}

impl Keymap {
    /// All bindings as `(label, chord)` for the help overlay / discoverability.
    pub fn bindings(&self) -> Vec<(&'static str, KeyChord)> {
        vec![
            ("New file", self.new_file),
            ("Save", self.save),
            ("Close tab", self.close_tab),
            ("Next / prev tab", self.next_tab),
            ("Select all", self.select_all),
            ("Cycle focus", self.focus_next),
            ("Toggle sidebar", self.toggle_sidebar),
            ("Toggle editor", self.toggle_editor),
            ("Source control", self.toggle_scm),
            ("Toggle scrollbar", self.toggle_scrollbar),
            ("Toggle auto-save", self.toggle_autosave),
            ("Command palette", self.command_palette),
            ("New terminal", self.new_terminal),
            ("Toggle terminal panel", self.toggle_panel),
            ("Help", self.help),
            ("Quit", self.quit),
        ]
    }

    /// Overlay string overrides (action -> chord) onto the defaults; bad entries are ignored.
    fn apply(&mut self, overrides: &HashMap<String, String>) {
        for (action, chord) in overrides {
            let Ok(chord) = chord.parse::<KeyChord>() else {
                continue;
            };
            match action.as_str() {
                "quit" => self.quit = chord,
                "command_palette" => self.command_palette = chord,
                "toggle_sidebar" => self.toggle_sidebar = chord,
                "toggle_editor" => self.toggle_editor = chord,
                "toggle_scm" => self.toggle_scm = chord,
                "new_terminal" => self.new_terminal = chord,
                "focus_next" => self.focus_next = chord,
                "next_tab" => self.next_tab = chord,
                "prev_tab" => self.prev_tab = chord,
                "close_tab" => self.close_tab = chord,
                "save" => self.save = chord,
                "new_file" => self.new_file = chord,
                "toggle_scrollbar" => self.toggle_scrollbar = chord,
                "select_all" => self.select_all = chord,
                "toggle_autosave" => self.toggle_autosave = chord,
                "toggle_panel" => self.toggle_panel = chord,
                "help" => self.help = chord,
                _ => {}
            }
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct SettingsFile {
    theme: Option<String>,
    solid_bg: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    keys: HashMap<String, String>,
    #[serde(default)]
    settings: SettingsFile,
}

/// Top-level resolved configuration handed to the app.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub keymap: Keymap,
    /// Persisted UI state.
    pub theme: Option<String>,
    pub solid_bg: bool,
}

const DEFAULT_CONFIG: &str = "\
# warren configuration
# Keybindings: action = \"chord\". Modifiers: ctrl, alt, shift, super.
[keys]
quit = \"ctrl+q\"
command_palette = \"ctrl+p\"
toggle_sidebar = \"ctrl+b\"
toggle_editor = \"alt+e\"
toggle_scm = \"ctrl+g\"
new_terminal = \"ctrl+t\"
focus_next = \"ctrl+w\"
next_tab = \"ctrl+pagedown\"
prev_tab = \"ctrl+pageup\"
close_tab = \"ctrl+x\"
save = \"ctrl+s\"
new_file = \"ctrl+n\"
toggle_scrollbar = \"alt+s\"
select_all = \"ctrl+a\"
toggle_autosave = \"alt+a\"
toggle_panel = \"ctrl+`\"
help = \"f1\"

# UI defaults (warren also persists runtime changes to state.toml).
[settings]
theme = \"Tokyo Night\"
solid_bg = false
";

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("warren").join("config.toml"))
}

/// Runtime UI state lives here (separate from the user-edited config.toml).
fn state_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("warren").join("state.toml"))
}

/// Persist the runtime UI choices (theme + solid background). Best-effort.
pub fn save_state(theme: &str, solid_bg: bool) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, format!("theme = \"{theme}\"\nsolid_bg = {solid_bg}\n"));
}

impl Config {
    /// Load config from disk, writing the default file on first run. Never fails: any error
    /// (missing dir, parse error) falls back to built-in defaults.
    pub fn load() -> Self {
        let mut config = Config::default();
        let Some(path) = config_path() else {
            return config;
        };

        let file = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str::<ConfigFile>(&text).unwrap_or_default(),
            Err(_) => {
                // First run (or unreadable): try to seed the default file.
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                    let _ = std::fs::write(&path, DEFAULT_CONFIG);
                }
                ConfigFile::default()
            }
        };

        config.keymap.apply(&file.keys);
        config.theme = file.settings.theme;
        config.solid_bg = file.settings.solid_bg.unwrap_or(false);

        // state.toml (written by warren on change) overrides config.toml settings.
        if let Some(sp) = state_path() {
            if let Ok(text) = std::fs::read_to_string(sp) {
                if let Ok(state) = toml::from_str::<SettingsFile>(&text) {
                    if state.theme.is_some() {
                        config.theme = state.theme;
                    }
                    if let Some(b) = state.solid_bg {
                        config.solid_bg = b;
                    }
                }
            }
        }
        config
    }
}
