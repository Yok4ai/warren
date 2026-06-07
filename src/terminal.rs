//! A terminal pane: a child process (e.g. `claude`) running in a pseudo-terminal, its output
//! parsed by `alacritty_terminal` (Alacritty's own VT engine) and blitted to ratatui by
//! `render_term`. Keyboard/mouse/paste are translated to the byte sequences a real terminal would
//! send. The reader thread parses bytes into the shared `Term` and pushes redraw/exit events into
//! the app funnel. A separate writer thread serializes all PTY writes (app input + the emulator's
//! own replies), so the parse thread never blocks on a lock the app holds.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use tokio::sync::mpsc::UnboundedSender;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Processor};

use crate::event::AppEvent;
use crate::theme::current as theme;

/// Lines of scrollback to retain per terminal.
const SCROLLBACK: usize = 5000;

/// Bridges the emulator to warren: replies the emulator must send back to the child (DA/DSR/cursor
/// reports, etc., via [`Event::PtyWrite`]) are pushed to the writer thread; everything else just
/// nudges a redraw. Runs on the parse thread, so it only ever *sends* on channels — never locks.
#[derive(Clone)]
pub(crate) struct WarrenListener {
    pty_write: mpsc::Sender<Vec<u8>>,
    redraw: UnboundedSender<AppEvent>,
}

impl EventListener for WarrenListener {
    fn send_event(&self, event: Event) {
        // Apps block waiting for these replies (e.g. Device Attributes on startup), so they must
        // reach the PTY. Title/Bell/Clipboard/ColorRequest are ignored: clipboard is handled
        // app-side via OSC52, the title isn't shown, and the bell is silent.
        if let Event::PtyWrite(s) = event {
            let _ = self.pty_write.send(s.into_bytes());
        }
        let _ = self.redraw.send(AppEvent::PtyChanged);
    }
}

/// Pane dimensions handed to alacritty for construction/resize (it reads columns + screen lines;
/// scrollback depth comes from `Config`, so `total_lines` just mirrors the visible rows).
struct PaneDims {
    cols: usize,
    rows: usize,
}

impl Dimensions for PaneDims {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct TerminalPane {
    /// Tab label (e.g. "claude", "fish").
    pub name: String,
    term: Arc<Mutex<Term<WarrenListener>>>,
    /// All PTY writes funnel through here to the writer thread (keeps a single writer).
    pty_write: mpsc::Sender<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
    rows: u16,
    cols: u16,
    /// Set once the child exits (reader hit EOF).
    exited: Arc<AtomicBool>,
}

impl TerminalPane {
    /// Spawn `command` in a PTY of the given size, inheriting the environment and running in `cwd`.
    pub fn spawn(
        command: &str,
        name: &str,
        cwd: &Path,
        rows: u16,
        cols: u16,
        tx: UnboundedSender<AppEvent>,
        extra_env: &[(String, String)],
    ) -> Result<Self> {
        let (rows, cols) = (rows.max(1), cols.max(1));
        let pair = native_pty_system().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(command);
        cmd.cwd(cwd);
        for (k, v) in std::env::vars() {
            cmd.env(k, v);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }

        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        // Writer thread: owns the PTY writer and drains a channel fed by both app input and the
        // emulator's own replies, so writes are serialized and the parse thread never blocks here.
        let (pty_write, pty_rx) = mpsc::channel::<Vec<u8>>();
        let mut writer = pair.master.take_writer()?;
        std::thread::spawn(move || {
            while let Ok(bytes) = pty_rx.recv() {
                if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        });

        let config = Config {
            scrolling_history: SCROLLBACK,
            ..Default::default()
        };
        let listener = WarrenListener {
            pty_write: pty_write.clone(),
            redraw: tx.clone(),
        };
        let term = Arc::new(Mutex::new(Term::new(
            config,
            &PaneDims {
                cols: cols as usize,
                rows: rows as usize,
            },
            listener,
        )));

        let mut reader = pair.master.try_clone_reader()?;
        let exited = Arc::new(AtomicBool::new(false));

        {
            let term = term.clone();
            let exited = exited.clone();
            std::thread::spawn(move || {
                // The VT parser is single-threaded state local to this thread; only `Term` is shared.
                let mut parser: Processor = Processor::new();
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            parser.advance(&mut *term.lock().unwrap(), &buf[..n]);
                            let _ = tx.send(AppEvent::PtyChanged);
                        }
                    }
                }
                exited.store(true, Ordering::Relaxed);
                let _ = tx.send(AppEvent::PtyExited);
            });
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        Ok(Self {
            name: name.to_string(),
            term,
            pty_write,
            master: pair.master,
            rows,
            cols,
            exited,
        })
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    /// Run `f` with the shared `Term` locked — used by the renderer (`render_term`). Keeps the
    /// concrete event-listener type private to this module.
    pub(crate) fn with_term<R>(&self, f: impl FnOnce(&Term<WarrenListener>) -> R) -> R {
        f(&self.term.lock().unwrap())
    }

    /// Resize the PTY (and emulator) to match the rendered area, if changed.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        // Note: TermSize is column-first (the opposite of warren's (rows, cols) convention).
        self.term.lock().unwrap().resize(PaneDims {
            cols: cols as usize,
            rows: rows as usize,
        });
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    fn write(&self, bytes: &[u8]) {
        let _ = self.pty_write.send(bytes.to_vec());
    }

    pub fn send_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // In application-cursor-keys mode (DECCKM, set by ncurses apps like ranger/vim) the arrow
        // and Home/End keys must be sent as SS3 (ESC O x) rather than CSI (ESC [ x).
        let app_cursor = self.term.lock().unwrap().mode().contains(TermMode::APP_CURSOR);
        if let Some(bytes) = key_to_bytes(code, mods, app_cursor) {
            self.write(&bytes);
        }
    }

    pub fn send_paste(&mut self, text: &str) {
        self.write(b"\x1b[200~");
        self.write(text.as_bytes());
        self.write(b"\x1b[201~");
    }

    /// True if the app is on the alternate screen (vim/htop/less). Those manage their own
    /// scrolling, so the wheel goes to them; normal-screen apps (claude, shells) scroll our
    /// scrollback instead.
    pub fn in_alt_screen(&self) -> bool {
        self.term.lock().unwrap().mode().contains(TermMode::ALT_SCREEN)
    }

    /// True if the app has requested mouse reporting (claude); a plain click is only forwarded
    /// in that case, so clicks in a mouse-unaware shell don't inject escape bytes.
    pub fn wants_mouse(&self) -> bool {
        self.term.lock().unwrap().mode().intersects(TermMode::MOUSE_MODE)
    }

    /// Text of the selection between two visible `(row, col)` cells (inclusive of both), read
    /// scrollback-aware: a viewport row maps to grid line `row - display_offset`.
    pub fn selection_text(&self, anchor: (u16, u16), cursor: (u16, u16)) -> String {
        let (a, b) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        let t = self.term.lock().unwrap();
        let off = t.grid().display_offset() as i32;
        let start = Point::new(Line(a.0 as i32 - off), Column(a.1 as usize));
        let end = Point::new(Line(b.0 as i32 - off), Column(b.1 as usize));
        t.bounds_to_string(start, end)
    }

    /// Scroll the scrollback by `delta` lines (positive = back into history). alacritty clamps.
    pub fn scroll(&mut self, delta: i32) {
        self.term.lock().unwrap().scroll_display(Scroll::Delta(delta));
    }

    /// Jump back to live output (bottom of the scrollback).
    pub fn scroll_to_bottom(&mut self) {
        self.term.lock().unwrap().scroll_display(Scroll::Bottom);
    }

    /// `(current offset, max offset)` lines above the live view.
    pub fn scrollback_state(&self) -> (usize, usize) {
        let t = self.term.lock().unwrap();
        (t.grid().display_offset(), t.grid().history_size())
    }

    /// Set the scrollback position to exactly `offset` lines above the live view (clamped).
    pub fn set_scrollback_offset(&mut self, offset: usize) {
        let mut t = self.term.lock().unwrap();
        let max = t.grid().history_size();
        let cur = t.grid().display_offset() as i32;
        let delta = offset.min(max) as i32 - cur;
        if delta != 0 {
            t.scroll_display(Scroll::Delta(delta));
        }
    }

    /// Forward a mouse event with coordinates already made relative to the pane (1-based).
    pub fn send_mouse(&mut self, kind: MouseEventKind, col: u16, row: u16) {
        if let Some(bytes) = mouse_to_bytes(kind, col, row) {
            self.write(&bytes);
        }
    }
}

/// Blit the terminal's visible grid into `area` of `buf`. `focused`/`blink` gate the cursor overlay
/// (matches the old block-cursor behavior: shown only when the panel is focused and blinking on).
pub fn render_term<T: EventListener>(
    term: &Term<T>,
    area: Rect,
    buf: &mut Buffer,
    focused: bool,
    blink: bool,
) {
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    for indexed in content.display_iter {
        // Skip the second half of wide glyphs: ratatui reserves the next column when we write a
        // 2-wide symbol, so painting the spacer would shear the line.
        if indexed
            .cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        // Viewport row: display_iter's top line is Line(-display_offset).
        let row = indexed.point.line.0 + offset;
        let col = indexed.point.column.0 as i32;
        if row < 0 || col < 0 {
            continue;
        }
        let (x, y) = (area.x + col as u16, area.y + row as u16);
        if x >= area.right() || y >= area.bottom() {
            continue;
        }
        if let Some(cell) = buf.cell_mut((x, y)) {
            let c = indexed.cell;
            let ch = if c.flags.contains(Flags::HIDDEN) { ' ' } else { c.c };
            cell.set_symbol(&ch.to_string());
            cell.set_style(style_from(c.fg, c.bg, c.flags));
        }
    }

    // Cursor overlay last so it wins, only at the live view (alacritty hides it when scrolled back).
    if focused && blink && content.display_offset == 0 && content.cursor.shape != CursorShape::Hidden
    {
        let row = content.cursor.point.line.0 + offset;
        let col = content.cursor.point.column.0 as i32;
        if row >= 0 && col >= 0 {
            let (x, y) = (area.x + col as u16, area.y + row as u16);
            if x < area.right() && y < area.bottom() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(Style::default().bg(theme().accent).fg(Color::Black));
                }
            }
        }
    }
}

/// Build a ratatui style from an alacritty cell's colors and flags.
fn style_from(fg: AnsiColor, bg: AnsiColor, flags: Flags) -> Style {
    let (mut f, mut b) = (color_to_ratatui(fg, true), color_to_ratatui(bg, false));
    if flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut f, &mut b);
    }
    let mut style = Style::default().fg(f);
    // Leave the default background as Reset so a transparent terminal shows through (CLAUDE.md:
    // don't paint a solid bg). Only set an explicit, non-default bg.
    if b != Color::Reset {
        style = style.bg(b);
    }
    let mut m = Modifier::empty();
    if flags.contains(Flags::BOLD) {
        m |= Modifier::BOLD;
    }
    if flags.contains(Flags::ITALIC) {
        m |= Modifier::ITALIC;
    }
    if flags.intersects(Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE) {
        m |= Modifier::UNDERLINED;
    }
    if flags.contains(Flags::DIM) {
        m |= Modifier::DIM;
    }
    if flags.contains(Flags::STRIKEOUT) {
        m |= Modifier::CROSSED_OUT;
    }
    style.add_modifier(m)
}

/// Map an alacritty color to ratatui. Named/indexed ANSI use ratatui's named ANSI variants so the
/// host terminal renders them with its own palette. `is_fg` picks the default foreground/background.
fn color_to_ratatui(c: AnsiColor, is_fg: bool) -> Color {
    match c {
        AnsiColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => ansi_index(i),
        AnsiColor::Named(n) => match n {
            NamedColor::Foreground | NamedColor::BrightForeground => theme().fg,
            NamedColor::Background => Color::Reset,
            NamedColor::Cursor => theme().accent,
            NamedColor::DimForeground => theme().dim,
            // The remaining named colors are the 16 ANSI (incl. Dim* which map to their base).
            other => ansi_index(named_ansi_index(other, is_fg)),
        },
    }
}

/// ANSI/256 palette index → ratatui color. 0..=15 use named variants (host palette); 16..=255 pass
/// through as a 256-color index.
fn ansi_index(i: u8) -> Color {
    match i {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        15 => Color::White,
        n => Color::Indexed(n),
    }
}

/// The 0..=15 palette index for a named ANSI color (Dim* fold onto their base color; the DIM
/// attribute is applied separately via the cell flags). Falls back to the default fg/bg index.
fn named_ansi_index(n: NamedColor, is_fg: bool) -> u8 {
    match n {
        NamedColor::Black | NamedColor::DimBlack => 0,
        NamedColor::Red | NamedColor::DimRed => 1,
        NamedColor::Green | NamedColor::DimGreen => 2,
        NamedColor::Yellow | NamedColor::DimYellow => 3,
        NamedColor::Blue | NamedColor::DimBlue => 4,
        NamedColor::Magenta | NamedColor::DimMagenta => 5,
        NamedColor::Cyan | NamedColor::DimCyan => 6,
        NamedColor::White | NamedColor::DimWhite => 7,
        NamedColor::BrightBlack => 8,
        NamedColor::BrightRed => 9,
        NamedColor::BrightGreen => 10,
        NamedColor::BrightYellow => 11,
        NamedColor::BrightBlue => 12,
        NamedColor::BrightMagenta => 13,
        NamedColor::BrightCyan => 14,
        NamedColor::BrightWhite => 15,
        _ => {
            if is_fg {
                7
            } else {
                0
            }
        }
    }
}

/// A VS Code-style terminal panel holding multiple terminals with a tab bar.
#[derive(Default)]
pub struct Panel {
    pub terms: Vec<TerminalPane>,
    pub active: usize,
    pub visible: bool,
    // Per-frame geometry for mouse mapping (set by the renderer).
    pub content_area: Rect,
    /// The vertical tab strip's clickable area (one row per terminal, then a "+ new" row).
    pub tablist_area: Rect,
}

impl Panel {
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Add a terminal, make it active, and show the panel.
    pub fn add(&mut self, pane: TerminalPane) {
        self.terms.push(pane);
        self.active = self.terms.len() - 1;
        self.visible = true;
    }

    pub fn active(&self) -> Option<&TerminalPane> {
        self.terms.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut TerminalPane> {
        self.terms.get_mut(self.active)
    }

    pub fn close(&mut self, idx: usize) {
        if idx >= self.terms.len() {
            return;
        }
        self.terms.remove(idx);
        if self.active > idx || self.active >= self.terms.len() {
            self.active = self.active.saturating_sub(1);
        }
        if self.terms.is_empty() {
            self.visible = false;
        }
    }

    pub fn next(&mut self) {
        if !self.terms.is_empty() {
            self.active = (self.active + 1) % self.terms.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.terms.is_empty() {
            self.active = (self.active + self.terms.len() - 1) % self.terms.len();
        }
    }

    /// Remove any terminals whose child has exited. Returns true if anything changed.
    pub fn prune_exited(&mut self) -> bool {
        let before = self.terms.len();
        let mut i = 0;
        while i < self.terms.len() {
            if self.terms[i].has_exited() {
                self.close(i);
            } else {
                i += 1;
            }
        }
        self.terms.len() != before
    }
}

fn xterm_mod(mods: KeyModifiers) -> Option<u8> {
    let mut code = 0u8;
    if mods.contains(KeyModifiers::SHIFT) {
        code += 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        code += 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        code += 4;
    }
    (code != 0).then_some(code + 1)
}

fn csi_letter(mods: KeyModifiers, letter: char) -> Vec<u8> {
    match xterm_mod(mods) {
        Some(m) => format!("\x1b[1;{m}{letter}").into_bytes(),
        None => format!("\x1b[{letter}").into_bytes(),
    }
}

fn csi_tilde(mods: KeyModifiers, num: u8) -> Vec<u8> {
    match xterm_mod(mods) {
        Some(m) => format!("\x1b[{num};{m}~").into_bytes(),
        None => format!("\x1b[{num}~").into_bytes(),
    }
}

/// A cursor/Home/End key. In application-cursor-keys mode an unmodified key uses SS3 (ESC O x);
/// otherwise (or when modified) it uses the CSI form so xterm-style modifier encoding still works.
fn cursor_key(mods: KeyModifiers, letter: char, app_cursor: bool) -> Vec<u8> {
    if app_cursor && mods.is_empty() {
        format!("\x1bO{letter}").into_bytes()
    } else {
        csi_letter(mods, letter)
    }
}

/// Translate a key event into the bytes a terminal app expects (matching xterm).
fn key_to_bytes(code: KeyCode, mods: KeyModifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let alt = mods.contains(KeyModifiers::ALT);
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let mut out: Vec<u8> = Vec::new();
    match code {
        KeyCode::Char(c) => {
            if ctrl {
                let b = match c.to_ascii_lowercase() {
                    'a'..='z' => (c.to_ascii_lowercase() as u8) - b'a' + 1,
                    ' ' | '@' => 0,
                    '[' => 27,
                    '\\' => 28,
                    ']' => 29,
                    '^' => 30,
                    '_' => 31,
                    _ => return None,
                };
                if alt {
                    out.push(0x1b);
                }
                out.push(b);
            } else {
                if alt {
                    out.push(0x1b);
                }
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        KeyCode::Enter => {
            // Alt+Enter / Ctrl+Enter -> ESC CR (apps like claude treat this as "insert newline").
            if alt || ctrl {
                out.push(0x1b);
            }
            out.push(b'\r');
        }
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => {
            if alt {
                out.push(0x1b);
            }
            out.push(if ctrl { 0x08 } else { 0x7f });
        }
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Up => out.extend_from_slice(&cursor_key(mods, 'A', app_cursor)),
        KeyCode::Down => out.extend_from_slice(&cursor_key(mods, 'B', app_cursor)),
        KeyCode::Right => out.extend_from_slice(&cursor_key(mods, 'C', app_cursor)),
        KeyCode::Left => out.extend_from_slice(&cursor_key(mods, 'D', app_cursor)),
        KeyCode::Home => out.extend_from_slice(&cursor_key(mods, 'H', app_cursor)),
        KeyCode::End => out.extend_from_slice(&cursor_key(mods, 'F', app_cursor)),
        KeyCode::PageUp => out.extend_from_slice(&csi_tilde(mods, 5)),
        KeyCode::PageDown => out.extend_from_slice(&csi_tilde(mods, 6)),
        KeyCode::Delete => out.extend_from_slice(&csi_tilde(mods, 3)),
        KeyCode::Insert => out.extend_from_slice(&csi_tilde(mods, 2)),
        _ => return None,
    }
    Some(out)
}

fn mouse_to_bytes(kind: MouseEventKind, col: u16, row: u16) -> Option<Vec<u8>> {
    let button = |b: MouseButton| match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (cb, press) = match kind {
        MouseEventKind::Down(b) => (button(b), true),
        MouseEventKind::Up(b) => (button(b), false),
        MouseEventKind::Drag(b) => (button(b) + 32, true),
        MouseEventKind::ScrollUp => (64, true),
        MouseEventKind::ScrollDown => (65, true),
        MouseEventKind::ScrollLeft => (66, true),
        MouseEventKind::ScrollRight => (67, true),
        MouseEventKind::Moved => return None,
    };
    let end = if press { 'M' } else { 'm' };
    Some(format!("\x1b[<{cb};{col};{row}{end}").into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_passthrough() {
        let s = style_from(
            AnsiColor::Spec(alacritty_terminal::vte::ansi::Rgb { r: 10, g: 20, b: 30 }),
            AnsiColor::Named(NamedColor::Background),
            Flags::empty(),
        );
        assert_eq!(s.fg, Some(Color::Rgb(10, 20, 30)));
        // Default background stays unset (Reset) so transparency shows through.
        assert_eq!(s.bg, None);
    }

    #[test]
    fn ansi_named_and_256() {
        assert_eq!(ansi_index(1), Color::Red);
        assert_eq!(ansi_index(9), Color::LightRed);
        assert_eq!(ansi_index(200), Color::Indexed(200));
    }

    #[test]
    fn flags_to_modifiers() {
        let s = style_from(
            AnsiColor::Named(NamedColor::Red),
            AnsiColor::Named(NamedColor::Background),
            Flags::BOLD | Flags::UNDERLINE,
        );
        assert!(s.add_modifier.contains(Modifier::BOLD));
        assert!(s.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn inverse_swaps_colors() {
        // Inverse with a real bg color: fg/bg should swap, so bg becomes set and fg the old bg.
        let s = style_from(
            AnsiColor::Named(NamedColor::Red),
            AnsiColor::Named(NamedColor::Blue),
            Flags::INVERSE,
        );
        assert_eq!(s.fg, Some(Color::Blue));
        assert_eq!(s.bg, Some(Color::Red));
    }
}
