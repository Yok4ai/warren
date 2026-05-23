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
- `config.rs` — `KeyChord` parser + `Keymap`; loads `~/.config/warren/config.toml` (keymap +
  `[settings]` defaults). Runtime UI choices (theme, solid_bg) persist to a separate
  `state.toml` (so config.toml isn't clobbered); state overrides config settings on load.
- `explorer.rs` — `FileTree`: lazily-expanded dir tree flattened to visible rows; owns its
  scroll offset so mouse clicks map to rows.
- `editor.rs` — `Editor` (tabs) + rope-backed `Buffer` (cursor, edits, selection, save).
  Highlight cache is rebuilt lazily (`refresh_highlight`, once per tick after edits).
- `highlight.rs` — syntect (fancy-regex, no C deps); `highlight_rope` highlights per rope line
  so line counts stay aligned with cursor coordinates.
- `watcher.rs` — `notify` fs watcher → `AppEvent::FsChanged`; ignores `target/.git/node_modules`.
- `terminal.rs` — `TerminalPane` (PTY via portable-pty/vt100/tui-term) + `Panel` (multi-terminal).
- `palette.rs` — fuzzy file finder + command mode (nucleo-matcher).
- `git.rs` — `git2` (no-TLS): status, log, diffs, commit-files, stage/unstage, commit.
- `prompt.rs` — reusable modal single-line input (currently new-file).
- `theme.rs` — the single built-in dark theme.
- `ui.rs` — all rendering: sidebar + editor (tabs/content/selection/scrollbar) + statusline
  + prompt overlay. Writes back per-frame geometry (hitboxes, content area) used for mouse mapping.

## Conventions / gotchas

- **Pinned `ratatui 0.29`** (0.30 needs Rust ≥1.86; project MSRV is 1.85). Same for `notify`,
  `ropey 1.x`, and **`git2 0.19` with `--no-default-features`** (0.21 needs Rust ≥1.87; default
  features pull `openssl-sys`, absent here — so push/pull/TLS is disabled). Verify MSRV at `cargo add`.
- `syntect` uses `default-fancy` (pure-Rust regex) to avoid the oniguruma C build.
- Clipboard = **OSC 52** escape (no clipboard daemon, works over SSH); see `copy_to_clipboard`.
- Don't paint a full-screen background — the user runs a transparent kitty; let it show through.
- Comments explain *why*, match surrounding density. Keep `cargo clippy` warning-free.
- Mouse capture is on, so native terminal selection needs Shift+drag; warren does its own
  pane-scoped selection instead.
- **Scrollbar house style** (editor vertical/horizontal, sidebar — keep all consistent):
  1. Custom-drawn via `draw_scrollbar` / `buf.cell_mut` — NOT ratatui's `Scrollbar` (it maps the
     thumb against total length and stops short of the end).
  2. Thumb sized to the visible fraction and positioned over the draggable range `track - thumb`,
     so it sits **flush at the end** at max scroll.
  3. **Click never jumps — only dragging scrolls**, with a grab offset so the thumb tracks the
     cursor (`*_grab_offset` + `*_to` helpers in `app.rs`; e.g. `scroll_bar_to`, `hbar_to`,
     `sidebar_sb_to`). Reserve a track row/column in the layout so it doesn't overlap content.
  4. The scroll offset is **independent of selection**: wheel/drag move the view only; keyboard
     nav moves the selection and nudges the offset to keep it visible (`ensure_visible`-style).

## Default keybindings

`ctrl+q` quit · `ctrl+p` palette · `ctrl+b` toggle sidebar · `ctrl+g` source control
· `alt+e` toggle editor · `ctrl+w` cycle focus
· `ctrl+n` new file · `ctrl+s` save · `ctrl+x` close tab · `ctrl+pageup/pagedown` prev/next tab
· `alt+s` toggle scrollbar · `alt+a` toggle auto-save · `f1` keybindings overlay.
Terminal panel: `ctrl+t` new terminal · `ctrl+\`` toggle panel (spawns a shell if empty; run
`claude`/`npm` in it). In the panel: `ctrl+pageup/pagedown` cycle, `ctrl+x` close, `ctrl+w` leave.
Vertical tab strip on the right (click to switch, ✕ to close, "+ new" row); both the editor↔panel
divider and the content↔strip divider are draggable.
Editor: arrows + word motion (ctrl/alt+←/→, ctrl/alt+backspace), home/end = file top/bottom,
click-to-place-cursor, drag-select (auto-scrolls), `ctrl+a` select all, `ctrl+c`/`ctrl+v` copy/
paste (in-app clipboard + OSC52), `ctrl+z`/`ctrl+y` undo/redo, `ctrl+f` find (live, n/total,
↵/↑ next/prev).

## Deferred (future)

Custom inline accept/reject diff by impersonating Claude Code's IDE server (WebSocket MCP via
`~/.claude/ide/<pid>.lock`), multiple terminals, workspace tabs, markdown preview, GitHub auth,
undo/redo, horizontal scroll.
