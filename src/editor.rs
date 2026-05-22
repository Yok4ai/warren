//! The editor: a set of open files shown as tabs. Phase 2 is read-only — files are read
//! from disk, syntax-highlighted, and scrolled. Editing lands in Phase 3.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ratatui::text::Line;

use crate::highlight;

/// One open file.
pub struct Buffer {
    pub path: PathBuf,
    pub name: String,
    /// Pre-highlighted lines for rendering.
    pub lines: Vec<Line<'static>>,
    /// Top line index currently scrolled to.
    pub scroll: usize,
}

impl Buffer {
    fn from_path(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Ok(Self {
            path: path.to_path_buf(),
            name,
            lines: highlight::highlight(path, &text),
            scroll: 0,
        })
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

/// The collection of open tabs.
#[derive(Default)]
pub struct Editor {
    pub tabs: Vec<Buffer>,
    pub active: usize,
    /// Last rendered content height, used to clamp scrolling.
    pub viewport: usize,
}

impl Editor {
    /// Open a file, or switch to it if already open. Returns an error message on failure.
    pub fn open(&mut self, path: &Path) -> Result<()> {
        if let Some(i) = self.tabs.iter().position(|b| b.path == path) {
            self.active = i;
            return Ok(());
        }
        let buf = Buffer::from_path(path)?;
        self.tabs.push(buf);
        self.active = self.tabs.len() - 1;
        Ok(())
    }

    pub fn active_buffer(&self) -> Option<&Buffer> {
        self.tabs.get(self.active)
    }

    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
    }

    pub fn close_active(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        self.tabs.remove(self.active);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
    }

    /// Reload an open file from disk (used when the watcher reports it changed).
    pub fn reload(&mut self, path: &Path) {
        if let Some(i) = self.tabs.iter().position(|b| b.path == path) {
            if let Ok(mut fresh) = Buffer::from_path(path) {
                fresh.scroll = self.tabs[i].scroll.min(fresh.line_count().saturating_sub(1));
                self.tabs[i] = fresh;
            }
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

    pub fn scroll_home(&mut self) {
        if let Some(b) = self.tabs.get_mut(self.active) {
            b.scroll = 0;
        }
    }

    pub fn scroll_end(&mut self) {
        let max = self.max_scroll();
        if let Some(b) = self.tabs.get_mut(self.active) {
            b.scroll = max;
        }
    }
}
