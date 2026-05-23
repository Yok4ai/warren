//! Git integration via `git2` (libgit2). Local operations only — status, log, diffs, staging,
//! and committing. Network operations (push/pull) need TLS and are deferred.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use git2::{DiffFormat, Oid, Repository, Sort, Status, StatusOptions};

/// A changed file in the working tree / index.
pub struct Change {
    pub path: String,
    /// Single-letter status: M/A/D/R/U(ntracked)/C(onflict).
    pub code: char,
    pub staged: bool,
}

/// A commit for the graph view.
pub struct Commit {
    pub id: Oid,
    pub short: String,
    pub summary: String,
    pub author: String,
    pub when: String,
}

pub struct Git {
    repo: Repository,
}

impl Git {
    /// Open the repository containing `root`, if any.
    pub fn open(root: &Path) -> Option<Self> {
        Repository::discover(root).ok().map(|repo| Self { repo })
    }

    pub fn branch(&self) -> String {
        self.repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().map(str::to_string))
            .unwrap_or_else(|| "HEAD".into())
    }

    /// Working-tree + index changes.
    pub fn changes(&self) -> Vec<Change> {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);
        let Ok(statuses) = self.repo.statuses(Some(&mut opts)) else {
            return Vec::new();
        };
        statuses
            .iter()
            .filter_map(|e| {
                let path = e.path()?.to_string();
                let st = e.status();
                if st.is_ignored() {
                    return None;
                }
                let staged = st.intersects(
                    Status::INDEX_NEW
                        | Status::INDEX_MODIFIED
                        | Status::INDEX_DELETED
                        | Status::INDEX_RENAMED
                        | Status::INDEX_TYPECHANGE,
                );
                let code = if st.is_conflicted() {
                    'C'
                } else if st.is_wt_new() {
                    'U'
                } else if st.is_index_new() {
                    'A'
                } else if st.is_wt_deleted() || st.is_index_deleted() {
                    'D'
                } else if st.is_wt_renamed() || st.is_index_renamed() {
                    'R'
                } else {
                    'M'
                };
                Some(Change { path, code, staged })
            })
            .collect()
    }

    /// Most recent commits, newest first.
    pub fn log(&self, limit: usize) -> Vec<Commit> {
        let mut out = Vec::new();
        let Ok(mut walk) = self.repo.revwalk() else {
            return out;
        };
        if walk.push_head().is_err() {
            return out;
        }
        let _ = walk.set_sorting(Sort::TIME);
        for oid in walk.flatten().take(limit) {
            let Ok(commit) = self.repo.find_commit(oid) else {
                continue;
            };
            out.push(Commit {
                id: oid,
                short: oid.to_string()[..7].to_string(),
                summary: commit.summary().unwrap_or("").to_string(),
                author: commit.author().name().unwrap_or("?").to_string(),
                when: rel_time(commit.time().seconds()),
            });
        }
        out
    }

    /// Contents of `path` in the HEAD tree (empty if absent, e.g. a newly added file).
    pub fn head_file(&self, path: &str) -> String {
        self.repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_tree().ok())
            .and_then(|tree| tree.get_path(Path::new(path)).ok())
            .and_then(|entry| self.repo.find_blob(entry.id()).ok())
            .map(|blob| String::from_utf8_lossy(blob.content()).into_owned())
            .unwrap_or_default()
    }

    /// `(old, new)` contents of `path` for a commit: the parent's version vs the commit's version.
    pub fn commit_file_versions(&self, oid: Oid, path: &str) -> (String, String) {
        let blob_at = |tree: Option<git2::Tree>| -> String {
            tree.and_then(|t| t.get_path(Path::new(path)).ok())
                .and_then(|e| self.repo.find_blob(e.id()).ok())
                .map(|b| String::from_utf8_lossy(b.content()).into_owned())
                .unwrap_or_default()
        };
        let Ok(commit) = self.repo.find_commit(oid) else {
            return (String::new(), String::new());
        };
        let new = blob_at(commit.tree().ok());
        let old = blob_at(commit.parent(0).ok().and_then(|p| p.tree().ok()));
        (old, new)
    }

    /// Header (hash/author/date) plus the full message and patch for a commit (like `git show`).
    pub fn commit_details(&self, oid: Oid) -> String {
        let Ok(commit) = self.repo.find_commit(oid) else {
            return String::new();
        };
        let author = commit.author();
        let mut out = format!(
            "commit {}\nAuthor: {} <{}>\nDate:   {}\n\n    {}\n\n",
            oid,
            author.name().unwrap_or("?"),
            author.email().unwrap_or("?"),
            rel_time(commit.time().seconds()),
            commit.message().unwrap_or("").trim_end().replace('\n', "\n    "),
        );
        let new_tree = commit.tree().ok();
        let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
        if let Ok(diff) =
            self.repo
                .diff_tree_to_tree(parent_tree.as_ref(), new_tree.as_ref(), None)
        {
            out.push_str(&diff_to_string(&diff));
        }
        out
    }

    /// Files changed in a commit (vs its first parent), as (path, status-letter).
    pub fn commit_files(&self, oid: Oid) -> Vec<(String, char)> {
        let Ok(commit) = self.repo.find_commit(oid) else {
            return Vec::new();
        };
        let tree = commit.tree().ok();
        let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
        let Ok(diff) =
            self.repo
                .diff_tree_to_tree(parent_tree.as_ref(), tree.as_ref(), None)
        else {
            return Vec::new();
        };
        diff.deltas()
            .map(|d| {
                let path = d
                    .new_file()
                    .path()
                    .or_else(|| d.old_file().path())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let code = match d.status() {
                    git2::Delta::Added => 'A',
                    git2::Delta::Deleted => 'D',
                    git2::Delta::Renamed => 'R',
                    git2::Delta::Copied => 'C',
                    _ => 'M',
                };
                (path, code)
            })
            .collect()
    }

    /// Stage a path (add, or record a deletion).
    pub fn stage(&self, path: &str) -> Result<()> {
        let mut index = self.repo.index()?;
        let p = Path::new(path);
        if self.repo.workdir().map(|w| w.join(p).exists()).unwrap_or(false) {
            index.add_path(p)?;
        } else {
            index.remove_path(p)?;
        }
        index.write()?;
        Ok(())
    }

    /// Unstage a path (reset it in the index to HEAD).
    pub fn unstage(&self, path: &str) -> Result<()> {
        let head = self.repo.head()?.peel_to_commit()?;
        self.repo
            .reset_default(Some(head.as_object()), [path])
            .context("unstage")?;
        Ok(())
    }

    /// Commit the staged changes with `message`.
    pub fn commit(&self, message: &str) -> Result<()> {
        let sig = self
            .repo
            .signature()
            .context("no git user.name/email configured")?;
        let mut index = self.repo.index()?;
        let tree = self.repo.find_tree(index.write_tree()?)?;
        let parent = self.repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        self.repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;
        Ok(())
    }
}

fn diff_to_string(diff: &git2::Diff) -> String {
    let mut out = String::new();
    let _ = diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            out.push(line.origin());
        }
        out.push_str(std::str::from_utf8(line.content()).unwrap_or(""));
        true
    });
    if out.is_empty() {
        out.push_str("(no changes)");
    }
    out
}

fn rel_time(secs: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let d = (now - secs).max(0);
    match d {
        0..=59 => format!("{d}s ago"),
        60..=3599 => format!("{}m ago", d / 60),
        3600..=86399 => format!("{}h ago", d / 3600),
        _ => format!("{}d ago", d / 86400),
    }
}
