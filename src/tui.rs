//! Terminal lifecycle: enter/leave raw mode + alternate screen, panic-safe restore.

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Enter raw mode + alternate screen and install a panic hook that restores the terminal
/// before propagating the panic (so a crash never leaves the user's shell garbled).
pub fn init() -> Result<Tui> {
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    // Disambiguate keys like Ctrl+Backspace / Ctrl+Enter where the terminal supports it.
    if matches!(supports_keyboard_enhancement(), Ok(true)) {
        let _ = execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    install_panic_hook();
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    Ok(terminal)
}

/// Restore the terminal to its original state. Safe to call more than once.
pub fn restore() -> Result<()> {
    if matches!(supports_keyboard_enhancement(), Ok(true)) {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    execute!(
        io::stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    disable_raw_mode()?;
    Ok(())
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        original(info);
    }));
}
