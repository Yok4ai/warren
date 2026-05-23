//! warren — a terminal IDE that wraps the Claude Code CLI.

mod app;
mod config;
mod editor;
mod event;
mod explorer;
mod find;
mod git;
mod highlight;
mod ide;
mod markdown;
mod palette;
mod prompt;
mod terminal;
mod theme;
mod tui;
mod update;
mod ui;
mod watcher;

use anyhow::Result;

use crate::app::App;
use crate::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load();
    let mut terminal = tui::init()?;
    // Detect the terminal's graphics capabilities + cell size for image rendering. Must run after
    // raw mode is on but before the input reader thread starts (it reads the query responses from
    // stdin). Falls back to a guessed cell size + unicode half-blocks if the query fails.
    let picker = ratatui_image::picker::Picker::from_query_stdio()
        .unwrap_or_else(|_| ratatui_image::picker::Picker::from_fontsize((8, 16)));
    let result = App::new(config, Some(picker)).run(&mut terminal).await;
    tui::restore()?;
    result
}
