//! The single event funnel. Every source (terminal input, redraw ticks, and later PTY
//! output / filesystem / git results) pushes an [`AppEvent`] into one mpsc channel that the
//! app's run loop drains. This keeps rendering on one thread and state mutation in one place.

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event as CtEvent};
use tokio::sync::mpsc::UnboundedSender;

/// Everything the run loop reacts to. New variants (PtyOutput, GitRefreshed, …) get added
/// here as later phases land.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// A terminal input event: key, mouse, paste, or resize.
    Input(CtEvent),
    /// One or more paths changed on disk (from the filesystem watcher).
    FsChanged(Vec<PathBuf>),
    /// The embedded terminal produced output (its screen changed).
    PtyChanged,
    /// The embedded terminal's child process exited.
    PtyExited,
    /// Redraw cadence. Rendering is gated on a dirty flag so idle ticks are cheap.
    Tick,
}

/// Redraw cadence (~60fps). Rendering happens only on a tick, and only when something marked
/// the UI dirty — so bursts of input (key-repeat, mouse motion) coalesce into one frame.
const TICK: Duration = Duration::from_millis(16);

/// Spawn a blocking thread that reads terminal input and forwards it to the funnel.
/// crossterm's `read()` is blocking, so it lives on its own OS thread rather than the runtime.
pub fn spawn_input(tx: UnboundedSender<AppEvent>) {
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if tx.send(AppEvent::Input(ev)).is_err() {
                break; // receiver dropped: app is shutting down
            }
        }
    });
}

/// Spawn the redraw tick on the async runtime.
pub fn spawn_ticks(tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        loop {
            interval.tick().await;
            if tx.send(AppEvent::Tick).is_err() {
                break;
            }
        }
    });
}
