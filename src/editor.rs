//! The editor: open files as tabs, with a rope-backed editable buffer, a cursor, syntax
//! highlighting (recomputed lazily after edits), text selection, and save.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ropey::Rope;

use crate::highlight;

/// A text selection in buffer coordinates `(line, column)`, where column is a char index.
#[derive(Clone, Copy)]
pub struct Selection {
    pub anchor: (usize, usize),
    pub cursor: (usize, usize),
}

impl Selection {
    /// `(start, end)` ordered so start precedes end in document order.
    pub fn normalized(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.cursor
    }
}

/// One open file.
pub struct Buffer {
    pub path: PathBuf,
    pub name: String,
    /// Source of truth for the text.
    rope: Rope,
    /// Cached highlighted lines for rendering; rebuilt when `highlight_dirty`.
    pub lines: Vec<Line<'static>>,
    highlight_dirty: bool,
    /// Cursor as `(line, char column)`.
    pub cursor: (usize, usize),
    /// Top visible line.
    pub scroll: usize,
    /// Unsaved changes since the last load/save.
    pub modified: bool,
    /// When the buffer was last edited, used to debounce auto-save.
    last_edit: Instant,
    /// Read-only buffers (diffs, commit details) ignore edits and saves.
    readonly: bool,
    /// A diff/patch buffer: rendered with green/red line backgrounds.
    pub is_diff: bool,
}

impl Buffer {
    fn from_path(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let rope = Rope::from_str(&text);
        Ok(Self {
            lines: highlight::highlight_rope(path, &rope),
            path: path.to_path_buf(),
            name,
            rope,
            highlight_dirty: false,
            cursor: (0, 0),
            scroll: 0,
            modified: false,
            last_edit: Instant::now(),
            readonly: false,
            is_diff: false,
        })
    }

    /// A read-only buffer backed by in-memory text (e.g. a diff or commit details). `ext` drives
    /// syntax highlighting; `name` is the tab label. `ext == "diff"` enables green/red line
    /// backgrounds (and skips syntect, since the row backgrounds carry the meaning).
    fn from_virtual(name: String, ext: &str, text: &str) -> Self {
        let rope = Rope::from_str(text);
        let path = PathBuf::from(format!("\u{0}{name}.{ext}"));
        let is_diff = ext == "diff";
        let lines = if is_diff {
            let mut v: Vec<Line<'static>> = rope
                .lines()
                .map(|l| {
                    let mut s = l.to_string();
                    while s.ends_with('\n') || s.ends_with('\r') {
                        s.pop();
                    }
                    Line::raw(s)
                })
                .collect();
            if v.is_empty() {
                v.push(Line::raw(""));
            }
            v
        } else {
            highlight::highlight_rope(&path, &rope)
        };
        Self {
            lines,
            path,
            name,
            rope,
            highlight_dirty: false,
            cursor: (0, 0),
            scroll: 0,
            modified: false,
            last_edit: Instant::now(),
            readonly: true,
            is_diff,
        }
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Number of characters on line `i`, excluding the trailing newline.
    fn line_len(&self, i: usize) -> usize {
        if i >= self.rope.len_lines() {
            return 0;
        }
        let line = self.rope.line(i);
        let mut n = line.len_chars();
        if n > 0 && line.char(n - 1) == '\n' {
            n -= 1;
        }
        if n > 0 && line.char(n - 1) == '\r' {
            n -= 1;
        }
        n
    }

    /// Plain text of line `i`, without the trailing newline.
    pub fn line_text(&self, i: usize) -> String {
        if i >= self.rope.len_lines() {
            return String::new();
        }
        let mut s = self.rope.line(i).to_string();
        while s.ends_with('\n') || s.ends_with('\r') {
            s.pop();
        }
        s
    }

    fn cursor_char(&self) -> usize {
        self.rope.line_to_char(self.cursor.0) + self.cursor.1
    }

    pub fn is_readonly(&self) -> bool {
        self.readonly
    }

    fn touch(&mut self) {
        self.modified = true;
        self.highlight_dirty = true;
        self.last_edit = Instant::now();
    }

    pub fn insert_char(&mut self, ch: char) {
        if self.readonly {
            return;
        }
        let idx = self.cursor_char();
        self.rope.insert_char(idx, ch);
        if ch == '\n' {
            self.cursor = (self.cursor.0 + 1, 0);
        } else {
            self.cursor.1 += 1;
        }
        self.touch();
    }

    pub fn insert_str(&mut self, s: &str) {
        let idx = self.cursor_char();
        self.rope.insert(idx, s);
        self.cursor.1 += s.chars().count();
        self.touch();
    }

    /// Insert possibly-multi-line text (e.g. a paste) at the cursor, advancing the cursor.
    pub fn insert_text(&mut self, s: &str) {
        let idx = self.cursor_char();
        self.rope.insert(idx, s);
        let newlines = s.matches('\n').count();
        if newlines == 0 {
            self.cursor.1 += s.chars().count();
        } else {
            let last = s.rsplit('\n').next().unwrap_or("");
            self.cursor = (self.cursor.0 + newlines, last.chars().count());
        }
        self.touch();
    }

    /// Remove the character range from `(sl, sc)` to `(el, ec)` and place the cursor at the start.
    pub fn delete_range(&mut self, sl: usize, sc: usize, el: usize, ec: usize) {
        let start = self.rope.line_to_char(sl) + sc;
        let end = (self.rope.line_to_char(el) + ec).min(self.rope.len_chars());
        if start < end {
            self.rope.remove(start..end);
            self.cursor = (sl, sc);
            self.touch();
        }
    }

    pub fn backspace(&mut self) {
        let (l, c) = self.cursor;
        if c > 0 {
            let idx = self.cursor_char();
            self.rope.remove(idx - 1..idx);
            self.cursor.1 -= 1;
            self.touch();
        } else if l > 0 {
            let prev_len = self.line_len(l - 1);
            let idx = self.rope.line_to_char(l); // start of line l == just after prev newline
            self.rope.remove(idx - 1..idx);
            self.cursor = (l - 1, prev_len);
            self.touch();
        }
    }

    pub fn delete(&mut self) {
        let (l, c) = self.cursor;
        let idx = self.cursor_char();
        if c < self.line_len(l) {
            self.rope.remove(idx..idx + 1);
            self.touch();
        } else if l + 1 < self.rope.len_lines() {
            // At end of line: remove the newline to join the next line.
            self.rope.remove(idx..idx + 1);
            self.touch();
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
        } else if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = self.line_len(self.cursor.0);
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor.1 < self.line_len(self.cursor.0) {
            self.cursor.1 += 1;
        } else if self.cursor.0 + 1 < self.rope.len_lines() {
            self.cursor = (self.cursor.0 + 1, 0);
        }
    }

    pub fn move_up(&mut self, n: usize) {
        self.cursor.0 = self.cursor.0.saturating_sub(n);
        self.cursor.1 = self.cursor.1.min(self.line_len(self.cursor.0));
    }

    pub fn move_down(&mut self, n: usize) {
        self.cursor.0 = (self.cursor.0 + n).min(self.rope.len_lines().saturating_sub(1));
        self.cursor.1 = self.cursor.1.min(self.line_len(self.cursor.0));
    }

    /// Column at the start of the word to the left of `col` on `line`.
    fn word_left_col(&self, line: usize, col: usize) -> usize {
        let chars: Vec<char> = self.line_text(line).chars().collect();
        let mut i = col.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        if i > 0 {
            let cls = char_class(chars[i - 1]);
            while i > 0 && char_class(chars[i - 1]) == cls {
                i -= 1;
            }
        }
        i
    }

    pub fn move_word_left(&mut self) {
        if self.cursor.1 == 0 {
            self.move_left();
        } else {
            self.cursor.1 = self.word_left_col(self.cursor.0, self.cursor.1);
        }
    }

    pub fn move_word_right(&mut self) {
        let chars: Vec<char> = self.line_text(self.cursor.0).chars().collect();
        let len = chars.len();
        if self.cursor.1 >= len {
            self.move_right();
            return;
        }
        let mut i = self.cursor.1;
        let cls = char_class(chars[i]);
        if cls == 0 {
            while i < len && chars[i].is_whitespace() {
                i += 1;
            }
        } else {
            while i < len && char_class(chars[i]) == cls {
                i += 1;
            }
            while i < len && chars[i].is_whitespace() {
                i += 1;
            }
        }
        self.cursor.1 = i;
    }

    pub fn delete_word_back(&mut self) {
        if self.cursor.1 == 0 {
            self.backspace();
            return;
        }
        let target = self.word_left_col(self.cursor.0, self.cursor.1);
        let line_start = self.rope.line_to_char(self.cursor.0);
        self.rope.remove(line_start + target..line_start + self.cursor.1);
        self.cursor.1 = target;
        self.touch();
    }

    /// Place the cursor directly (used by clicks, Home/End); clamps to valid positions.
    pub fn set_cursor(&mut self, line: usize, col: usize) {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        self.cursor = (line, col.min(self.line_len(line)));
    }

    pub fn ensure_cursor_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.cursor.0 < self.scroll {
            self.scroll = self.cursor.0;
        } else if self.cursor.0 >= self.scroll + height {
            self.scroll = self.cursor.0 + 1 - height;
        }
    }

    fn rehighlight(&mut self) {
        if self.highlight_dirty {
            self.lines = highlight::highlight_rope(&self.path, &self.rope);
            self.highlight_dirty = false;
        }
    }

    fn save(&mut self) -> Result<()> {
        if self.readonly {
            return Ok(());
        }
        std::fs::write(&self.path, self.rope.to_string())
            .with_context(|| format!("writing {}", self.path.display()))?;
        self.modified = false;
        Ok(())
    }
}

/// Character class for word-wise motion: 0 = whitespace, 1 = word char, 2 = punctuation.
fn char_class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// The collection of open tabs.
#[derive(Default)]
pub struct Editor {
    pub tabs: Vec<Buffer>,
    pub active: usize,
    /// Last rendered content height, used to clamp scrolling and paging.
    pub viewport: usize,
    /// Clickable tab regions from the last frame: (x_start, x_end_exclusive, tab_index).
    pub tab_hitboxes: Vec<(u16, u16, usize)>,
    /// Clickable close-button regions from the last frame: (x_start, x_end_exclusive, tab_index).
    pub close_hitboxes: Vec<(u16, u16, usize)>,
    /// Screen row of the tab bar from the last frame, for click mapping.
    pub tabbar_row: u16,
    /// Editor content area (excluding border/tabs) from the last frame, for mouse mapping.
    pub content_area: Rect,
    /// Screen column of the scrollbar from the last frame, if shown (for drag mapping).
    pub scrollbar_col: Option<u16>,
    /// Active text selection, if any.
    pub selection: Option<Selection>,
}

impl Editor {
    /// Open a file, or switch to it if already open. Returns an error message on failure.
    pub fn open(&mut self, path: &Path) -> Result<()> {
        self.selection = None;
        if let Some(i) = self.tabs.iter().position(|b| b.path == path) {
            self.active = i;
            return Ok(());
        }
        let buf = Buffer::from_path(path)?;
        self.tabs.push(buf);
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    /// Open (or replace) a read-only tab showing in-memory text such as a diff.
    pub fn open_virtual(&mut self, name: String, ext: &str, text: &str) {
        self.selection = None;
        let buf = Buffer::from_virtual(name.clone(), ext, text);
        if let Some(i) = self.tabs.iter().position(|b| b.name == name) {
            self.tabs[i] = buf;
            self.active = i;
        } else {
            self.tabs.push(buf);
            self.active = self.tabs.len() - 1;
        }
    }

    pub fn active_buffer(&self) -> Option<&Buffer> {
        self.tabs.get(self.active)
    }

    pub fn active_buffer_mut(&mut self) -> Option<&mut Buffer> {
        self.tabs.get_mut(self.active)
    }

    /// Save the active buffer to disk.
    pub fn save_active(&mut self) -> Result<()> {
        match self.tabs.get_mut(self.active) {
            Some(b) => b.save(),
            None => Ok(()),
        }
    }

    /// Save the buffer at `idx` to disk.
    pub fn save_at(&mut self, idx: usize) -> Result<()> {
        match self.tabs.get_mut(idx) {
            Some(b) => b.save(),
            None => Ok(()),
        }
    }

    /// Whether the tab at `idx` has unsaved changes.
    pub fn is_modified(&self, idx: usize) -> bool {
        self.tabs.get(idx).map(|b| b.modified).unwrap_or(false)
    }

    /// Save any modified buffer that has been idle for at least `delay`. Returns whether
    /// anything was saved (so the caller can refresh the UI).
    pub fn autosave(&mut self, delay: Duration) -> bool {
        let mut saved = false;
        for b in &mut self.tabs {
            if b.modified && b.last_edit.elapsed() >= delay && b.save().is_ok() {
                saved = true;
            }
        }
        saved
    }

    /// Re-highlight any buffers edited since the last frame. Called once per tick so a burst of
    /// keystrokes coalesces into a single re-highlight before rendering.
    pub fn refresh_highlight(&mut self) {
        for b in &mut self.tabs {
            b.rehighlight();
        }
    }

    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
            self.selection = None;
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
            self.selection = None;
        }
    }

    /// Close the tab at `idx`, keeping the active tab pointing at a sensible neighbour.
    pub fn close_at(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        self.selection = None;
        self.tabs.remove(idx);
        if self.active > idx || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    /// Reload an open file from disk when the watcher reports it changed. Unsaved buffers are
    /// protected, and unchanged content (e.g. our own save) is ignored so the cursor doesn't jump.
    pub fn reload(&mut self, path: &Path) {
        let Some(i) = self.tabs.iter().position(|b| b.path == path) else {
            return;
        };
        if self.tabs[i].modified {
            return;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        if self.tabs[i].rope == text {
            return;
        }
        if let Ok(mut fresh) = Buffer::from_path(path) {
            let old = &self.tabs[i];
            let max_line = fresh.line_count().saturating_sub(1);
            fresh.cursor = (old.cursor.0.min(max_line), old.cursor.1);
            fresh.scroll = old.scroll.min(max_line);
            self.tabs[i] = fresh;
        }
    }

    fn max_scroll(&self) -> usize {
        self.active_buffer()
            .map(|b| b.line_count().saturating_sub(self.viewport.max(1)))
            .unwrap_or(0)
    }

    pub fn scroll_down(&mut self, n: usize) {
        let max = self.max_scroll();
        if let Some(b) = self.tabs.get_mut(self.active) {
            b.scroll = (b.scroll + n).min(max);
        }
    }

    pub fn scroll_up(&mut self, n: usize) {
        if let Some(b) = self.tabs.get_mut(self.active) {
            b.scroll = b.scroll.saturating_sub(n);
        }
    }

    /// Set the scroll position directly (used by scrollbar dragging), clamped to range.
    pub fn scroll_to(&mut self, pos: usize) {
        let max = self.max_scroll();
        if let Some(b) = self.tabs.get_mut(self.active) {
            b.scroll = pos.min(max);
        }
    }

    pub fn start_selection(&mut self, line: usize, col: usize) {
        self.selection = Some(Selection {
            anchor: (line, col),
            cursor: (line, col),
        });
    }

    pub fn update_selection(&mut self, line: usize, col: usize) {
        if let Some(s) = &mut self.selection {
            s.cursor = (line, col);
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Select the entire active buffer.
    pub fn select_all(&mut self) {
        let Some(b) = self.tabs.get(self.active) else {
            return;
        };
        let last = b.line_count().saturating_sub(1);
        let end_col = b.line_text(last).chars().count();
        self.selection = Some(Selection {
            anchor: (0, 0),
            cursor: (last, end_col),
        });
    }

    /// Delete the active selection from the buffer, if non-empty. Returns whether anything
    /// was deleted; clears the selection either way.
    pub fn delete_selection(&mut self) -> bool {
        let Some(sel) = self.selection.take() else {
            return false;
        };
        if sel.is_empty() {
            return false;
        }
        let ((sl, sc), (el, ec)) = sel.normalized();
        if let Some(b) = self.tabs.get_mut(self.active) {
            b.delete_range(sl, sc, el, ec);
        }
        true
    }

    /// The selected text, or `None` if there is no (non-empty) selection.
    pub fn selected_text(&self) -> Option<String> {
        let sel = self.selection?;
        if sel.is_empty() {
            return None;
        }
        let buf = self.active_buffer()?;
        let ((sl, sc), (el, ec)) = sel.normalized();
        let mut out = String::new();
        for li in sl..=el.min(buf.line_count().saturating_sub(1)) {
            let chars: Vec<char> = buf.line_text(li).chars().collect();
            let start = if li == sl { sc.min(chars.len()) } else { 0 };
            let end = if li == el { ec.min(chars.len()) } else { chars.len() };
            out.extend(&chars[start..end.max(start)]);
            if li != el {
                out.push('\n');
            }
        }
        Some(out)
    }
}
