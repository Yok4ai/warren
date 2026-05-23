# warren — roadmap & status

Phased build of the TUI IDE. Design doc: `~/.claude/plans/i-want-you-to-ancient-sunbeam.md`.

## Done

- [x] **Phase 0 — PTY proof.** Confirmed the real `claude` runs fully inside a ratatui pane
      (`portable-pty` + `vt100` + `tui-term`). Throwaway lives in `../phase0-pty-proof/`.
- [x] **Phase 1 — Skeleton.** Panic-safe terminal, tokio event funnel + tick, config/keymap,
      single pane + statusline, quit.
- [x] **Phase 2 — Explorer + viewer.** File tree (click/keyboard nav, fs watcher), tabbed
      read-only viewer with syntect highlighting. Mouse focus/scroll, click-to-open,
      tab clicking, pane-scoped text selection + OSC52 copy, coalesced rendering.
- [x] **Phase 3 — Editing.** Rope-backed buffers: cursor, insert/delete, Enter/Tab, save
      (`ctrl+s`), dirty marker, live re-highlight, external-change reload (protects unsaved).
      New file (`ctrl+n`). Word motions (`ctrl+←/→`, `ctrl+backspace`), click-to-place-cursor,
      per-tab close buttons, draggable pane divider, custom scrollbar (grab-drag, no click-jump).
      Selection delete/replace, paste (bracketed), select-all (`ctrl+a`), auto-save (`alt+a`),
      Home/End = file top/bottom, close-confirm (save/discard/cancel), help overlay (`f1`).
- [x] **Phase 5 — Terminal panel + flexible panes (headline).** Editor↔panel split (draggable);
      VS Code-style terminal panel with multiple generic terminals (`ctrl+t` new, `ctrl+\`` toggle),
      a draggable vertical tab strip (click/✕/"+ new", smallest-free numbering). Run `claude`/`npm`
      in any terminal. Pane visibility toggles: sidebar (`ctrl+b`), editor (`alt+e`), panel.
      Focus cycling, mouse forwarding, bracketed paste. Drag-and-drop from explorer → terminal
      (inserts path) or editor (opens). Blinking cursor (editor + terminal). Editor line numbers.

## Next

- [ ] **Phase 4 — Command palette.** `ctrl+p` fuzzy file finder + command list (nucleo).
      Reuses `prompt.rs`'s input. Also enables Save As / quick-open.
- [ ] **Phase 6 — Git.** `git2` status/stage/commit/push/pull/diff + SCM sidebar + commit graph.

## Later / deferred

- [ ] Inline accept/reject diff by impersonating Claude Code's IDE server (WS MCP via
      `~/.claude/ide/<pid>.lock`) — render claude's edits in our editor pane.
- [ ] Multiple terminals, workspace tabs (bottom bar), markdown preview, GitHub auth.
- [ ] Editor: undo/redo, horizontal scroll, explorer scrollbar.

## Known constraints

- Pinned `ratatui 0.29` / MSRV 1.85 (see `CLAUDE.md`).
- Interactive TUI — verify by running `cargo run --release`, not headless.
