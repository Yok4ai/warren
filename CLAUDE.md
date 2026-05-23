# warren

A terminal TUI "IDE" that **wraps the real Claude Code CLI** (not a reimplemented agent).
Goal: file explorer + lightweight editor + an embedded `claude` pane + git, all in one terminal,
so you never leave it to juggle separate windows for `claude`, `npm run dev`, `btop`, etc.

Built with **Rust + ratatui**. The full design and phase plan live in
`~/.claude/plans/i-want-you-to-ancient-sunbeam.md`; current status is in `TASKS.md`.

## Build / run / lint

```bash
cargo run --release            # run warren in the current directory
cargo run --release -- /path   # open a specific folder
cargo clippy                   # lint (keep it clean — zero warnings)
cargo build --release          # optimized build
```

Always test with `--release`: debug builds make full-screen TUI redraws noticeably laggy.
warren is an interactive TUI, so it can't be driven headlessly — it needs a real terminal
(`enable_raw_mode` fails without a TTY). Manual run + eyeballing is the verification loop.

## Architecture

Single-threaded state, one event funnel, tick-coalesced rendering.

- `main.rs` — entry: load config → `tui::init` → `App::run` → `tui::restore`.
- `tui.rs` — raw-mode/alt-screen lifecycle, panic-safe restore, kitty keyboard-enhancement.
- `event.rs` — `AppEvent` enum; an input thread (blocking `crossterm::read`) + a 16ms tick
  feed one `mpsc`. **Rendering happens only on a tick, gated by a dirty flag**, so bursts of
  input/mouse-motion coalesce into one frame.
- `app.rs` — `App` global state and the run loop. `App` is the **only writer of state**.
  Key dispatch: modal `prompt` → global keys → focused component (`Sidebar` | `Editor`).
  Mouse handling (focus, click-to-select, divider resize, scrollbar drag, text selection).
- `config.rs` — `KeyChord` parser + `Keymap`; loads `~/.config/warren/config.toml`
  (written with defaults on first run).
- `explorer.rs` — `FileTree`: lazily-expanded dir tree flattened to visible rows; owns its
  scroll offset so mouse clicks map to rows.
- `editor.rs` — `Editor` (tabs) + rope-backed `Buffer` (cursor, edits, selection, save).
  Highlight cache is rebuilt lazily (`refresh_highlight`, once per tick after edits).
- `highlight.rs` — syntect (fancy-regex, no C deps); `highlight_rope` highlights per rope line
  so line counts stay aligned with cursor coordinates.
- `watcher.rs` — `notify` fs watcher → `AppEvent::FsChanged`; ignores `target/.git/node_modules`.
- `prompt.rs` — reusable modal single-line input (currently new-file).
- `theme.rs` — the single built-in dark theme.
- `ui.rs` — all rendering: sidebar + editor (tabs/content/selection/scrollbar) + statusline
  + prompt overlay. Writes back per-frame geometry (hitboxes, content area) used for mouse mapping.

## Conventions / gotchas

- **Pinned `ratatui 0.29`** (0.30 needs Rust ≥1.86; project MSRV is 1.85). Same for `notify`,
  `ropey 1.x` — verify MSRV at `cargo add` time.
- `syntect` uses `default-fancy` (pure-Rust regex) to avoid the oniguruma C build.
- Clipboard = **OSC 52** escape (no clipboard daemon, works over SSH); see `copy_to_clipboard`.
- Don't paint a full-screen background — the user runs a transparent kitty; let it show through.
- Comments explain *why*, match surrounding density. Keep `cargo clippy` warning-free.
- Mouse capture is on, so native terminal selection needs Shift+drag; warren does its own
  pane-scoped selection instead.

## Default keybindings

`ctrl+q` quit · `ctrl+p` palette (todo) · `ctrl+b` toggle sidebar · `ctrl+j` claude pane (todo)
· `ctrl+w` cycle focus · `ctrl+n` new file · `ctrl+s` save · `ctrl+x` close tab
· `ctrl+pageup/pagedown` prev/next tab · `alt+s` toggle scrollbar · `alt+a` toggle auto-save
· `f1` keybindings overlay.
Editor: arrows + word motion (ctrl+←/→, ctrl+backspace), home/end = file top/bottom, click-to-
place-cursor, drag-select (auto-scrolls), `ctrl+a` select all, `ctrl+c` copy, paste, select-replace.

## Deferred (future)

Custom inline accept/reject diff by impersonating Claude Code's IDE server (WebSocket MCP via
`~/.claude/ide/<pid>.lock`), multiple terminals, workspace tabs, markdown preview, GitHub auth,
undo/redo, horizontal scroll.
