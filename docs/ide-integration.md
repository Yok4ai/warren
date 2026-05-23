# Claude Code IDE integration — research + plan

Goal: make warren show Claude's edits as a live accept/reject diff in the editor, the way the
VS Code extension does. To do that, warren must **impersonate the IDE** that Claude Code connects
to. Findings below are reverse-engineered from the installed CLI bundle
(`~/.local/share/claude/versions/2.1.150`, a compiled bun binary — grep its embedded strings).

## How the connection works

- **Trigger / discovery.** The CLI auto-connects to an IDE when any of these hold (function `OlH`):
  `autoConnectIde` setting, `process.env.CLAUDE_CODE_SSE_PORT`, or `CLAUDE_CODE_AUTO_CONNECT_IDE`.
  It then reads lockfiles from `~/.claude/ide/` (cwd is normalized NFC and matched against each
  lockfile's `workspaceFolders`).
- **Lockfile** `~/.claude/ide/<port>.lock` (JSON), confirmed shape (this machine's live one):
  `{pid, workspaceFolders:[<abs path>], ideName, transport:"ws", runningInWindows:bool, authToken}`.
- **Transport.** Config schema is `type:"ws-ide"` with `{url, ideName, authToken?(optional),
  ideRunningInWindows?}`. URL is built as `ws://<host>:<port>` (host can be overridden by
  `CLAUDE_CODE_IDE_HOST_OVERRIDE`, default `127.0.0.1`). **`authToken` is optional** — we can start
  without auth and add it if a build requires it. (There's also an SSE path; we use WS.)
- **MCP.** Over that WebSocket they speak MCP (JSON-RPC 2.0). **The IDE is the MCP server; Claude is
  the client.** Claude does `initialize` → `notifications/initialized` → `tools/list` → `tools/call`.

## The diff flow (the part we care about)

When Claude wants to write/edit a file it calls the IDE tool **`openDiff`** (function `Dt7`):

```
openDiff({ old_file_path, new_file_path, new_file_contents, tab_name })
```

It then interprets the tool result array (predicates `zM_`/`_M_`/`AM_` in the bundle):

| Result content[0].text | Meaning | Claude uses |
|---|---|---|
| `"FILE_SAVED"` (+ `content[1].text` = final text) | accepted (possibly user-edited) | `content[1].text` |
| `"TAB_CLOSED"` | accepted as proposed | the proposed `new_file_contents` |
| `"DIFF_REJECTED"` | rejected | keeps the old content (edit aborted) |

So warren's `openDiff` handler must show the diff, wait for the user, then return:
- **Accept** → `[{type:"text",text:"FILE_SAVED"},{type:"text",text:<final contents>}]`
- **Reject** → `[{type:"text",text:"DIFF_REJECTED"}]`

After resolving, Claude also calls **`close_tab`** / **`closeAllDiffTabs`** (function `ZC6`), which
warren should handle by closing the diff view.

Other IDE tools Claude may call (stub them — return empty/sane defaults): `getDiagnostics`,
`getOpenEditors`, `getCurrentSelection`, `getWorkspaceFolders`, `openFile`, `executeCode`.

## Remaining unknowns (resolve during build via a logging WS server)

1. Exact `initialize` params/capabilities Claude expects back, and the precise `tools/list` schema
   it requires (tool input schemas). Easiest to capture live.
2. Whether/where `authToken` is sent on the WS handshake (header vs initialize param). It's optional,
   so step 1 of the build is a no-auth lockfile + a server that logs every frame.

## Implementation plan for warren

1. **Add deps:** `tokio-tungstenite` (WS) + `serde_json` (already have serde).
2. **`ide.rs` module** — a tokio task that:
   - binds a TCP listener on `127.0.0.1:0` (random port), accepts one WS upgrade,
   - writes `~/.claude/ide/<port>.lock` with `{pid: our pid, workspaceFolders:[workspace], ideName:"warren", transport:"ws", runningInWindows:false, authToken:<uuid>}`,
   - speaks MCP server-side: replies to `initialize`, `tools/list` (advertise `openDiff`, `close_tab`,
     `closeAllDiffTabs`, plus stubbed `getDiagnostics`/`getOpenEditors`/`getCurrentSelection`),
   - on `tools/call openDiff`: send `AppEvent::OpenDiff{ old_path, new_contents, tab_name, reply: oneshot::Sender<DiffDecision> }` into the existing mpsc; await the oneshot; respond with FILE_SAVED/DIFF_REJECTED.
   - cleans up the lockfile on exit.
3. **Spawn wiring:** when launching a `claude` terminal (`spawn_terminal` for claude), set
   `CLAUDE_CODE_SSE_PORT=<port>` (and `ENABLE_IDE_INTEGRATION`/`CLAUDE_CODE_AUTO_CONNECT_IDE` if needed)
   so that embedded `claude` connects to warren as its IDE.
4. **Diff UI:** reuse the read-only diff buffer (we already render green/red). Add an *interactive*
   accept/reject diff view: shows `old` vs `new_file_contents` with a header bar ("Accept ⌃↵ ·
   Reject Esc"), and on decision fires the `oneshot` back to the IDE task. Allowing in-pane edits
   (FILE_SAVED with edited text) is a v2; v1 returns the proposed text on accept.
5. **AppEvent::OpenDiff** carries a `oneshot::Sender`; the run loop opens the diff and stores the
   responder until the user accepts/rejects.

## Risks / notes

- Undocumented protocol; the `initialize`/`tools/list` exact shapes need a live capture (build a
  logging WS server first, point one `claude` at it, record the frames).
- Only one IDE connection at a time is typical; if multiple claude terminals run, share one server.
- Keep it behind a toggle/setting in case the protocol shifts between CLI versions.
