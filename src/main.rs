//! warren — a terminal IDE that wraps the Claude Code CLI.

mod app;
mod config;
mod editor;
mod event;
mod explorer;
mod git;
mod highlight;
mod palette;
mod prompt;
mod terminal;
mod theme;
mod tui;
mod ui;
mod watcher;

use anyhow::Result;

use crate::app::App;
use crate::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load();
    let mut terminal = tui::init()?;
    let result = App::new(config).run(&mut terminal).await;
    tui::restore()?;
    result
}
