//! Application state and the central run loop.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use git2::Oid;
use crossterm::event::{
    Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use notify::RecommendedWatcher;
use ratatui::layout::Rect;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::config::Config;
use crate::editor::{self, Editor};
use crate::event::{self, AppEvent};
use crate::explorer::FileTree;
use crate::find::Search;
use crate::git::{Change, Commit, Git};
use crate::ide::{self, DiffDecision, IdeServer};
use crate::palette::{self, Choice, Command, Palette};
use tokio::sync::oneshot;
use crate::prompt::{Prompt, PromptKind};
use crate::terminal::{Panel, TerminalPane};
use crate::tui::Tui;
use crate::{ui, watcher};

/// Which component currently receives key input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Editor,
    Terminal,
}

/// What the sidebar shows (the VS Code "activity bar" views we support).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarMode {
    Explorer,
    SourceControl,
}

/// A selectable row in the source-control view (flattened from changes + the expandable graph).
#[derive(Clone)]
pub enum ScmItem {
    /// A working-tree change (index into `git_changes`).
    Change(usize),
    /// A commit row (index into `git_commits`).
    Commit(usize),
    /// A file inside an expanded commit.
    CommitFile {
        commit: usize,
        path: String,
        code: char,
    },
}

/// A diff awaiting the user's accept/reject (from Claude via the IDE protocol).
struct PendingDiff {
    reply: oneshot::Sender<DiffDecision>,
    new_contents: String,
}

/// Global application state. The run loop owns this and is the only writer.
pub struct App {
    pub config: Config,
    pub workspace: PathBuf,
    pub tree: FileTree,
    pub editor: Editor,
    pub focus: Focus,
    pub sidebar_visible: bool,
    pub sidebar_mode: SidebarMode,
    pub editor_visible: bool,
    /// Git repository for the workspace, if any, and its cached state.
    git: Option<Git>,
    pub git_branch: String,
    pub git_changes: Vec<Change>,
    pub git_commits: Vec<Commit>,
    /// Commits whose file list is expanded in the graph.
    pub git_expanded: HashSet<Oid>,
    /// Flattened, currently-visible SCM rows (changes + commits + expanded files).
    pub scm_items: Vec<ScmItem>,
    pub scm_selected: usize,
    /// Independent scroll offset (top line) for the SCM list.
    pub scm_scroll: usize,
    /// Line index per SCM item, the SCM viewport height, and total line count — set by the
    /// renderer so keyboard nav can keep the selection visible.
    pub scm_item_lines: Vec<usize>,
    pub scm_viewport: usize,
    pub scm_total_lines: usize,
    /// Screen-row → SCM item index, set by the renderer for click mapping.
    pub scm_rows: Vec<(u16, usize)>,
    /// Sidebar scrollbar geometry (column, top-row, height, total-rows) when shown.
    pub sidebar_sb: Option<(u16, u16, u16, usize)>,
    /// True while dragging the sidebar scrollbar, and the grab offset within the thumb.
    dragging_sidebar_sb: bool,
    sidebar_sb_grab: i32,
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
    /// Paint a solid theme background (overrides terminal transparency).
    pub solid_bg: bool,
    /// In-app clipboard for ctrl+c / ctrl+v (also mirrored to the system clipboard via OSC 52).
    clipboard: String,
    /// Last known terminal width, for clamping resize drags.
    pub term_width: u16,
    /// Active modal prompt (e.g. new-file input), if any.
    pub prompt: Option<Prompt>,
    /// Tab index awaiting a save/discard/cancel decision before closing, if any.
    pub close_confirm: Option<usize>,
    /// The command palette / fuzzy finder, when open.
    pub palette: Option<Palette>,
    /// In-editor find, when open.
    pub search: Option<Search>,
    /// Whether the keybinding help overlay is shown.
    pub show_help: bool,
    /// Current cursor-blink phase (on/off), and when blinking started.
    pub blink_on: bool,
    blink_start: Instant,
    /// Drag-and-drop from the explorer: the path being dragged, whether a real drag is underway,
    /// and the current mouse position (for the floating label).
    pub drag_source: Option<PathBuf>,
    pub dragging: bool,
    pub drag_pos: (u16, u16),
    /// The terminal panel (multiple terminals with a vertical tab strip).
    pub panel: Panel,
    /// Percentage of the editor/panel split given to the panel (right side).
    pub term_ratio: u16,
    /// Width (columns) of the panel's vertical tab strip, including its border (draggable).
    pub panel_strip_w: u16,
    /// True while dragging the editor/panel divider.
    resizing_term: bool,
    /// True while dragging the terminal-content / tab-strip divider.
    resizing_strip: bool,
    /// Per-frame geometry of the tab-strip divider (set by the renderer).
    pub panel_divider_col: u16,
    pub panel_inner_right: u16,
    /// Per-frame geometry for mouse mapping (set by the renderer).
    pub editor_area: Rect,
    pub terminal_area: Rect,
    /// Editor horizontal scrollbar geometry (x, y, width, thumb, max_scroll) when shown.
    pub editor_hbar: Option<(u16, u16, u16, usize, usize)>,
    dragging_hbar: bool,
    hbar_grab: i32,
    /// IDE-integration server (impersonates the editor for Claude); held to keep it alive.
    _ide: Option<IdeServer>,
    ide_port: Option<u16>,
    /// A diff Claude proposed, awaiting accept/reject.
    pending_diff: Option<PendingDiff>,
    /// Funnel sender, kept so panes spawned later can push events.
    tx: Option<UnboundedSender<AppEvent>>,
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
        let git = Git::open(&workspace);
        // Apply persisted UI state.
        if let Some(t) = &config.theme {
            crate::theme::set_by_name(t);
        }
        let solid_bg = config.solid_bg;
        Self {
            config,
            workspace,
            tree,
            editor: Editor::default(),
            focus: Focus::Sidebar,
            sidebar_visible: true,
            sidebar_mode: SidebarMode::Explorer,
            editor_visible: true,
            git,
            git_branch: String::new(),
            git_changes: Vec::new(),
            git_commits: Vec::new(),
            git_expanded: HashSet::new(),
            scm_items: Vec::new(),
            scm_selected: 0,
            scm_scroll: 0,
            scm_item_lines: Vec::new(),
            scm_viewport: 0,
            scm_total_lines: 0,
            scm_rows: Vec::new(),
            sidebar_sb: None,
            dragging_sidebar_sb: false,
            sidebar_sb_grab: 0,
            sidebar_width: 32,
            resizing: false,
            dragging_scrollbar: false,
            scrollbar_grab: 0,
            show_scrollbar: true,
            auto_save: false,
            solid_bg,
            clipboard: String::new(),
            term_width: 80,
            prompt: None,
            close_confirm: None,
            palette: None,
            search: None,
            show_help: false,
            blink_on: true,
            blink_start: Instant::now(),
            drag_source: None,
            dragging: false,
            drag_pos: (0, 0),
            panel: Panel::default(),
            term_ratio: 50,
            panel_strip_w: 10,
            resizing_term: false,
            resizing_strip: false,
            panel_divider_col: 0,
            panel_inner_right: 0,
            editor_area: Rect::default(),
            terminal_area: Rect::default(),
            editor_hbar: None,
            dragging_hbar: false,
            hbar_grab: 0,
            _ide: None,
            ide_port: None,
            pending_diff: None,
            tx: None,
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
        self._watcher = watcher::spawn(&self.workspace, tx.clone());
        // Impersonate the IDE so Claude shows its edits as accept/reject diffs in warren.
        if let Some(server) = ide::start(self.workspace.clone(), tx.clone()).await {
            self.ide_port = Some(server.port);
            self._ide = Some(server);
        }
        self.tx = Some(tx);

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
            if self.sidebar_mode == SidebarMode::SourceControl {
                self.refresh_git();
            }
            self.needs_redraw = true;
        }
        self.editor.refresh_highlight();
        if self.auto_save && self.editor.autosave(Duration::from_millis(800)) {
            self.status = "auto-saved".into();
            self.needs_redraw = true;
        }
        // Drive the cursor blink (~530ms), forcing a redraw when the phase flips.
        let phase = (self.blink_start.elapsed().as_millis() / 530) % 2 == 0;
        if phase != self.blink_on {
            self.blink_on = phase;
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
            AppEvent::OpenDiff {
                path,
                new_contents,
                tab_name,
                reply,
            } => self.open_diff(path, new_contents, tab_name, reply),
            AppEvent::CloseDiff => {
                if let Some(pd) = self.pending_diff.take() {
                    let _ = pd.reply.send(DiffDecision::Reject);
                }
                self.close_diff_tab();
                self.needs_redraw = true;
            }
            AppEvent::PtyChanged => self.needs_redraw = true,
            AppEvent::PtyExited => {
                if self.panel.prune_exited() {
                    if self.panel.terms.is_empty() && self.focus == Focus::Terminal {
                        self.focus = Focus::Editor;
                    }
                    self.needs_redraw = true;
                }
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
        if self.focus == Focus::Terminal {
            if let Some(t) = self.panel.active_mut() {
                t.send_paste(&text);
            }
            self.needs_redraw = true;
            return;
        }
        if self.focus == Focus::Editor {
            if self.editor.active_buffer().map(|b| b.is_readonly()).unwrap_or(false) {
                return;
            }
            self.editor.delete_selection();
            let vp = self.editor.viewport.max(1);
        let vw = self.editor.viewport_w.max(1);
            if let Some(b) = self.editor.active_buffer_mut() {
                b.insert_text(&text);
                b.ensure_cursor_visible(vp, vw);
            }
            self.needs_redraw = true;
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let in_sidebar = self.sidebar_visible && m.column < self.sidebar_width;
        let on_sidebar_divider = self.sidebar_visible
            && (self.sidebar_width.saturating_sub(1)..=self.sidebar_width).contains(&m.column);
        let term_open = self.panel.visible && !self.panel.is_empty();
        let in_terminal = term_open && rect_contains(self.terminal_area, m.column, m.row);
        let on_term_divider = term_open
            && self.editor_visible
            && (self.terminal_area.x.saturating_sub(1)..=self.terminal_area.x).contains(&m.column)
            && (self.terminal_area.y..self.terminal_area.y + self.terminal_area.height)
                .contains(&m.row);
        let on_strip_divider = term_open
            && (self.panel_divider_col.saturating_sub(1)..=self.panel_divider_col)
                .contains(&m.column)
            && (self.terminal_area.y..self.terminal_area.y + self.terminal_area.height)
                .contains(&m.row);

        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let down = matches!(m.kind, MouseEventKind::ScrollDown);
                if in_sidebar {
                    // Scroll the view by an offset (don't move the selection).
                    self.sidebar_scroll_by(if down { 3 } else { -3 });
                } else if in_terminal {
                    self.forward_terminal_mouse(&m);
                } else if down {
                    self.editor.scroll_down(3);
                } else {
                    self.editor.scroll_up(3);
                }
                self.needs_redraw = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if on_strip_divider {
                    self.resizing_strip = true;
                } else if on_term_divider {
                    self.resizing_term = true;
                } else if !in_terminal && self.on_scrollbar(&m) {
                    self.dragging_scrollbar = true;
                    self.scrollbar_grab = self.scrollbar_grab_offset(m.row);
                } else if on_sidebar_divider {
                    self.resizing = true;
                } else if in_sidebar {
                    self.focus = Focus::Sidebar;
                    if self.on_sidebar_scrollbar(&m) {
                        // Grab the thumb without jumping; only dragging scrolls.
                        self.dragging_sidebar_sb = true;
                        self.sidebar_sb_grab = self.sidebar_sb_grab_offset(m.row);
                    } else if self.sidebar_mode == SidebarMode::SourceControl {
                        self.scm_click(m.row);
                    } else if m.row >= 1 {
                        let idx = self.tree.scroll + (m.row as usize - 1);
                        if idx < self.tree.rows.len() {
                            self.tree.selected = idx;
                            // Record a potential drag; a plain click (no move) activates on release.
                            self.drag_source = Some(self.tree.rows[idx].path.clone());
                            self.dragging = false;
                            self.drag_pos = (m.column, m.row);
                        }
                    }
                } else if in_terminal {
                    self.focus = Focus::Terminal;
                    if rect_contains(self.panel.tablist_area, m.column, m.row) {
                        self.handle_tablist_click(m.column, m.row);
                    } else {
                        self.forward_terminal_mouse(&m);
                    }
                } else if self.editor_visible {
                    self.focus = Focus::Editor;
                    if self.on_hbar(&m) {
                        // Grab the horizontal scrollbar without jumping.
                        self.dragging_hbar = true;
                        self.hbar_grab = self.hbar_grab_offset(m.column);
                    } else if m.row == self.editor.tabbar_row {
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
                if self.dragging_hbar {
                    self.hbar_to(m.column);
                } else if self.dragging_sidebar_sb {
                    self.sidebar_sb_to(m.row);
                } else if self.drag_source.is_some() {
                    // Dragging a file out of the explorer.
                    self.dragging = true;
                    self.drag_pos = (m.column, m.row);
                } else if self.dragging_scrollbar {
                    self.scroll_bar_to(m.row);
                } else if self.resizing {
                    let max = self.term_width.saturating_sub(15).max(15);
                    self.sidebar_width = m.column.clamp(15, max);
                } else if self.resizing_term {
                    self.resize_term_to(m.column);
                } else if self.resizing_strip {
                    // Strip grows toward the left as the divider moves left.
                    let w = self.panel_inner_right.saturating_sub(m.column);
                    self.panel_strip_w = w.clamp(5, 40);
                } else if in_terminal {
                    self.forward_terminal_mouse(&m);
                } else if self.editor.selection.is_some() {
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
                if self.dragging_hbar {
                    self.dragging_hbar = false;
                } else if self.dragging_sidebar_sb {
                    self.dragging_sidebar_sb = false;
                } else if let Some(path) = self.drag_source.take() {
                    if self.dragging {
                        self.handle_drop(&path, m.column, m.row);
                    } else {
                        // No movement: treat as a click (open file / toggle folder).
                        self.activate_selected();
                    }
                    self.dragging = false;
                } else if self.dragging_scrollbar {
                    self.dragging_scrollbar = false;
                } else if self.resizing {
                    self.resizing = false;
                } else if self.resizing_term {
                    self.resizing_term = false;
                } else if self.resizing_strip {
                    self.resizing_strip = false;
                } else if in_terminal {
                    self.forward_terminal_mouse(&m);
                } else if let Some(text) = self.editor.selected_text() {
                    let n = text.chars().count();
                    self.clipboard = text.clone();
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

    /// Forward a mouse event to the active terminal, relative to its content area.
    fn forward_terminal_mouse(&mut self, m: &MouseEvent) {
        let inner = self.panel.content_area;
        if inner.width == 0
            || inner.height == 0
            || m.column < inner.x
            || m.column >= inner.x + inner.width
            || m.row < inner.y
            || m.row >= inner.y + inner.height
        {
            return;
        }
        let col = m.column - inner.x + 1;
        let row = m.row - inner.y + 1;
        if let Some(t) = self.panel.active_mut() {
            t.send_mouse(m.kind, col, row);
        }
    }

    /// Handle a click in the vertical tab strip: select a terminal, close it (✕ at the right
    /// edge), or add a new one (the "+ new" row below the terminals).
    fn handle_tablist_click(&mut self, col: u16, row: u16) {
        let strip = self.panel.tablist_area;
        let idx = (row - strip.y) as usize;
        let count = self.panel.terms.len();
        match idx.cmp(&count) {
            // The "+ new" row, just past the terminals.
            std::cmp::Ordering::Equal => self.spawn_terminal(),
            // A terminal row: the last two columns are the ✕ close button.
            std::cmp::Ordering::Less => {
                if col >= strip.x + strip.width.saturating_sub(2) {
                    self.panel.close(idx);
                    if self.panel.is_empty() {
                        self.focus = Focus::Editor;
                    }
                } else {
                    self.panel.active = idx;
                }
            }
            std::cmp::Ordering::Greater => {}
        }
    }

    /// Adjust the editor/terminal split from the divider column.
    fn resize_term_to(&mut self, col: u16) {
        let rest_x = if self.sidebar_visible {
            self.sidebar_width
        } else {
            0
        };
        let rest_w = self.term_width.saturating_sub(rest_x).max(1) as i32;
        let term_w = (self.term_width as i32 - col as i32).max(0);
        let ratio = (term_w * 100 / rest_w).clamp(15, 85);
        self.term_ratio = ratio as u16;
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
        let col =
            ((m.column - ca.x) as usize + buf.hscroll).min(buf.line_text(line).chars().count());
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
        let c = ((col - ca.x) as usize + buf.hscroll).min(buf.line_text(line).chars().count());
        Some((line, c))
    }

    fn on_hbar(&self, m: &MouseEvent) -> bool {
        match self.editor_hbar {
            Some((x, y, width, _, _)) => m.row == y && m.column >= x && m.column < x + width,
            None => false,
        }
    }

    /// Offset between a grab column and the horizontal thumb's left edge.
    fn hbar_grab_offset(&self, col: u16) -> i32 {
        let Some((x, _, width, thumb, max_scroll)) = self.editor_hbar else {
            return 0;
        };
        let hscroll = self.editor.active_buffer().map(|b| b.hscroll).unwrap_or(0);
        let denom = (width as usize).saturating_sub(thumb).max(1);
        let thumb_x = if max_scroll > 0 {
            hscroll * denom / max_scroll
        } else {
            0
        };
        col as i32 - x as i32 - thumb_x as i32
    }

    /// Set horizontal scroll from a drag column (thumb tracks the cursor via the grab offset).
    fn hbar_to(&mut self, col: u16) {
        let Some((x, _, width, thumb, max_scroll)) = self.editor_hbar else {
            return;
        };
        let denom = (width as usize).saturating_sub(thumb).max(1);
        let target = (col as i32 - x as i32 - self.hbar_grab).clamp(0, denom as i32) as usize;
        let hscroll = target * max_scroll / denom;
        if let Some(b) = self.editor.active_buffer_mut() {
            b.hscroll = hscroll;
        }
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
                    self.editor_visible = true; // opening a file reveals a hidden editor
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
        // Restart the blink so the cursor is solid right after a keystroke.
        self.blink_start = Instant::now();
        self.blink_on = true;
        // A pending Claude diff captures input (accept/reject/scroll).
        if self.pending_diff.is_some() {
            self.handle_pending_diff(key);
            return;
        }
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
        if self.palette.is_some() {
            self.handle_palette(key);
            return;
        }
        if self.prompt.is_some() {
            self.handle_prompt(key);
            return;
        }
        if self.search.is_some() {
            self.handle_search(key);
            return;
        }
        // When the terminal is focused, almost everything is forwarded to Claude, so it does
        // NOT go through the global shortcut handler (only a few warren keys are reserved).
        if self.focus == Focus::Terminal {
            self.handle_terminal(key);
            return;
        }
        if self.handle_global(key) {
            return;
        }
        match self.focus {
            Focus::Sidebar => self.handle_sidebar(key),
            Focus::Editor => self.handle_editor(key),
            Focus::Terminal => {}
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
            if !self.sidebar_visible && self.focus == Focus::Sidebar {
                self.cycle_focus();
            } else if self.sidebar_visible {
                self.focus = Focus::Sidebar;
            }
        } else if km.toggle_editor.matches(c, m) {
            self.editor_visible = !self.editor_visible;
            if !self.editor_visible && self.focus == Focus::Editor {
                self.cycle_focus();
            }
        } else if km.toggle_scm.matches(c, m) {
            self.sidebar_mode = if self.sidebar_mode == SidebarMode::SourceControl {
                SidebarMode::Explorer
            } else {
                SidebarMode::SourceControl
            };
            self.sidebar_visible = true;
            self.focus = Focus::Sidebar;
            if self.sidebar_mode == SidebarMode::SourceControl {
                self.refresh_git();
            }
        } else if km.focus_next.matches(c, m) {
            self.cycle_focus();
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
            self.open_palette();
        } else if km.new_terminal.matches(c, m) || is_ctrl_tilde(c, m) {
            self.spawn_terminal();
        } else if km.toggle_panel.matches(c, m) {
            self.toggle_panel();
        } else {
            return false;
        }
        true
    }

    /// Cycle focus across the visible panes: sidebar → editor → terminal (only those shown).
    fn cycle_focus(&mut self) {
        let mut order = Vec::new();
        if self.sidebar_visible {
            order.push(Focus::Sidebar);
        }
        if self.editor_visible {
            order.push(Focus::Editor);
        }
        if self.panel.visible && !self.panel.is_empty() {
            order.push(Focus::Terminal);
        }
        if order.is_empty() {
            return;
        }
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = order[(i + 1) % order.len()];
    }

    /// Spawn a new terminal (a shell) in the panel and focus it. Run `claude`, `npm`, etc. in it.
    fn spawn_terminal(&mut self) {
        let Some(tx) = self.tx.clone() else {
            return;
        };
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        // Reuse the smallest free number so closing terminals frees their label.
        let mut n = 1;
        while self.panel.terms.iter().any(|t| t.name == n.to_string()) {
            n += 1;
        }
        let name = n.to_string();
        // Point any `claude` launched in this terminal at warren's IDE server.
        let mut env = Vec::new();
        if let Some(port) = self.ide_port {
            env.push(("CLAUDE_CODE_SSE_PORT".to_string(), port.to_string()));
            env.push(("ENABLE_IDE_INTEGRATION".to_string(), "true".to_string()));
        }
        // Size is provisional; the renderer resizes the PTY to its actual area next frame.
        match TerminalPane::spawn(&shell, &name, &self.workspace, 24, 80, tx, &env) {
            Ok(pane) => {
                self.panel.add(pane);
                self.focus = Focus::Terminal;
            }
            Err(e) => self.status = format!("terminal failed: {e}"),
        }
    }

    /// Toggle the terminal panel: focus it if hidden/unfocused (spawning a terminal if empty),
    /// or hide it if it's already focused.
    fn toggle_panel(&mut self) {
        if self.panel.visible {
            if self.focus == Focus::Terminal {
                self.panel.visible = false;
                self.focus = Focus::Editor;
            } else {
                self.focus = Focus::Terminal;
            }
        } else if self.panel.is_empty() {
            self.spawn_terminal();
        } else {
            self.panel.visible = true;
            self.focus = Focus::Terminal;
        }
    }

    /// Keys while the panel is focused: a few warren keys are reserved for managing the panel,
    /// everything else is forwarded to the active terminal.
    fn handle_terminal(&mut self, key: KeyEvent) {
        let km = &self.config.keymap;
        let (c, m) = (key.code, key.modifiers);
        if km.quit.matches(c, m) {
            self.should_quit = true;
        } else if km.help.matches(c, m) {
            self.show_help = true;
        } else if km.focus_next.matches(c, m) {
            self.cycle_focus();
        } else if km.toggle_sidebar.matches(c, m) {
            self.sidebar_visible = !self.sidebar_visible;
        } else if km.toggle_editor.matches(c, m) {
            self.editor_visible = !self.editor_visible;
        } else if km.toggle_panel.matches(c, m) {
            self.toggle_panel();
        } else if km.command_palette.matches(c, m) {
            self.open_palette();
        } else if km.new_terminal.matches(c, m) || is_ctrl_tilde(c, m) {
            self.spawn_terminal();
        } else if km.next_tab.matches(c, m) {
            self.panel.next();
        } else if km.prev_tab.matches(c, m) {
            self.panel.prev();
        } else if km.close_tab.matches(c, m) {
            self.panel.close(self.panel.active);
            if self.panel.is_empty() {
                self.focus = Focus::Editor;
            }
        } else if let Some(t) = self.panel.active_mut() {
            t.send_key(c, m);
        }
    }

    /// Drop a dragged explorer path: onto a terminal inserts its (workspace-relative) path;
    /// onto the editor opens the file.
    fn handle_drop(&mut self, path: &Path, col: u16, row: u16) {
        let term_open = self.panel.visible && !self.panel.is_empty();
        if term_open && rect_contains(self.terminal_area, col, row) {
            let text = self.drop_text(path);
            if let Some(t) = self.panel.active_mut() {
                t.send_paste(&text);
            }
            self.focus = Focus::Terminal;
            self.status = format!("inserted {}", path.display());
        } else if self.editor_visible && rect_contains(self.editor_area, col, row) && path.is_file()
        {
            match self.editor.open(path) {
                Ok(()) => {
                    self.editor_visible = true;
                    self.focus = Focus::Editor;
                    self.status = path.display().to_string();
                }
                Err(e) => self.status = format!("error: {e}"),
            }
        }
    }

    /// The text inserted when a path is dropped on a terminal: workspace-relative, quoted if it
    /// contains spaces, with a trailing space.
    fn drop_text(&self, path: &Path) -> String {
        let rel = path.strip_prefix(&self.workspace).unwrap_or(path);
        let s = rel.to_string_lossy();
        if s.contains(' ') {
            format!("\"{s}\" ")
        } else {
            format!("{s} ")
        }
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
                        self.editor_visible = true;
                        self.focus = Focus::Editor;
                        self.status = format!("created {}", path.display());
                    }
                    Err(e) => self.status = format!("error: {e}"),
                }
            }
            PromptKind::Commit => {
                let msg = prompt.input.trim().to_string();
                if msg.is_empty() {
                    return;
                }
                match self.git.as_ref().map(|g| g.commit(&msg)) {
                    Some(Ok(())) => {
                        self.status = "committed".into();
                        self.refresh_git();
                    }
                    Some(Err(e)) => self.status = format!("commit failed: {e}"),
                    None => self.status = "no git repo".into(),
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

    fn open_palette(&mut self) {
        self.palette = Some(Palette::new(palette::gather_files(&self.workspace)));
    }

    fn handle_palette(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Enter => {
                let choice = self.palette.as_ref().and_then(|p| p.choose(&self.workspace));
                self.palette = None;
                match choice {
                    Some(Choice::File(path)) => match self.editor.open(&path) {
                        Ok(()) => {
                            self.editor_visible = true;
                            self.focus = Focus::Editor;
                            self.status = path.display().to_string();
                        }
                        Err(e) => self.status = format!("error: {e}"),
                    },
                    Some(Choice::Command(cmd)) => self.run_command(cmd),
                    None => {}
                }
            }
            KeyCode::Up => {
                if let Some(p) = &mut self.palette {
                    p.move_up();
                }
            }
            KeyCode::Down => {
                if let Some(p) = &mut self.palette {
                    p.move_down();
                }
            }
            KeyCode::Backspace => {
                if let Some(p) = &mut self.palette {
                    p.backspace();
                }
            }
            KeyCode::Left => {
                if let Some(p) = &mut self.palette {
                    p.move_left();
                }
            }
            KeyCode::Right => {
                if let Some(p) = &mut self.palette {
                    p.move_right();
                }
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                if let Some(p) = &mut self.palette {
                    p.insert_char(c);
                }
            }
            _ => {}
        }
    }

    /// Save theme + solid-background to state.toml so they persist across launches.
    fn persist_ui_state(&self) {
        crate::config::save_state(crate::theme::current().name, self.solid_bg);
    }

    fn run_command(&mut self, cmd: Command) {
        match cmd {
            Command::NewFile => self.open_new_file_prompt(),
            Command::NewTerminal => self.spawn_terminal(),
            Command::TogglePanel => self.toggle_panel(),
            Command::ToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
                if !self.sidebar_visible && self.focus == Focus::Sidebar {
                    self.cycle_focus();
                }
            }
            Command::ToggleEditor => {
                self.editor_visible = !self.editor_visible;
                if !self.editor_visible && self.focus == Focus::Editor {
                    self.cycle_focus();
                }
            }
            Command::Save => match self.editor.save_active() {
                Ok(()) => self.status = "saved".into(),
                Err(e) => self.status = format!("save failed: {e}"),
            },
            Command::SelectAll => {
                self.editor.select_all();
                self.focus = Focus::Editor;
            }
            Command::ToggleScrollbar => self.show_scrollbar = !self.show_scrollbar,
            Command::ToggleAutosave => {
                self.auto_save = !self.auto_save;
                self.status = if self.auto_save {
                    "auto-save: on".into()
                } else {
                    "auto-save: off".into()
                };
            }
            Command::ToggleSolidBg => {
                self.solid_bg = !self.solid_bg;
                self.status = if self.solid_bg {
                    "solid background: on".into()
                } else {
                    "solid background: off".into()
                };
                self.persist_ui_state();
            }
            Command::CycleTheme => {
                self.status = format!("theme: {}", crate::theme::cycle());
                self.persist_ui_state();
            }
            Command::SetTheme(i) => {
                crate::theme::set(i);
                self.status = format!("theme: {}", crate::theme::current().name);
                self.persist_ui_state();
            }
            Command::Help => self.show_help = true,
            Command::Quit => self.should_quit = true,
        }
    }

    /// Show a proposed diff (old file on disk vs Claude's new contents) and await accept/reject.
    fn open_diff(
        &mut self,
        path: String,
        new_contents: String,
        tab_name: String,
        reply: oneshot::Sender<DiffDecision>,
    ) {
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let name = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(tab_name);
        // Full-file inline diff: every line of the new file, with removed old lines interleaved
        // where they were. Rows carry real old/new line numbers for a VS Code-style dual gutter.
        let diff = similar::TextDiff::from_lines(&old, &new_contents);
        let mut text = String::new();
        let mut rows: Vec<editor::DiffRow> = Vec::new();
        for change in diff.iter_all_changes() {
            let kind = match change.tag() {
                similar::ChangeTag::Equal => editor::DiffKind::Context,
                similar::ChangeTag::Delete => editor::DiffKind::Del,
                similar::ChangeTag::Insert => editor::DiffKind::Add,
            };
            rows.push(editor::DiffRow {
                kind,
                old: change.old_index().map(|i| i + 1),
                new: change.new_index().map(|i| i + 1),
            });
            let line = change.value().trim_end_matches(['\n', '\r']);
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(line);
        }
        self.editor
            .open_diff_view(format!("✎ {name}"), Path::new(&path), &text, rows);
        self.editor_visible = true;
        self.focus = Focus::Editor;
        self.pending_diff = Some(PendingDiff {
            reply,
            new_contents,
        });
        self.status = "Claude wants to edit — ⏎ accept · Esc reject".into();
        self.needs_redraw = true;
    }

    fn close_diff_tab(&mut self) {
        if let Some(i) = self.editor.tabs.iter().position(|b| b.name.starts_with('✎')) {
            self.editor.close_at(i);
        }
    }

    fn resolve_diff(&mut self, accept: bool) {
        if let Some(pd) = self.pending_diff.take() {
            let decision = if accept {
                DiffDecision::Accept(pd.new_contents)
            } else {
                DiffDecision::Reject
            };
            let _ = pd.reply.send(decision);
            self.status = if accept { "accepted edit" } else { "rejected edit" }.into();
            self.close_diff_tab();
        }
    }

    fn handle_pending_diff(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => self.resolve_diff(true),
            KeyCode::Esc | KeyCode::Char('n') => self.resolve_diff(false),
            // Scroll the diff while deciding.
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                let vp = self.editor.viewport.max(1);
                if let Some(b) = self.editor.active_buffer_mut() {
                    match key.code {
                        KeyCode::Up => b.move_up(1),
                        KeyCode::Down => b.move_down(1),
                        KeyCode::PageUp => b.move_up(vp),
                        KeyCode::PageDown => b.move_down(vp),
                        _ => {}
                    }
                    b.ensure_cursor_visible(vp, 1);
                }
            }
            _ => {}
        }
    }

    fn handle_search(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // Ctrl+F again (or Enter/Down) goes to the next match.
        if self.config.keymap.find.matches(key.code, key.modifiers) {
            if let Some(s) = &mut self.search {
                s.next();
            }
            self.jump_to_active();
            return;
        }
        match key.code {
            KeyCode::Esc => self.search = None,
            KeyCode::Enter | KeyCode::Down => {
                if let Some(s) = &mut self.search {
                    s.next();
                }
                self.jump_to_active();
            }
            KeyCode::Up => {
                if let Some(s) = &mut self.search {
                    s.prev();
                }
                self.jump_to_active();
            }
            KeyCode::Backspace => {
                if let Some(s) = &mut self.search {
                    s.backspace();
                }
                self.recompute_search();
            }
            KeyCode::Left => {
                if let Some(s) = &mut self.search {
                    s.move_left();
                }
            }
            KeyCode::Right => {
                if let Some(s) = &mut self.search {
                    s.move_right();
                }
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                if let Some(s) = &mut self.search {
                    s.insert_char(c);
                }
                self.recompute_search();
            }
            _ => {}
        }
    }

    /// Recompute search matches in the active buffer (case-insensitive) and jump to the first
    /// match at/after the cursor.
    fn recompute_search(&mut self) {
        let query = self
            .search
            .as_ref()
            .map(|s| s.query.to_lowercase())
            .unwrap_or_default();
        let mut matches = Vec::new();
        if !query.is_empty() {
            if let Some(b) = self.editor.active_buffer() {
                for li in 0..b.line_count() {
                    let line = b.line_text(li).to_lowercase();
                    let mut start = 0;
                    while let Some(pos) = line[start..].find(&query) {
                        let bp = start + pos;
                        matches.push((li, line[..bp].chars().count()));
                        start = bp + query.len().max(1);
                        if start > line.len() {
                            break;
                        }
                    }
                }
            }
        }
        let cursor = self.editor.active_buffer().map(|b| b.cursor).unwrap_or((0, 0));
        if let Some(s) = &mut self.search {
            s.active = matches
                .iter()
                .position(|&(l, c)| (l, c) >= cursor)
                .unwrap_or(0);
            s.matches = matches;
        }
        self.jump_to_active();
    }

    fn jump_to_active(&mut self) {
        let m = self.search.as_ref().and_then(|s| s.active_match());
        if let Some((line, col)) = m {
            let (vp, vw) = (self.editor.viewport.max(1), self.editor.viewport_w.max(1));
            if let Some(b) = self.editor.active_buffer_mut() {
                b.set_cursor(line, col);
                b.ensure_cursor_visible(vp, vw);
            }
        }
    }

    fn handle_sidebar(&mut self, key: KeyEvent) {
        match self.sidebar_mode {
            SidebarMode::Explorer => {
                match key.code {
                    KeyCode::Up => self.tree.move_up(),
                    KeyCode::Down => self.tree.move_down(),
                    KeyCode::Right => self.tree.expand(),
                    KeyCode::Left => self.tree.collapse(),
                    KeyCode::Enter => self.activate_selected(),
                    _ => {}
                }
                let vp = self.tree.viewport;
                self.tree.ensure_visible(vp);
            }
            SidebarMode::SourceControl => self.handle_scm_keys(key),
        }
    }

    fn handle_scm_keys(&mut self, key: KeyEvent) {
        let total = self.scm_items.len();
        match key.code {
            KeyCode::Up => {
                self.scm_selected = self.scm_selected.saturating_sub(1);
                self.scm_ensure_visible();
            }
            KeyCode::Down => {
                if self.scm_selected + 1 < total {
                    self.scm_selected += 1;
                }
                self.scm_ensure_visible();
            }
            KeyCode::Enter => self.scm_activate(),
            KeyCode::Char('s') => self.scm_stage_toggle(),
            KeyCode::Char('c') => self.prompt = Some(Prompt::commit()),
            _ => {}
        }
    }

    /// Keep the selected SCM item within the visible window by adjusting `scm_scroll`.
    fn scm_ensure_visible(&mut self) {
        let h = self.scm_viewport;
        if h == 0 {
            return;
        }
        if let Some(&line) = self.scm_item_lines.get(self.scm_selected) {
            if line < self.scm_scroll {
                self.scm_scroll = line;
            } else if line >= self.scm_scroll + h {
                self.scm_scroll = line + 1 - h;
            }
        }
    }

    pub fn has_git(&self) -> bool {
        self.git.is_some()
    }

    /// Recompute git state from the repository, then rebuild the flattened item list.
    fn refresh_git(&mut self) {
        if let Some(g) = &self.git {
            self.git_branch = g.branch();
            self.git_changes = g.changes();
            self.git_commits = g.log(100);
        }
        self.rebuild_scm_items();
    }

    /// Flatten changes + the (expandable) commit graph into the selectable item list.
    fn rebuild_scm_items(&mut self) {
        let mut items = Vec::new();
        for i in 0..self.git_changes.len() {
            items.push(ScmItem::Change(i));
        }
        for (j, c) in self.git_commits.iter().enumerate() {
            items.push(ScmItem::Commit(j));
            if self.git_expanded.contains(&c.id) {
                if let Some(g) = &self.git {
                    for (path, code) in g.commit_files(c.id) {
                        items.push(ScmItem::CommitFile { commit: j, path, code });
                    }
                }
            }
        }
        self.scm_items = items;
        if self.scm_selected >= self.scm_items.len() {
            self.scm_selected = self.scm_items.len().saturating_sub(1);
        }
    }

    /// Enter on an SCM row: change → its diff; commit → expand/collapse; commit-file → its diff.
    fn scm_activate(&mut self) {
        let Some(item) = self.scm_items.get(self.scm_selected).cloned() else {
            return;
        };
        match item {
            ScmItem::Change(i) => {
                let path = self.git_changes[i].path.clone();
                if let Some(diff) = self.git.as_ref().map(|g| g.file_diff(&path)) {
                    self.editor.open_virtual(format!("◆ {path}"), "diff", &diff);
                    self.editor_visible = true;
                    self.focus = Focus::Editor;
                }
            }
            ScmItem::Commit(j) => {
                if let Some((oid, short)) =
                    self.git_commits.get(j).map(|c| (c.id, c.short.clone()))
                {
                    // Toggle the file list...
                    if !self.git_expanded.insert(oid) {
                        self.git_expanded.remove(&oid);
                    }
                    self.rebuild_scm_items();
                    // ...and show the commit's message + info in the editor (keep sidebar focus
                    // so you can keep browsing commits).
                    if let Some(details) = self.git.as_ref().map(|g| g.commit_details(oid)) {
                        self.editor
                            .open_virtual(format!("● {short}"), "diff", &details);
                        self.editor_visible = true;
                    }
                }
            }
            ScmItem::CommitFile { commit, path, .. } => {
                if let Some((oid, short)) =
                    self.git_commits.get(commit).map(|c| (c.id, c.short.clone()))
                {
                    if let Some(diff) = self.git.as_ref().map(|g| g.commit_file_diff(oid, &path)) {
                        self.editor
                            .open_virtual(format!("● {short} ◆ {path}"), "diff", &diff);
                        self.editor_visible = true;
                        self.focus = Focus::Editor;
                    }
                }
            }
        }
    }

    fn scm_stage_toggle(&mut self) {
        let Some(ScmItem::Change(i)) = self.scm_items.get(self.scm_selected).cloned() else {
            return;
        };
        let change = &self.git_changes[i];
        let (path, staged) = (change.path.clone(), change.staged);
        let res = self.git.as_ref().map(|g| {
            if staged {
                g.unstage(&path)
            } else {
                g.stage(&path)
            }
        });
        if let Some(Err(e)) = res {
            self.status = format!("git: {e}");
        }
        self.refresh_git();
    }

    fn on_sidebar_scrollbar(&self, m: &MouseEvent) -> bool {
        match self.sidebar_sb {
            Some((col, y, h, _)) => m.column == col && m.row >= y && m.row < y + h,
            None => false,
        }
    }

    fn sidebar_scroll(&self) -> usize {
        match self.sidebar_mode {
            SidebarMode::Explorer => self.tree.scroll,
            SidebarMode::SourceControl => self.scm_scroll,
        }
    }

    /// Offset between a grab row and the sidebar thumb's top, so dragging holds the thumb.
    fn sidebar_sb_grab_offset(&self, row: u16) -> i32 {
        let Some((_, y, h, total)) = self.sidebar_sb else {
            return 0;
        };
        let track = h as usize;
        let thumb = (track * track / total.max(1)).clamp(1, track);
        let max_scroll = total.saturating_sub(track);
        let denom = track.saturating_sub(thumb).max(1);
        let thumb_top = if max_scroll > 0 {
            self.sidebar_scroll() * denom / max_scroll
        } else {
            0
        };
        row.saturating_sub(y) as i32 - thumb_top as i32
    }

    /// Drag the sidebar scrollbar: set the scroll offset so the thumb tracks the cursor.
    fn sidebar_sb_to(&mut self, row: u16) {
        let Some((_, y, h, total)) = self.sidebar_sb else {
            return;
        };
        let track = h as usize;
        let thumb = (track * track / total.max(1)).clamp(1, track);
        let max_scroll = total.saturating_sub(track);
        let denom = track.saturating_sub(thumb).max(1);
        let target = (row as i32 - y as i32 - self.sidebar_sb_grab).clamp(0, denom as i32) as usize;
        let scroll = target * max_scroll / denom;
        match self.sidebar_mode {
            SidebarMode::Explorer => self.tree.scroll = scroll,
            SidebarMode::SourceControl => self.scm_scroll = scroll,
        }
    }

    /// Wheel-scroll the sidebar view by `delta` rows (offset only, selection untouched).
    fn sidebar_scroll_by(&mut self, delta: i32) {
        match self.sidebar_mode {
            SidebarMode::Explorer => {
                let max = self.tree.rows.len().saturating_sub(self.tree.viewport);
                self.tree.scroll =
                    (self.tree.scroll as i32 + delta).clamp(0, max as i32) as usize;
            }
            SidebarMode::SourceControl => {
                let max = self.scm_total_lines.saturating_sub(self.scm_viewport);
                self.scm_scroll = (self.scm_scroll as i32 + delta).clamp(0, max as i32) as usize;
            }
        }
    }

    /// Map a click row in the SCM sidebar to its item and activate it.
    fn scm_click(&mut self, row: u16) {
        if let Some(&(_, idx)) = self.scm_rows.iter().find(|(y, _)| *y == row) {
            self.scm_selected = idx;
            self.scm_activate();
        }
    }

    fn handle_editor(&mut self, key: KeyEvent) {
        let km = &self.config.keymap;
        let (c, m) = (key.code, key.modifiers);
        let ctrl = m.contains(KeyModifiers::CONTROL);
        let alt = m.contains(KeyModifiers::ALT);

        if km.copy.matches(c, m) {
            if let Some(text) = self.editor.selected_text() {
                let n = text.chars().count();
                self.clipboard = text.clone();
                copy_to_clipboard(&text);
                self.status = format!("copied {n} chars");
            }
            return;
        }
        if km.paste.matches(c, m) {
            let text = self.clipboard.clone();
            if !text.is_empty() {
                let vp = self.editor.viewport.max(1);
                let vw = self.editor.viewport_w.max(1);
                self.editor.delete_selection();
                if let Some(b) = self.editor.active_buffer_mut() {
                    if !b.is_readonly() {
                        b.insert_text(&text);
                        b.ensure_cursor_visible(vp, vw);
                    }
                }
            }
            return;
        }
        if km.undo.matches(c, m) {
            let (vp, vw) = (self.editor.viewport.max(1), self.editor.viewport_w.max(1));
            if let Some(b) = self.editor.active_buffer_mut() {
                if b.undo() {
                    b.ensure_cursor_visible(vp, vw);
                }
            }
            return;
        }
        if km.redo.matches(c, m) {
            let (vp, vw) = (self.editor.viewport.max(1), self.editor.viewport_w.max(1));
            if let Some(b) = self.editor.active_buffer_mut() {
                if b.redo() {
                    b.ensure_cursor_visible(vp, vw);
                }
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
        if km.find.matches(c, m) {
            self.search = Some(Search::default());
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
        let vw = self.editor.viewport_w.max(1);

        // Read-only buffers (diffs, commit details): movement/scroll only.
        if self.editor.active_buffer().map(|b| b.is_readonly()).unwrap_or(false) {
            if let Some(b) = self.editor.active_buffer_mut() {
                match c {
                    KeyCode::Up => b.move_up(1),
                    KeyCode::Down => b.move_down(1),
                    KeyCode::PageUp => b.move_up(vp),
                    KeyCode::PageDown => b.move_down(vp),
                    KeyCode::Left if ctrl || alt => b.move_word_left(),
                    KeyCode::Right if ctrl || alt => b.move_word_right(),
                    KeyCode::Left => b.move_left(),
                    KeyCode::Right => b.move_right(),
                    KeyCode::Home => b.set_cursor(0, 0),
                    KeyCode::End => {
                        let last = b.line_count().saturating_sub(1);
                        b.set_cursor(last, usize::MAX);
                    }
                    _ => {}
                }
                b.ensure_cursor_visible(vp, vw);
            }
            return;
        }

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
                    b.ensure_cursor_visible(vp, vw);
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
                KeyCode::Backspace if ctrl || alt => {
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
                KeyCode::Left if ctrl || alt => {
                    b.move_word_left();
                    edited = true;
                }
                KeyCode::Right if ctrl || alt => {
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
                b.ensure_cursor_visible(vp, vw);
            }
        }
        if edited {
            self.editor.clear_selection();
        }
    }
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

/// Some terminals report Ctrl+Shift+` as Ctrl+~; accept it as the new-terminal chord too.
fn is_ctrl_tilde(code: KeyCode, mods: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('~')) && mods.contains(KeyModifiers::CONTROL)
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
