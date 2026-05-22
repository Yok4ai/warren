//! Filesystem watcher. notify runs its callback on its own thread; we forward changed paths
//! into the app's event funnel as [`AppEvent::FsChanged`]. The returned watcher must be kept
//! alive (dropping it stops watching), so the app holds onto it.

use std::path::Path;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

use crate::event::AppEvent;

/// Directory names whose contents change constantly and aren't interesting to show or react to.
/// Their churn (especially `target/` during a build) would otherwise flood the event funnel.
const IGNORED: &[&str] = &[".git", "target", "node_modules"];

fn is_ignored(path: &Path) -> bool {
    path.components()
        .any(|c| IGNORED.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

/// Begin watching `root` recursively. Returns `None` if a watcher can't be created.
pub fn spawn(root: &Path, tx: UnboundedSender<AppEvent>) -> Option<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            let paths: Vec<_> = event.paths.into_iter().filter(|p| !is_ignored(p)).collect();
            if !paths.is_empty() {
                let _ = tx.send(AppEvent::FsChanged(paths));
            }
        }
    })
    .ok()?;
    watcher.watch(root, RecursiveMode::Recursive).ok()?;
    Some(watcher)
}
