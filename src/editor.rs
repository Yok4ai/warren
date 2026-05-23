//! The editor: open files as tabs, with a rope-backed editable buffer, a cursor, syntax
//! highlighting (recomputed lazily after edits), text selection, and save.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ropey::Rope;
use syntect::parsing::SyntaxReference;
use tokio::sync::mpsc::UnboundedSender;

use crate::event::AppEvent;
use crate::highlight::{self, LineState};

/// File extensions opened as images rather than text.
pub fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tiff" | "tif" | "qoi")
    )
}

fn decode_image_file(path: &Path) -> Option<image::DynamicImage> {
    image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()
}

fn decode_image_bytes(data: &[u8]) -> Option<image::DynamicImage> {
    image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
}

/// Download an image over HTTP(S) with a timeout. Returns the raw bytes (capped at 20 MB).
fn fetch_url(url: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let resp = ureq::get(url)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .ok()?;
    let mut buf = Vec::new();
    resp.into_reader().take(20_000_000).read_to_end(&mut buf).ok()?;
    Some(buf)
}

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

/// One row of a full-file inline diff (VS Code style): a line carried over unchanged, removed
/// from the old file, or added in the new one. `old`/`new` are 1-based line numbers in each side
/// (the gutter shows both), `None` where that side has no line.
#[derive(Clone, Copy, PartialEq)]
pub enum DiffKind {
    Context,
    Del,
    Add,
}

#[derive(Clone, Copy)]
pub struct DiffRow {
    pub kind: DiffKind,
    pub old: Option<usize>,
    pub new: Option<usize>,
}

/// An image embedded in a markdown preview: a reserved band of rows plus its (lazily loaded)
/// render protocol. `proto` is `None` until the image is decoded (and stays `None` if it fails).
pub struct PreviewImage {
    pub line: usize,
    pub height: u16,
    pub source: String,
    pub proto: Option<StatefulProtocol>,
    /// True once loading has been kicked off (so we don't request it every frame).
    pub requested: bool,
}

/// Cached markdown preview render at a particular width.
struct PreviewRender {
    width: usize,
    lines: Vec<Line<'static>>,
    images: Vec<PreviewImage>,
}

/// One open file.
pub struct Buffer {
    pub path: PathBuf,
    pub name: String,
    /// Source of truth for the text.
    rope: Rope,
    /// Cached highlighted lines for rendering.
    pub lines: Vec<Line<'static>>,
    /// syntect syntax for this buffer.
    syntax: &'static SyntaxReference,
    /// Per-line parse/highlight state for incremental re-highlighting.
    hl_states: Vec<LineState>,
    /// First line changed since the last re-highlight (None = clean).
    dirty_from: Option<usize>,
    /// Cursor as `(line, char column)`.
    pub cursor: (usize, usize),
    /// Top visible line.
    pub scroll: usize,
    /// Leftmost visible column (horizontal scroll).
    pub hscroll: usize,
    /// Unsaved changes since the last load/save.
    pub modified: bool,
    /// When the buffer was last edited, used to debounce auto-save.
    last_edit: Instant,
    /// Read-only buffers (diffs, commit details) ignore edits and saves.
    readonly: bool,
    /// A diff/patch buffer: rendered with green/red line backgrounds.
    pub is_diff: bool,
    /// For a full-file inline diff, per-row kind + old/new line numbers (parallel to `lines`).
    pub diff_rows: Option<Vec<DiffRow>>,
    /// Markdown preview toggle (only meaningful for .md buffers).
    pub preview: bool,
    /// Cached rendered markdown preview; invalidated on edit, re-rendered on width change.
    preview_cache: Option<PreviewRender>,
    /// For an image buffer, the resize-aware render protocol (kitty/sixel/iTerm2/half-blocks).
    pub image: Option<StatefulProtocol>,
    /// Undo/redo snapshots of `(rope, cursor)`. Rope clones are cheap (structural sharing).
    undo: Vec<(Rope, (usize, usize))>,
    redo: Vec<(Rope, (usize, usize))>,
}

const UNDO_LIMIT: usize = 1000;

impl Buffer {
    fn from_path(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let rope = Rope::from_str(&text);
        let syntax = highlight::syntax_for(path);
        let (lines, hl_states) = highlight::full(syntax, &rope);
        Ok(Self {
            lines,
            path: path.to_path_buf(),
            name,
            rope,
            syntax,
            hl_states,
            dirty_from: None,
            cursor: (0, 0),
            scroll: 0,
            hscroll: 0,
            modified: false,
            last_edit: Instant::now(),
            readonly: false,
            is_diff: false,
            diff_rows: None,
            preview: false,
            preview_cache: None,
            image: None,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    /// A read-only buffer backed by in-memory text (e.g. a diff or commit details). `ext` drives
    /// syntax highlighting; `name` is the tab label. `ext == "diff"` enables green/red line
    /// backgrounds (and skips syntect, since the row backgrounds carry the meaning).
    fn from_virtual(name: String, ext: &str, text: &str) -> Self {
        let rope = Rope::from_str(text);
        let path = PathBuf::from(format!("\u{0}{name}.{ext}"));
        let syntax = highlight::syntax_for(&path);
        let is_diff = ext == "diff";
        let (lines, hl_states) = if is_diff {
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
            (v, Vec::new())
        } else {
            highlight::full(syntax, &rope)
        };
        Self {
            lines,
            path,
            name,
            rope,
            syntax,
            hl_states,
            dirty_from: None,
            cursor: (0, 0),
            scroll: 0,
            hscroll: 0,
            modified: false,
            last_edit: Instant::now(),
            readonly: true,
            is_diff,
            diff_rows: None,
            preview: false,
            preview_cache: None,
            image: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// A read-only full-file inline diff: `text` is the interleaved old+new content (newlines
    /// stripped per row), `rows` is the parallel per-row diff metadata, and `path` selects the
    /// syntax so the code is highlighted normally (the add/del backgrounds layer on top).
    fn from_diff(name: String, path: &Path, text: &str, rows: Vec<DiffRow>) -> Self {
        let rope = Rope::from_str(text);
        let syntax = highlight::syntax_for(path);
        let (lines, hl_states) = highlight::full(syntax, &rope);
        Self {
            lines,
            path: PathBuf::from(format!("\u{0}{name}")),
            name,
            rope,
            syntax,
            hl_states,
            dirty_from: None,
            cursor: (0, 0),
            scroll: 0,
            hscroll: 0,
            modified: false,
            last_edit: Instant::now(),
            readonly: true,
            is_diff: true,
            diff_rows: Some(rows),
            preview: false,
            preview_cache: None,
            image: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// A read-only image buffer rendered via the terminal graphics protocol. The text rope is an
    /// empty placeholder; `image` carries the renderable protocol state.
    fn from_image(path: &Path, picker: &Picker) -> Result<Self> {
        let img = image::ImageReader::open(path)
            .with_context(|| format!("opening {}", path.display()))?
            .with_guessed_format()
            .with_context(|| format!("reading {}", path.display()))?
            .decode()
            .map_err(|e| anyhow!("decoding {}: {e}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let syntax = highlight::syntax_for(path);
        Ok(Self {
            lines: vec![Line::raw("")],
            path: path.to_path_buf(),
            name,
            rope: Rope::new(),
            syntax,
            hl_states: Vec::new(),
            dirty_from: None,
            cursor: (0, 0),
            scroll: 0,
            hscroll: 0,
            modified: false,
            last_edit: Instant::now(),
            readonly: true,
            is_diff: false,
            diff_rows: None,
            preview: false,
            preview_cache: None,
            image: Some(picker.new_resize_protocol(img)),
            undo: Vec::new(),
            redo: Vec::new(),
        })
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

    /// Whether this buffer is a Markdown file (so preview is meaningful).
    pub fn is_markdown(&self) -> bool {
        matches!(
            self.path.extension().and_then(|e| e.to_str()),
            Some("md") | Some("markdown") | Some("mdown") | Some("mkd")
        )
    }

    /// Toggle the rendered-markdown preview. Returns the new state (false for non-markdown).
    pub fn toggle_preview(&mut self) -> bool {
        if self.is_markdown() {
            self.preview = !self.preview;
        }
        self.preview
    }

    /// (Re)render the markdown preview for `width` if needed, preserving already-loaded images.
    pub fn ensure_preview(&mut self, width: usize) {
        if self.preview_cache.as_ref().map(|c| c.width) == Some(width) {
            return;
        }
        let (lines, marks) = crate::markdown::render(&self.rope.to_string(), width);
        // Carry over decoded protocols for images whose source is unchanged.
        let mut old: std::collections::HashMap<String, Option<StatefulProtocol>> = self
            .preview_cache
            .take()
            .map(|c| {
                c.images
                    .into_iter()
                    .filter(|i| i.proto.is_some())
                    .map(|i| (i.source, i.proto))
                    .collect()
            })
            .unwrap_or_default();
        let images = marks
            .into_iter()
            .map(|m| {
                let proto = old.remove(&m.source).flatten();
                PreviewImage {
                    requested: proto.is_some(),
                    proto,
                    line: m.line,
                    height: m.height,
                    source: m.source,
                }
            })
            .collect();
        self.preview_cache = Some(PreviewRender {
            width,
            lines,
            images,
        });
    }

    pub fn preview_lines(&self) -> &[Line<'static>] {
        self.preview_cache.as_ref().map(|c| &c.lines[..]).unwrap_or(&[])
    }

    pub fn preview_images_mut(&mut self) -> &mut [PreviewImage] {
        self.preview_cache
            .as_mut()
            .map(|c| &mut c.images[..])
            .unwrap_or(&mut [])
    }

    /// The directory the buffer's file lives in (for resolving relative image paths).
    pub fn dir(&self) -> PathBuf {
        self.path.parent().map(|p| p.to_path_buf()).unwrap_or_default()
    }

    /// Mark the buffer modified and record the earliest line that changed (for incremental
    /// re-highlighting).
    fn mark_dirty(&mut self, line: usize) {
        self.modified = true;
        self.last_edit = Instant::now();
        self.dirty_from = Some(self.dirty_from.map_or(line, |d| d.min(line)));
        self.preview_cache = None;
    }

    /// Snapshot the current state onto the undo stack before a mutation (clears redo).
    fn push_undo(&mut self) {
        self.undo.push((self.rope.clone(), self.cursor));
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        if let Some((rope, cursor)) = self.undo.pop() {
            self.redo.push((self.rope.clone(), self.cursor));
            self.restore(rope, cursor);
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some((rope, cursor)) = self.redo.pop() {
            self.undo.push((self.rope.clone(), self.cursor));
            self.restore(rope, cursor);
            true
        } else {
            false
        }
    }

    fn restore(&mut self, rope: Rope, cursor: (usize, usize)) {
        self.rope = rope;
        let last = self.rope.len_lines().saturating_sub(1);
        let line = cursor.0.min(last);
        self.cursor = (line, cursor.1.min(self.line_len(line)));
        self.modified = true;
        // Undo/redo can change any line, so re-highlight the whole buffer (incremental
        // convergence from line 0 would stop early and miss the changed line).
        let (lines, states) = highlight::full(self.syntax, &self.rope);
        self.lines = lines;
        self.hl_states = states;
        self.dirty_from = None;
    }

    pub fn insert_char(&mut self, ch: char) {
        if self.readonly {
            return;
        }
        self.push_undo();
        let line = self.cursor.0;
        let idx = self.cursor_char();
        self.rope.insert_char(idx, ch);
        if ch == '\n' {
            self.cursor = (self.cursor.0 + 1, 0);
        } else {
            self.cursor.1 += 1;
        }
        self.mark_dirty(line);
    }

    pub fn insert_str(&mut self, s: &str) {
        self.push_undo();
        let line = self.cursor.0;
        let idx = self.cursor_char();
        self.rope.insert(idx, s);
        self.cursor.1 += s.chars().count();
        self.mark_dirty(line);
    }

    /// Insert possibly-multi-line text (e.g. a paste) at the cursor, advancing the cursor.
    pub fn insert_text(&mut self, s: &str) {
        self.push_undo();
        let line = self.cursor.0;
        let idx = self.cursor_char();
        self.rope.insert(idx, s);
        let newlines = s.matches('\n').count();
        if newlines == 0 {
            self.cursor.1 += s.chars().count();
        } else {
            let last = s.rsplit('\n').next().unwrap_or("");
            self.cursor = (self.cursor.0 + newlines, last.chars().count());
        }
        self.mark_dirty(line);
    }

    /// Remove the character range from `(sl, sc)` to `(el, ec)` and place the cursor at the start.
    pub fn delete_range(&mut self, sl: usize, sc: usize, el: usize, ec: usize) {
        let start = self.rope.line_to_char(sl) + sc;
        let end = (self.rope.line_to_char(el) + ec).min(self.rope.len_chars());
        if start < end {
            self.push_undo();
            self.rope.remove(start..end);
            self.cursor = (sl, sc);
            self.mark_dirty(sl);
        }
    }

    pub fn backspace(&mut self) {
        let (l, c) = self.cursor;
        if c > 0 {
            self.push_undo();
            let idx = self.cursor_char();
            self.rope.remove(idx - 1..idx);
            self.cursor.1 -= 1;
            self.mark_dirty(l);
        } else if l > 0 {
            self.push_undo();
            let prev_len = self.line_len(l - 1);
            let idx = self.rope.line_to_char(l); // start of line l == just after prev newline
            self.rope.remove(idx - 1..idx);
            self.cursor = (l - 1, prev_len);
            self.mark_dirty(l - 1);
        }
    }

    pub fn delete(&mut self) {
        let (l, c) = self.cursor;
        let idx = self.cursor_char();
        if c < self.line_len(l) {
            self.push_undo();
            self.rope.remove(idx..idx + 1);
            self.mark_dirty(l);
        } else if l + 1 < self.rope.len_lines() {
            // At end of line: remove the newline to join the next line.
            self.push_undo();
            self.rope.remove(idx..idx + 1);
            self.mark_dirty(l);
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
        self.push_undo();
        let target = self.word_left_col(self.cursor.0, self.cursor.1);
        let line_start = self.rope.line_to_char(self.cursor.0);
        self.rope.remove(line_start + target..line_start + self.cursor.1);
        self.cursor.1 = target;
        self.mark_dirty(self.cursor.0);
    }

    /// Place the cursor directly (used by clicks, Home/End); clamps to valid positions.
    pub fn set_cursor(&mut self, line: usize, col: usize) {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        self.cursor = (line, col.min(self.line_len(line)));
    }

    pub fn ensure_cursor_visible(&mut self, height: usize, width: usize) {
        if height > 0 {
            if self.cursor.0 < self.scroll {
                self.scroll = self.cursor.0;
            } else if self.cursor.0 >= self.scroll + height {
                self.scroll = self.cursor.0 + 1 - height;
            }
        }
        if width > 0 {
            if self.cursor.1 < self.hscroll {
                self.hscroll = self.cursor.1;
            } else if self.cursor.1 >= self.hscroll + width {
                self.hscroll = self.cursor.1 + 1 - width;
            }
        }
    }

    fn rehighlight(&mut self) {
        if let Some(from) = self.dirty_from.take() {
            highlight::incremental(
                self.syntax,
                &mut self.lines,
                &mut self.hl_states,
                &self.rope,
                from,
            );
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
    /// Last rendered content width, used to drive horizontal scrolling.
    pub viewport_w: usize,
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
    /// Terminal graphics picker (font size + protocol), used to open image files.
    pub picker: Option<Picker>,
}

impl Editor {
    /// Open a file, or switch to it if already open. Image files open as image buffers (rendered
    /// via the terminal graphics protocol); everything else opens as editable text.
    pub fn open(&mut self, path: &Path) -> Result<()> {
        self.selection = None;
        if let Some(i) = self.tabs.iter().position(|b| b.path == path) {
            self.active = i;
            return Ok(());
        }
        let buf = if is_image_path(path) {
            let picker = self
                .picker
                .as_ref()
                .ok_or_else(|| anyhow!("image rendering is unavailable"))?;
            Buffer::from_image(path, picker)?
        } else {
            Buffer::from_path(path)?
        };
        self.tabs.push(buf);
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    /// Decode any not-yet-loaded images in the active markdown preview. Local files are decoded
    /// immediately; remote URLs are fetched on a background thread that posts `ImageLoaded`.
    pub fn load_preview_images(&mut self, tx: &UnboundedSender<AppEvent>) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let idx = self.active;
        let Some(buf) = self.tabs.get_mut(idx) else {
            return;
        };
        if !(buf.preview && buf.is_markdown()) {
            return;
        }
        let (dir, bpath) = (buf.dir(), buf.path.clone());
        for img in buf.preview_images_mut() {
            if img.requested {
                continue;
            }
            img.requested = true;
            let src = img.source.clone();
            if src.starts_with("http://") || src.starts_with("https://") {
                let (tx, bp) = (tx.clone(), bpath.clone());
                std::thread::spawn(move || {
                    if let Some(data) = fetch_url(&src) {
                        let _ = tx.send(AppEvent::ImageLoaded {
                            buffer: bp,
                            source: src,
                            data,
                        });
                    }
                });
            } else if let Some(im) = decode_image_file(&dir.join(&src)) {
                img.proto = Some(picker.new_resize_protocol(im));
            }
        }
    }

    /// Attach a downloaded remote image (raw bytes) to its preview slot.
    pub fn set_loaded_image(&mut self, buffer: &Path, source: &str, data: &[u8]) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let Some(buf) = self.tabs.iter_mut().find(|b| b.path == buffer) else {
            return;
        };
        let Some(im) = decode_image_bytes(data) else {
            return;
        };
        let proto = picker.new_resize_protocol(im);
        if let Some(slot) = buf.preview_images_mut().iter_mut().find(|i| i.source == source) {
            slot.proto = Some(proto);
        }
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

    /// Open (or replace) a read-only full-file inline diff tab. `path` is the real file path
    /// (drives syntax highlighting); `name` is the tab label.
    pub fn open_diff_view(&mut self, name: String, path: &Path, text: &str, rows: Vec<DiffRow>) {
        self.selection = None;
        let buf = Buffer::from_diff(name.clone(), path, text, rows);
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
