//! Application state and the central run loop.

use std::path::PathBuf;

use anyhow::Result;
use crossterm::event::{
    Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use notify::RecommendedWatcher;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::editor::Editor;
use crate::event::{self, AppEvent};
use crate::explorer::FileTree;
use crate::tui::Tui;
use crate::{ui, watcher};

/// Which component currently receives key input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Editor,
}

/// Global application state. The run loop owns this and is the only writer.
pub struct App {
    pub config: Config,
    pub workspace: PathBuf,
    pub tree: FileTree,
    pub editor: Editor,
    pub focus: Focus,
    pub sidebar_visible: bool,
    pub status: String,
    should_quit: bool,
    needs_redraw: bool,
    /// Coalesce bursts of filesystem events into one rebuild on the next tick.
    tree_dirty: bool,
    /// Open files reported changed on disk, reloaded on the next tick.
    changed_paths: Vec<PathBuf>,
    /// Held so the watcher keeps running (dropping it stops watching).
    _watcher: Option<RecommendedWatcher>,
}

impl App {
    pub fn new(config: Config) -> Self {
        // Optional folder argument, else the current directory.
        let workspace = std::env::args()
            .nth(1)
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .and_then(|p| p.canonicalize().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let tree = FileTree::new(workspace.clone());
        Self {
            config,
            workspace,
            tree,
            editor: Editor::default(),
            focus: Focus::Sidebar,
            sidebar_visible: true,
            status: "ready".into(),
            should_quit: false,
            needs_redraw: true,
            tree_dirty: false,
            changed_paths: Vec::new(),
            _watcher: None,
        }
    }

    /// Drive the app: wire up event sources, then drain the funnel until quit.
    pub async fn run(mut self, terminal: &mut Tui) -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        event::spawn_input(tx.clone());
        event::spawn_ticks(tx.clone());
        self._watcher = watcher::spawn(&self.workspace, tx);

        // Rendering is driven by the tick only: input/fs events mutate state and mark the UI
        // dirty, and each tick paints at most one frame. This coalesces bursts so holding a key
        // or flooding mouse motion can't pile up redraws.
        self.draw(terminal)?;
        while let Some(ev) = rx.recv().await {
            match ev {
                AppEvent::Tick => {
                    self.on_tick();
                    if self.needs_redraw {
                        self.draw(terminal)?;
                        self.needs_redraw = false;
                    }
                }
                other => {
                    self.handle(other);
                    if self.should_quit {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Per-frame housekeeping before painting: apply any coalesced filesystem changes.
    fn on_tick(&mut self) {
        if self.tree_dirty {
            self.tree_dirty = false;
            self.tree.rebuild();
            for path in std::mem::take(&mut self.changed_paths) {
                self.editor.reload(&path);
            }
            self.needs_redraw = true;
        }
    }

    fn draw(&mut self, terminal: &mut Tui) -> Result<()> {
        terminal.draw(|frame| ui::draw(frame, self))?;
        Ok(())
    }

    fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Input(CtEvent::Key(key)) => self.on_key(key),
            AppEvent::Input(CtEvent::Mouse(m)) => self.on_mouse(m),
            AppEvent::Input(CtEvent::Resize(..)) => self.needs_redraw = true,
            AppEvent::Input(_) => {}
            AppEvent::FsChanged(paths) => {
                self.tree_dirty = true;
                self.changed_paths.extend(paths);
            }
            AppEvent::Tick => {} // handled in run loop's render step
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let in_sidebar = self.sidebar_visible && m.column < ui::SIDEBAR_WIDTH;
        match m.kind {
            MouseEventKind::ScrollDown => {
                if in_sidebar {
                    self.tree.move_down();
                } else {
                    self.editor.scroll_down(3);
                }
                self.needs_redraw = true;
            }
            MouseEventKind::ScrollUp => {
                if in_sidebar {
                    self.tree.move_up();
                } else {
                    self.editor.scroll_up(3);
                }
                self.needs_redraw = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if in_sidebar {
                    self.focus = Focus::Sidebar;
                    // Map the click row to a tree row using the offset from the last frame.
                    // Row 0 is the top border, so the first item is at row 1.
                    if m.row >= 1 {
                        let idx = self.tree.scroll + (m.row as usize - 1);
                        if idx < self.tree.rows.len() {
                            self.tree.selected = idx;
                            self.activate_selected();
                        }
                    }
                } else {
                    self.focus = Focus::Editor;
                    if m.row == self.editor.tabbar_row {
                        // Click on the tab bar switches tabs.
                        if let Some(&(_, _, idx)) = self
                            .editor
                            .tab_hitboxes
                            .iter()
                            .find(|(s, e, _)| m.column >= *s && m.column < *e)
                        {
                            self.editor.active = idx;
                            self.editor.clear_selection();
                        }
                    } else if let Some((line, col)) = self.editor_coords(&m) {
                        // Begin a text selection in the editor content.
                        self.editor.start_selection(line, col);
                    } else {
                        self.editor.clear_selection();
                    }
                }
                self.needs_redraw = true;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((line, col)) = self.editor_coords(&m) {
                    self.editor.update_selection(line, col);
                    self.needs_redraw = true;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(text) = self.editor.selected_text() {
                    let n = text.chars().count();
                    copy_to_clipboard(&text);
                    self.status = format!("copied {n} chars");
                } else {
                    self.editor.clear_selection();
                }
                self.needs_redraw = true;
            }
            _ => {}
        }
    }

    /// Map a mouse position to `(line, column)` in the active buffer, or `None` if it's outside
    /// the editor content area.
    fn editor_coords(&self, m: &MouseEvent) -> Option<(usize, usize)> {
        let ca = self.editor.content_area;
        if ca.width == 0
            || ca.height == 0
            || m.column < ca.x
            || m.column >= ca.x + ca.width
            || m.row < ca.y
            || m.row >= ca.y + ca.height
        {
            return None;
        }
        let buf = self.editor.active_buffer()?;
        let line = (buf.scroll + (m.row - ca.y) as usize).min(buf.line_count().saturating_sub(1));
        let col = ((m.column - ca.x) as usize).min(buf.line_text(line).chars().count());
        Some((line, col))
    }

    /// Open the selected file, or expand/collapse the selected directory (shared by the
    /// Enter key and mouse clicks).
    fn activate_selected(&mut self) {
        if let Some(path) = self.tree.activate() {
            match self.editor.open(&path) {
                Ok(()) => {
                    self.focus = Focus::Editor;
                    self.status = path.display().to_string();
                }
                Err(e) => self.status = format!("error: {e}"),
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.needs_redraw = true;
        if self.handle_global(key) {
            return;
        }
        match self.focus {
            Focus::Sidebar => self.handle_sidebar(key),
            Focus::Editor => self.handle_editor(key),
        }
    }

    /// Global keys handled regardless of focus. Returns true if consumed.
    fn handle_global(&mut self, key: KeyEvent) -> bool {
        let km = &self.config.keymap;
        let (c, m) = (key.code, key.modifiers);
        if km.quit.matches(c, m) {
            self.should_quit = true;
        } else if km.toggle_sidebar.matches(c, m) {
            self.sidebar_visible = !self.sidebar_visible;
            if !self.sidebar_visible {
                self.focus = Focus::Editor;
            } else {
                self.focus = Focus::Sidebar;
            }
        } else if km.focus_next.matches(c, m) {
            self.focus = match (self.focus, self.sidebar_visible) {
                (Focus::Sidebar, _) => Focus::Editor,
                (Focus::Editor, true) => Focus::Sidebar,
                (Focus::Editor, false) => Focus::Editor,
            };
        } else if km.command_palette.matches(c, m) {
            self.status = "command palette: coming in Phase 4".into();
        } else if km.open_claude.matches(c, m) {
            self.status = "claude pane: coming in Phase 5".into();
        } else {
            return false;
        }
        true
    }

    fn handle_sidebar(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.tree.move_up(),
            KeyCode::Down => self.tree.move_down(),
            KeyCode::Right => self.tree.expand(),
            KeyCode::Left => self.tree.collapse(),
            KeyCode::Enter => self.activate_selected(),
            _ => {}
        }
    }

    fn handle_editor(&mut self, key: KeyEvent) {
        let km = &self.config.keymap;
        let (c, m) = (key.code, key.modifiers);
        // Ctrl+C copies the current selection (if any).
        if c == KeyCode::Char('c') && m.contains(KeyModifiers::CONTROL) {
            if let Some(text) = self.editor.selected_text() {
                let n = text.chars().count();
                copy_to_clipboard(&text);
                self.status = format!("copied {n} chars");
            }
            return;
        }
        if km.next_tab.matches(c, m) {
            self.editor.next_tab();
        } else if km.prev_tab.matches(c, m) {
            self.editor.prev_tab();
        } else if km.close_tab.matches(c, m) {
            self.editor.close_active();
        } else {
            match c {
                KeyCode::Up => self.editor.scroll_up(1),
                KeyCode::Down => self.editor.scroll_down(1),
                KeyCode::PageUp => self.editor.scroll_up(self.editor.viewport.max(1)),
                KeyCode::PageDown => self.editor.scroll_down(self.editor.viewport.max(1)),
                KeyCode::Home => self.editor.scroll_home(),
                KeyCode::End => self.editor.scroll_end(),
                _ => {}
            }
        }
    }
}

/// Copy text to the system clipboard via the OSC 52 terminal escape. Works in kitty (and most
/// modern terminals), needs no clipboard daemon, and survives over SSH.
fn copy_to_clipboard(text: &str) {
    use base64::Engine;
    use std::io::Write;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{encoded}\x07");
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}
