//! Optional, power-user font installer (`warren --install-font`).
//!
//! warren runs *inside* your terminal, so it can't change which font the terminal renders with.
//! What it CAN do is drop the bundled "Symbols Nerd Font Mono" into your user font directory.
//! It's a symbols-only font designed as a *fallback*: kitty / WezTerm / iTerm2 / VTE pull missing
//! glyphs from it automatically, so the Nerd Font icons light up without you changing your primary
//! font. Terminals that don't do symbol fallback (e.g. Apple Terminal.app) need a patched font set
//! manually — for those, `icons = "unicode"` in the config is the fallback.
//!
//! The font + its license are embedded at build time (see `assets/`), so install is offline.

use std::path::PathBuf;

use anyhow::{Context, Result};

const FONT_BYTES: &[u8] = include_bytes!("../assets/SymbolsNerdFontMono-Regular.ttf");
const FONT_FILE: &str = "SymbolsNerdFontMono-Regular.ttf";

/// Copy the bundled symbols font into the user font directory and refresh the cache.
pub fn install() -> Result<()> {
    let dir = font_dir().context("could not determine a user font directory")?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let dest = dir.join(FONT_FILE);
    std::fs::write(&dest, FONT_BYTES).with_context(|| format!("writing {}", dest.display()))?;
    println!(
        "installed {FONT_FILE} ({} KB) → {}",
        FONT_BYTES.len() / 1024,
        dest.display()
    );

    #[cfg(not(target_os = "macos"))]
    refresh_cache(&dir);

    println!();
    println!("Done. Fully restart your terminal so it picks up the new fallback font.");
    println!("If glyphs still don't render, your terminal may not do symbol fallback —");
    println!("set a Nerd Font as its primary font, or set icons = \"unicode\" in warren's config.");
    Ok(())
}

/// `~/Library/Fonts` on macOS, `~/.local/share/fonts` (XDG data dir) elsewhere.
fn font_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir().map(|h| h.join("Library").join("Fonts"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        dirs::data_dir().map(|d| d.join("fonts"))
    }
}

/// Rebuild the fontconfig cache so apps see the font without a re-login (best-effort).
#[cfg(not(target_os = "macos"))]
fn refresh_cache(dir: &std::path::Path) {
    match std::process::Command::new("fc-cache").arg("-f").arg(dir).status() {
        Ok(s) if s.success() => println!("refreshed the fontconfig cache (fc-cache -f)"),
        _ => println!("note: run `fc-cache -f` yourself if the glyphs don't appear"),
    }
}
