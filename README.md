<h1 align="center">warren</h1>

<p align="center">
  <b>A terminal IDE that wraps the <a href="https://claude.com/claude-code">Claude Code</a> CLI.</b><br>
  File explorer · editor · embedded <code>claude</code> · terminals · git — all in one terminal.
</p>

<p align="center">
  <a href="https://github.com/Yok4ai/warren/releases"><img src="https://img.shields.io/github/v/release/Yok4ai/warren?color=7aa2f7" alt="release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-7aa2f7" alt="license"></a>
  <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS-9aa5ce" alt="platform">
  <img src="https://img.shields.io/badge/built%20with-Rust-dea584" alt="rust">
</p>

<p align="center">
  <img src="https://raw.githubusercontent.com/Yok4ai/warren/master/images/warren.png?v=3" alt="warren" width="900">
</p>

warren wraps the `claude` CLI in a pane — it isn't another reimplemented agent. Around it
sits a lightweight editor with syntax highlighting, a file explorer, multiple terminals, git, and
**live accept/reject diffs** for the edits Claude proposes. The point: stop juggling separate
windows for `claude`, `nvim`, `npm run dev`, and `git` — keep them all in one terminal.

Built with Rust + [ratatui](https://ratatui.rs). Runs on Linux and macOS.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/Yok4ai/warren/master/install.sh | sh
```

Downloads the prebuilt binary for your OS/architecture (Linux x86_64/arm64, macOS Intel/Apple
Silicon) and installs it to `~/.local/bin`. Override the location with `WARREN_INSTALL_DIR`, or pin
a version with `WARREN_VERSION=v0.1.2`.

<details>
<summary>Build from source (needs Rust ≥ 1.85)</summary>

```sh
git clone https://github.com/Yok4ai/warren
cd warren
cargo install --path .
```
</details>

> The embedded Claude pane and the diff integration need the `claude` CLI on your `PATH`.
> Everything else (editor, explorer, terminals, git) works without it.

## Usage

```sh
warren            # open the current directory
warren /path/dir  # open a specific folder
```

Open a terminal (`ctrl+t`), run `claude`, and ask it to edit a file. warren intercepts the edit and
shows it as a full-file inline diff with line numbers — press `enter` to accept or `esc` to
reject, without leaving the editor.

## Features

- **Wraps `claude`** in a PTY pane — full color, mouse, scrollback, and input fidelity.
- **Live accept/reject diffs** — Claude's edits open as a VS Code-style whole-file diff with dual
  `old │ new` line numbers, syntax highlighting, and a jump to the first change; accept or reject
  in place.
- **Editor** — tabs, syntax highlighting (syntect), rope buffers, undo/redo, find, text selection,
  save, and auto-save.
- **Markdown preview** — toggle a rendered view (`alt+m`) with styled headings, **tables**,
  fenced code blocks, lists, blockquotes, and **inline images** (local files and remote URLs).
- **Image viewer** — open `png`/`jpg`/`gif`/`webp`/… directly, rendered with the terminal graphics
  protocol (crisp kitty graphics, with sixel / iTerm2 / unicode-halfblock fallbacks).
- **Git** — status, staging, commit, and a commit graph in the source-control view; file diffs
  render with full syntax highlighting.
- **File explorer** — lazy tree with filesystem-watch refresh, click/keyboard nav, and it follows
  the active editor tab.
- **Multiple terminals** — vertical tab strip, 5000-line scrollback, drag-to-select + copy.
- **Command palette** (`ctrl+p`) — fuzzy file finder + command runner.
- **Themes** — Tokyo Night / Glow, Catppuccin, Dracula, Gruvbox, Light; optional solid background.

## Keybindings

| Key | Action |
|---|---|
| `ctrl+q` | quit |
| `ctrl+p` | command palette / fuzzy finder |
| `f1` | keybindings overlay |
| **Panes** | |
| `ctrl+b` | toggle sidebar |
| `alt+e` | toggle editor |
| `ctrl+g` | source control |
| `ctrl+w` | cycle focus between panes |
| `ctrl+t` | new terminal |
| `` ctrl+` `` | toggle terminal panel |
| **Files & tabs** | |
| `ctrl+n` | new file |
| `ctrl+s` | save |
| `ctrl+x` | close tab |
| `ctrl+pageup` / `ctrl+pagedown` | previous / next tab |
| **Editing** | |
| `ctrl+z` / `ctrl+y` | undo / redo |
| `ctrl+c` / `ctrl+v` | copy / paste |
| `ctrl+a` | select all |
| `ctrl+f` | find |
| **View** | |
| `alt+m` | toggle markdown preview |
| `alt+s` | toggle scrollbars |
| `alt+a` | toggle auto-save |

In a terminal pane: mouse-drag or `pageup`/`pagedown` scrolls the scrollback; drag selects text
(copied on release); typing snaps back to live output.

## Configuration

warren reads `~/.config/warren/config.toml` for keybindings and `[settings]` defaults. Runtime UI
choices (theme, solid background) persist to `~/.config/warren/state.toml`. Both are optional —
warren ships sensible defaults and writes a starter config on first run.

## How the Claude integration works

warren implements the same WebSocket MCP protocol that Claude Code's editor extensions use. On
startup it writes a lockfile to `~/.claude/ide/<port>.lock` and points any `claude` launched in its
terminals at that port. When Claude calls `openDiff`, warren renders the proposed change in the
editor and reports your accept/reject decision back over the protocol — the same mechanism the
VS Code and JetBrains extensions use. Details in [`docs/ide-integration.md`](docs/ide-integration.md).

## Building

```sh
cargo build --release   # optimized build
cargo clippy            # lint (kept warning-free)
cargo run --release     # run in the current directory
```

Pushing a `v*` tag triggers CI to build and attach the per-platform binaries that `install.sh`
downloads.

## License

[MIT](LICENSE) © [Yok4ai](https://github.com/Yok4ai)
