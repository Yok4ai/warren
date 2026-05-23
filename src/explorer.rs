//! The file-explorer model: a lazily-expanded directory tree flattened into a visible list
//! for rendering and selection. Expansion state is a set of paths, so refreshing after a
//! filesystem change is just a re-walk that preserves what the user had open.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// One row in the flattened, currently-visible tree.
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
}

pub struct FileTree {
    pub root: PathBuf,
    expanded: HashSet<PathBuf>,
    pub rows: Vec<Row>,
    pub selected: usize,
    /// Index of the first visible row; we own this (rather than a List widget) so mouse clicks
    /// can be mapped back to rows.
    pub scroll: usize,
    /// Last rendered height, so keyboard navigation can keep the selection visible.
    pub viewport: usize,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root,
            expanded: HashSet::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            viewport: 0,
        };
        tree.rebuild();
        tree
    }

    /// Expand the ancestor directories of `path` and select its row, so an open file is
    /// highlighted in the tree. No-op if `path` isn't under the root (e.g. virtual buffers).
    pub fn reveal(&mut self, path: &Path) {
        if path.strip_prefix(&self.root).is_err() {
            return;
        }
        let mut cur = path.parent();
        while let Some(d) = cur {
            if d == self.root {
                break;
            }
            self.expanded.insert(d.to_path_buf());
            cur = d.parent();
        }
        self.rebuild();
        if let Some(i) = self.rows.iter().position(|r| r.path == path) {
            self.selected = i;
        }
    }

    /// Adjust `scroll` so the selected row is visible within `height` rows.
    pub fn ensure_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
        let max = self.rows.len().saturating_sub(height);
        self.scroll = self.scroll.min(max);
    }

    /// Re-walk the filesystem into `rows`, descending into expanded directories only.
    pub fn rebuild(&mut self) {
        let mut rows = Vec::new();
        walk(&self.root, 0, &self.expanded, &mut rows);
        self.rows = rows;
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
    }

    /// Toggle expansion of the selected directory. Returns the file path if a file was
    /// activated instead (so the caller can open it).
    pub fn activate(&mut self) -> Option<PathBuf> {
        let row = self.rows.get(self.selected)?;
        if row.is_dir {
            let path = row.path.clone();
            if !self.expanded.remove(&path) {
                self.expanded.insert(path);
            }
            self.rebuild();
            None
        } else {
            Some(row.path.clone())
        }
    }

    pub fn expand(&mut self) {
        if let Some(row) = self.rows.get(self.selected) {
            if row.is_dir && !self.expanded.contains(&row.path) {
                self.expanded.insert(row.path.clone());
                self.rebuild();
            }
        }
    }

    /// Collapse the selected directory, or jump to the parent directory row.
    pub fn collapse(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if row.is_dir && self.expanded.contains(&row.path) {
            self.expanded.remove(&row.path);
            self.rebuild();
        } else if let Some(parent) = row.path.parent().map(Path::to_path_buf) {
            if let Some(i) = self.rows.iter().position(|r| r.path == parent) {
                self.selected = i;
            }
        }
    }
}

fn walk(dir: &Path, depth: usize, expanded: &HashSet<PathBuf>, out: &mut Vec<Row>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(PathBuf, String, bool)> = read
        .filter_map(|e| e.ok())
        .map(|e| {
            let path = e.path();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name = e.file_name().to_string_lossy().into_owned();
            (path, name, is_dir)
        })
        .collect();
    // Directories first, then case-insensitive alphabetical.
    entries.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });

    for (path, name, is_dir) in entries {
        let is_expanded = is_dir && expanded.contains(&path);
        out.push(Row {
            path: path.clone(),
            name,
            is_dir,
            depth,
            expanded: is_expanded,
        });
        if is_expanded {
            walk(&path, depth + 1, expanded, out);
        }
    }
}
