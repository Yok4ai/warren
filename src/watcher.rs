//! Filesystem watcher. notify runs its callback on its own thread; we forward changed paths
//! into the app's event funnel as [`AppEvent::FsChanged`]. The returned watcher must be kept
//! alive (dropping it stops watching), so the app holds onto it.

use std::path::Path;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

use crate::event::AppEvent;

/// Begin watching `root` recursively. Returns `None` if a watcher can't be created.
pub fn spawn(root: &Path, tx: UnboundedSender<AppEvent>) -> Option<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            let _ = tx.send(AppEvent::FsChanged(event.paths));
        }
    })
    .ok()?;
    watcher.watch(root, RecursiveMode::Recursive).ok()?;
    Some(watcher)
}
