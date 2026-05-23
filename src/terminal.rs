//! A terminal pane: a child process (e.g. `claude`) running in a pseudo-terminal, its output
//! parsed by `vt100` for rendering with `tui-term`. Keyboard/mouse/paste are translated to the
//! byte sequences a real terminal would send. The reader thread pushes redraw/exit events into
//! the app funnel. (Distilled from the Phase 0 proof.)

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use ratatui::layout::Rect;
use tokio::sync::mpsc::UnboundedSender;

use crate::event::AppEvent;

pub struct TerminalPane {
    /// Tab label (e.g. "claude", "fish").
    pub name: String,
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
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
    ) -> Result<Self> {
        let pair = native_pty_system().openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
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

        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows.max(1), cols.max(1), 0)));
        let writer = pair.master.take_writer()?;
        let mut reader = pair.master.try_clone_reader()?;
        let exited = Arc::new(AtomicBool::new(false));

        {
            let parser = parser.clone();
            let exited = exited.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            parser.lock().unwrap().process(&buf[..n]);
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
            parser,
            writer,
            master: pair.master,
            rows: rows.max(1),
            cols: cols.max(1),
            exited,
        })
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }

    /// Lock the parser to read its screen for rendering.
    pub fn lock(&self) -> MutexGuard<'_, vt100::Parser> {
        self.parser.lock().unwrap()
    }

    /// Resize the PTY (and parser) to match the rendered area, if changed.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        self.parser.lock().unwrap().set_size(rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn send_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if let Some(bytes) = key_to_bytes(code, mods) {
            self.write(&bytes);
        }
    }

    pub fn send_paste(&mut self, text: &str) {
        self.write(b"\x1b[200~");
        self.write(text.as_bytes());
        self.write(b"\x1b[201~");
    }

    /// Forward a mouse event with coordinates already made relative to the pane (1-based).
    pub fn send_mouse(&mut self, kind: MouseEventKind, col: u16, row: u16) {
        if let Some(bytes) = mouse_to_bytes(kind, col, row) {
            self.write(&bytes);
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

/// Translate a key event into the bytes a terminal app expects (matching xterm).
fn key_to_bytes(code: KeyCode, mods: KeyModifiers) -> Option<Vec<u8>> {
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
        KeyCode::Up => out.extend_from_slice(&csi_letter(mods, 'A')),
        KeyCode::Down => out.extend_from_slice(&csi_letter(mods, 'B')),
        KeyCode::Right => out.extend_from_slice(&csi_letter(mods, 'C')),
        KeyCode::Left => out.extend_from_slice(&csi_letter(mods, 'D')),
        KeyCode::Home => out.extend_from_slice(&csi_letter(mods, 'H')),
        KeyCode::End => out.extend_from_slice(&csi_letter(mods, 'F')),
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
