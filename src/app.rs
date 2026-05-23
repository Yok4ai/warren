//! Application state and the central run loop.

use std::path::PathBuf;
use std::time::Duration;

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
use crate::prompt::{Prompt, PromptKind};
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
    /// Sidebar width in columns (draggable).
    pub sidebar_width: u16,
    /// True while dragging the sidebar/editor divider.
    resizing: bool,
    /// True while dragging the editor scrollbar thumb.
    dragging_scrollbar: bool,
    /// Offset (rows) between the grab point and the thumb top, so dragging holds the thumb.
    scrollbar_grab: i32,
    /// Whether the editor scrollbar is shown.
    pub show_scrollbar: bool,
    /// Whether modified buffers are auto-saved after a short idle delay.
    pub auto_save: bool,
    /// Last known terminal width, for clamping resize drags.
    pub term_width: u16,
    /// Active modal prompt (e.g. new-file input), if any.
    pub prompt: Option<Prompt>,
    /// Tab index awaiting a save/discard/cancel decision before closing, if any.
    pub close_confirm: Option<usize>,
    /// Whether the keybinding help overlay is shown.
    pub show_help: bool,
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
            sidebar_width: 32,
            resizing: false,
            dragging_scrollbar: false,
            scrollbar_grab: 0,
            show_scrollbar: true,
            auto_save: false,
            term_width: 80,
            prompt: None,
            close_confirm: None,
            show_help: false,
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

    /// Per-frame housekeeping before painting: apply coalesced filesystem changes and refresh
    /// syntax highlighting for any buffers edited since the last frame.
    fn on_tick(&mut self) {
        if self.tree_dirty {
            self.tree_dirty = false;
            self.tree.rebuild();
            for path in std::mem::take(&mut self.changed_paths) {
                self.editor.reload(&path);
            }
            self.needs_redraw = true;
        }
        self.editor.refresh_highlight();
        if self.auto_save && self.editor.autosave(Duration::from_millis(800)) {
            self.status = "auto-saved".into();
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
            AppEvent::Input(CtEvent::Paste(text)) => self.on_paste(text),
            AppEvent::Input(CtEvent::Resize(..)) => self.needs_redraw = true,
            AppEvent::Input(_) => {}
            AppEvent::FsChanged(paths) => {
                self.tree_dirty = true;
                self.changed_paths.extend(paths);
            }
            AppEvent::Tick => {} // handled in run loop's render step
        }
    }

    /// Insert pasted text into the focused target (prompt or editor), replacing any selection.
    fn on_paste(&mut self, text: String) {
        if let Some(p) = &mut self.prompt {
            for ch in text.chars().filter(|c| *c != '\n' && *c != '\r') {
                p.insert_char(ch);
            }
            self.needs_redraw = true;
            return;
        }
        if self.focus == Focus::Editor {
            self.editor.delete_selection();
            let vp = self.editor.viewport.max(1);
            if let Some(b) = self.editor.active_buffer_mut() {
                b.insert_text(&text);
                b.ensure_cursor_visible(vp);
            }
            self.needs_redraw = true;
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let in_sidebar = self.sidebar_visible && m.column < self.sidebar_width;
        // The draggable divider sits on the seam between the sidebar and editor borders.
        let on_divider = self.sidebar_visible
            && (self.sidebar_width.saturating_sub(1)..=self.sidebar_width).contains(&m.column);
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
                if self.on_scrollbar(&m) {
                    // Grab the thumb without jumping; only dragging scrolls.
                    self.dragging_scrollbar = true;
                    self.scrollbar_grab = self.scrollbar_grab_offset(m.row);
                } else if on_divider {
                    self.resizing = true;
                } else if in_sidebar {
                    self.focus = Focus::Sidebar;
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
                        // A click on a tab's ✕ closes it; elsewhere on a tab switches to it.
                        if let Some(&(_, _, idx)) = self
                            .editor
                            .close_hitboxes
                            .iter()
                            .find(|(s, e, _)| m.column >= *s && m.column < *e)
                        {
                            self.request_close(idx);
                        } else if let Some(&(_, _, idx)) = self
                            .editor
                            .tab_hitboxes
                            .iter()
                            .find(|(s, e, _)| m.column >= *s && m.column < *e)
                        {
                            self.editor.active = idx;
                            self.editor.clear_selection();
                        }
                    } else if let Some((line, col)) = self.editor_coords(&m) {
                        // Click places the cursor and begins a (possibly empty) selection.
                        if let Some(b) = self.editor.active_buffer_mut() {
                            b.set_cursor(line, col);
                        }
                        self.editor.start_selection(line, col);
                    } else {
                        self.editor.clear_selection();
                    }
                }
                self.needs_redraw = true;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.dragging_scrollbar {
                    self.scroll_bar_to(m.row);
                } else if self.resizing {
                    let max = self.term_width.saturating_sub(15).max(15);
                    self.sidebar_width = m.column.clamp(15, max);
                } else if self.editor.selection.is_some() {
                    // Auto-scroll when the drag passes above/below the content area, so the
                    // selection can extend beyond what's currently visible.
                    let ca = self.editor.content_area;
                    if ca.height > 0 {
                        if m.row >= ca.y + ca.height {
                            self.editor.scroll_down(1);
                        } else if m.row < ca.y {
                            self.editor.scroll_up(1);
                        }
                    }
                    if let Some((line, col)) = self.editor_coords_clamped(&m) {
                        self.editor.update_selection(line, col);
                    }
                }
                self.needs_redraw = true;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.dragging_scrollbar {
                    self.dragging_scrollbar = false;
                } else if self.resizing {
                    self.resizing = false;
                } else if let Some(text) = self.editor.selected_text() {
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

    /// Like [`editor_coords`], but clamps the position into the content area (used while
    /// drag-selecting past the edges).
    fn editor_coords_clamped(&self, m: &MouseEvent) -> Option<(usize, usize)> {
        let ca = self.editor.content_area;
        if ca.width == 0 || ca.height == 0 {
            return None;
        }
        let row = m.row.clamp(ca.y, ca.y + ca.height - 1);
        let col = m.column.clamp(ca.x, ca.x + ca.width - 1);
        let buf = self.editor.active_buffer()?;
        let line = (buf.scroll + (row - ca.y) as usize).min(buf.line_count().saturating_sub(1));
        let c = ((col - ca.x) as usize).min(buf.line_text(line).chars().count());
        Some((line, c))
    }

    /// Whether a mouse event falls on the editor scrollbar column.
    fn on_scrollbar(&self, m: &MouseEvent) -> bool {
        let Some(sx) = self.editor.scrollbar_col else {
            return false;
        };
        let ca = self.editor.content_area;
        m.column == sx && m.row >= ca.y && m.row < ca.y + ca.height
    }

    /// Set the editor scroll from a scrollbar row. Inverts `draw_scrollbar`'s thumb-position
    /// formula so the thumb top tracks the cursor exactly (the draggable range is the track
    /// height minus the thumb height, not the full track).
    fn scroll_bar_to(&mut self, row: u16) {
        let ca = self.editor.content_area;
        let track = ca.height as usize;
        let total = self
            .editor
            .active_buffer()
            .map(|b| b.line_count())
            .unwrap_or(0);
        if track == 0 || total <= track {
            self.editor.scroll_to(0);
            return;
        }
        let thumb = ((track * track) / total).clamp(1, track);
        let max_scroll = total - track;
        let denom = track.saturating_sub(thumb).max(1);
        // Desired thumb top = cursor row minus where on the thumb it was grabbed.
        let target = (row as i32 - ca.y as i32 - self.scrollbar_grab).clamp(0, denom as i32) as usize;
        self.editor.scroll_to(target * max_scroll / denom);
    }

    /// Offset between a grab row and the current thumb top, so dragging holds the thumb steady.
    fn scrollbar_grab_offset(&self, row: u16) -> i32 {
        let ca = self.editor.content_area;
        let track = ca.height as usize;
        let total = self
            .editor
            .active_buffer()
            .map(|b| b.line_count())
            .unwrap_or(0);
        let scroll = self.editor.active_buffer().map(|b| b.scroll).unwrap_or(0);
        if track == 0 || total <= track {
            return 0;
        }
        let thumb = ((track * track) / total).clamp(1, track);
        let max_scroll = total - track;
        let denom = track.saturating_sub(thumb).max(1);
        let thumb_top = scroll * denom / max_scroll;
        row as i32 - ca.y as i32 - thumb_top as i32
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
        // The help overlay is dismissed by any of Help/Esc and swallows other keys.
        if self.show_help {
            if key.code == KeyCode::Esc
                || self.config.keymap.help.matches(key.code, key.modifiers)
            {
                self.show_help = false;
            }
            return;
        }
        // Modals capture all input until resolved.
        if self.close_confirm.is_some() {
            self.handle_close_confirm(key);
            return;
        }
        if self.prompt.is_some() {
            self.handle_prompt(key);
            return;
        }
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
        } else if km.new_file.matches(c, m) {
            self.open_new_file_prompt();
        } else if km.toggle_scrollbar.matches(c, m) {
            self.show_scrollbar = !self.show_scrollbar;
        } else if km.toggle_autosave.matches(c, m) {
            self.auto_save = !self.auto_save;
            self.status = if self.auto_save {
                "auto-save: on".into()
            } else {
                "auto-save: off".into()
            };
        } else if km.help.matches(c, m) {
            self.show_help = true;
        } else if km.command_palette.matches(c, m) {
            self.status = "command palette: coming in Phase 4".into();
        } else if km.open_claude.matches(c, m) {
            self.status = "claude pane: coming in Phase 5".into();
        } else {
            return false;
        }
        true
    }

    /// Open the new-file prompt, defaulting the target directory to the explorer selection
    /// (the selected folder, or the folder containing the selected file).
    fn open_new_file_prompt(&mut self) {
        let base = self
            .tree
            .rows
            .get(self.tree.selected)
            .map(|r| {
                if r.is_dir {
                    r.path.clone()
                } else {
                    r.path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| self.workspace.clone())
                }
            })
            .unwrap_or_else(|| self.workspace.clone());
        self.prompt = Some(Prompt::new_file(base));
    }

    fn handle_prompt(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => self.confirm_prompt(),
            KeyCode::Backspace => {
                if let Some(p) = &mut self.prompt {
                    p.backspace();
                }
            }
            KeyCode::Left => {
                if let Some(p) = &mut self.prompt {
                    p.move_left();
                }
            }
            KeyCode::Right => {
                if let Some(p) = &mut self.prompt {
                    p.move_right();
                }
            }
            KeyCode::Home => {
                if let Some(p) = &mut self.prompt {
                    p.move_home();
                }
            }
            KeyCode::End => {
                if let Some(p) = &mut self.prompt {
                    p.move_end();
                }
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                if let Some(p) = &mut self.prompt {
                    p.insert_char(c);
                }
            }
            _ => {}
        }
    }

    fn confirm_prompt(&mut self) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        match prompt.kind {
            PromptKind::NewFile { base } => {
                let name = prompt.input.trim();
                if name.is_empty() {
                    return;
                }
                let path = base.join(name);
                if let Some(parent) = path.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        self.status = format!("error: {e}");
                        return;
                    }
                }
                if !path.exists() {
                    if let Err(e) = std::fs::write(&path, "") {
                        self.status = format!("error: {e}");
                        return;
                    }
                }
                match self.editor.open(&path) {
                    Ok(()) => {
                        self.focus = Focus::Editor;
                        self.status = format!("created {}", path.display());
                    }
                    Err(e) => self.status = format!("error: {e}"),
                }
            }
        }
    }

    /// Close the tab at `idx`, but ask first if it has unsaved changes.
    fn request_close(&mut self, idx: usize) {
        if self.editor.is_modified(idx) {
            self.close_confirm = Some(idx);
        } else {
            self.editor.close_at(idx);
        }
    }

    fn handle_close_confirm(&mut self, key: KeyEvent) {
        let Some(idx) = self.close_confirm else {
            return;
        };
        match key.code {
            KeyCode::Char('s' | 'S') => match self.editor.save_at(idx) {
                Ok(()) => {
                    self.editor.close_at(idx);
                    self.close_confirm = None;
                    self.status = "saved & closed".into();
                }
                Err(e) => self.status = format!("save failed: {e}"),
            },
            KeyCode::Char('d' | 'D') => {
                self.editor.close_at(idx);
                self.close_confirm = None;
                self.status = "closed without saving".into();
            }
            KeyCode::Char('c' | 'C') | KeyCode::Esc => self.close_confirm = None,
            _ => {}
        }
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
        let ctrl = m.contains(KeyModifiers::CONTROL);
        let alt = m.contains(KeyModifiers::ALT);

        // Ctrl+C copies the current selection (if any).
        if c == KeyCode::Char('c') && ctrl {
            if let Some(text) = self.editor.selected_text() {
                let n = text.chars().count();
                copy_to_clipboard(&text);
                self.status = format!("copied {n} chars");
            }
            return;
        }
        if km.save.matches(c, m) {
            match self.editor.save_active() {
                Ok(()) => self.status = "saved".into(),
                Err(e) => self.status = format!("save failed: {e}"),
            }
            return;
        }
        if km.select_all.matches(c, m) {
            self.editor.select_all();
            return;
        }
        if km.next_tab.matches(c, m) {
            self.editor.next_tab();
            return;
        }
        if km.prev_tab.matches(c, m) {
            self.editor.prev_tab();
            return;
        }
        if km.close_tab.matches(c, m) {
            self.request_close(self.editor.active);
            return;
        }

        let vp = self.editor.viewport.max(1);

        // With a non-empty selection, an edit replaces the whole selection.
        let has_sel = self.editor.selection.map(|s| !s.is_empty()).unwrap_or(false);
        if has_sel {
            let replaced = match c {
                KeyCode::Backspace | KeyCode::Delete => self.editor.delete_selection(),
                KeyCode::Enter => {
                    self.editor.delete_selection();
                    if let Some(b) = self.editor.active_buffer_mut() {
                        b.insert_char('\n');
                    }
                    true
                }
                KeyCode::Tab => {
                    self.editor.delete_selection();
                    if let Some(b) = self.editor.active_buffer_mut() {
                        b.insert_str("    ");
                    }
                    true
                }
                KeyCode::Char(ch) if !ctrl && !alt => {
                    self.editor.delete_selection();
                    if let Some(b) = self.editor.active_buffer_mut() {
                        b.insert_char(ch);
                    }
                    true
                }
                _ => false,
            };
            if replaced {
                if let Some(b) = self.editor.active_buffer_mut() {
                    b.ensure_cursor_visible(vp);
                }
                return;
            }
        }

        // Editing + cursor movement operate on the active buffer.
        let mut edited = false;
        if let Some(b) = self.editor.active_buffer_mut() {
            match c {
                KeyCode::Char(ch) if !ctrl && !alt => {
                    b.insert_char(ch);
                    edited = true;
                }
                KeyCode::Enter => {
                    b.insert_char('\n');
                    edited = true;
                }
                KeyCode::Tab => {
                    b.insert_str("    ");
                    edited = true;
                }
                KeyCode::Backspace if ctrl => {
                    b.delete_word_back();
                    edited = true;
                }
                KeyCode::Backspace => {
                    b.backspace();
                    edited = true;
                }
                KeyCode::Delete => {
                    b.delete();
                    edited = true;
                }
                KeyCode::Left if ctrl => {
                    b.move_word_left();
                    edited = true;
                }
                KeyCode::Right if ctrl => {
                    b.move_word_right();
                    edited = true;
                }
                KeyCode::Left => {
                    b.move_left();
                    edited = true;
                }
                KeyCode::Right => {
                    b.move_right();
                    edited = true;
                }
                KeyCode::Up => {
                    b.move_up(1);
                    edited = true;
                }
                KeyCode::Down => {
                    b.move_down(1);
                    edited = true;
                }
                KeyCode::PageUp => {
                    b.move_up(vp);
                    edited = true;
                }
                KeyCode::PageDown => {
                    b.move_down(vp);
                    edited = true;
                }
                KeyCode::Home => {
                    b.set_cursor(0, 0);
                    edited = true;
                }
                KeyCode::End => {
                    let last = b.line_count().saturating_sub(1);
                    b.set_cursor(last, usize::MAX);
                    edited = true;
                }
                _ => {}
            }
            if edited {
                b.ensure_cursor_visible(vp);
            }
        }
        if edited {
            self.editor.clear_selection();
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
