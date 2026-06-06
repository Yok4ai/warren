//! IDE integration: warren impersonates the editor that Claude Code connects to, so Claude's
//! edits show up as an accept/reject diff in our editor. We run an MCP server over WebSocket,
//! advertise the IDE tools, and handle `openDiff`. See `docs/ide-integration.md`.
//!
//! Every WS frame is logged to `~/.claude/warren-ide.log` for debugging the (undocumented) handshake.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;

use crate::event::AppEvent;

/// The user's decision on a proposed diff.
pub enum DiffDecision {
    /// Accept; the (possibly edited) final file contents.
    Accept(String),
    Reject,
}

/// Handle to the running IDE server; removes its lockfile on drop.
pub struct IdeServer {
    pub port: u16,
    lockfile: PathBuf,
    /// Fan-out of outbound JSON-RPC notifications to every connected Claude client.
    notify: broadcast::Sender<String>,
}

impl IdeServer {
    /// Push a JSON-RPC notification (e.g. `selection_changed`) to all connected clients.
    /// Best-effort: drops silently when nobody is listening.
    pub fn notify(&self, msg: String) {
        let _ = self.notify.send(msg);
    }
}

impl Drop for IdeServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lockfile);
    }
}

/// Build a `selection_changed` notification — tells Claude what the user is looking at right now.
/// Line/character are 0-based (VS Code Position convention); pass `start == end` for a bare cursor.
pub fn selection_changed(file: &str, start: (usize, usize), end: (usize, usize), text: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "selection_changed",
        "params": {
            "selection": {
                "start": { "line": start.0, "character": start.1 },
                "end": { "line": end.0, "character": end.1 },
            },
            "text": text,
            "filePath": file,
        },
    })
    .to_string()
}

/// Build an `at_mentioned` notification — the explicit "reference this file (lines) in the prompt"
/// action. Lines are 0-based; Claude renders them 1-based as `@file#L…`. `None` mentions the whole file.
pub fn at_mentioned(file: &str, lines: Option<(usize, usize)>) -> String {
    let mut params = json!({ "filePath": file });
    if let Some((start, end)) = lines {
        params["lineStart"] = json!(start);
        params["lineEnd"] = json!(end);
    }
    json!({ "jsonrpc": "2.0", "method": "at_mentioned", "params": params }).to_string()
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

fn log(msg: &str) {
    use std::io::Write;
    if let Some(path) = home().map(|h| h.join(".claude").join("warren-ide.log")) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{} {msg}", now_nanos());
        }
    }
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Start the IDE server: bind a random local port, write the lockfile, and accept MCP/WS clients.
pub async fn start(workspace: PathBuf, tx: UnboundedSender<AppEvent>) -> Option<IdeServer> {
    let listener = TcpListener::bind("127.0.0.1:0").await.ok()?;
    let port = listener.local_addr().ok()?.port();
    let token = format!("warren-{}-{}", std::process::id(), now_nanos());

    let dir = home()?.join(".claude").join("ide");
    std::fs::create_dir_all(&dir).ok()?;
    let lockfile = dir.join(format!("{port}.lock"));
    let body = json!({
        "pid": std::process::id(),
        "workspaceFolders": [workspace.to_string_lossy()],
        "ideName": "warren",
        "transport": "ws",
        "runningInWindows": false,
        "authToken": token,
    });
    std::fs::write(&lockfile, body.to_string()).ok()?;
    log(&format!("ide server listening on {port}, lockfile {lockfile:?}"));

    let (notify, _) = broadcast::channel::<String>(64);
    let accept_notify = notify.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(handle_conn(stream, tx, accept_notify.subscribe()));
        }
    });

    Some(IdeServer {
        port,
        lockfile,
        notify,
    })
}

async fn handle_conn(
    stream: tokio::net::TcpStream,
    tx: UnboundedSender<AppEvent>,
    mut notify_rx: broadcast::Receiver<String>,
) {
    let ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            log(&format!("ws handshake error: {e}"));
            return;
        }
    };
    log("client connected");
    let (mut sink, mut stream) = ws.split();
    // One task owns the sink; multiplex inbound requests against app-pushed notifications so
    // warren can speak spontaneously (selection_changed/at_mentioned), not just reply.
    loop {
        tokio::select! {
            note = notify_rx.recv() => match note {
                Ok(text) => {
                    log(&format!(">> (notify) {text}"));
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = stream.next() => {
                let Some(msg) = msg else { break };
                let msg = match msg {
                    Ok(m) => m,
                    Err(_) => break,
                };
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Ping(p) => {
                        let _ = sink.send(Message::Pong(p)).await;
                        continue;
                    }
                    Message::Close(_) => break,
                    _ => continue,
                };
                log(&format!("<< {text}"));
                if let Some(resp) = handle_message(&text, &tx).await {
                    log(&format!(">> {resp}"));
                    if sink.send(Message::Text(resp.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
    log("client disconnected");
}

/// Handle one JSON-RPC message; returns a response string for requests (those with an `id`).
async fn handle_message(text: &str, tx: &UnboundedSender<AppEvent>) -> Option<String> {
    let v: Value = serde_json::from_str(text).ok()?;
    let method = v.get("method")?.as_str()?;
    let id = v.get("id").cloned();

    match method {
        "initialize" => {
            let pv = v["params"]["protocolVersion"]
                .as_str()
                .unwrap_or("2025-06-18");
            Some(reply(
                id,
                json!({
                    "protocolVersion": pv,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "warren", "version": env!("CARGO_PKG_VERSION") },
                }),
            ))
        }
        "notifications/initialized" => {
            // Client is ready; re-sync editor state so a mid-session connect knows the selection.
            let _ = tx.send(AppEvent::IdeConnected);
            None
        }
        "tools/list" => Some(reply(id, json!({ "tools": tool_defs() }))),
        "tools/call" => {
            let name = v["params"]["name"].as_str().unwrap_or("");
            let args = &v["params"]["arguments"];
            match name {
                "openDiff" => {
                    let path = args["old_file_path"].as_str().unwrap_or("").to_string();
                    let new_contents = args["new_file_contents"].as_str().unwrap_or("").to_string();
                    let tab_name = args["tab_name"].as_str().unwrap_or("diff").to_string();
                    let (rtx, rrx) = oneshot::channel();
                    let _ = tx.send(AppEvent::OpenDiff {
                        path,
                        new_contents,
                        tab_name,
                        reply: rtx,
                    });
                    let content = match rrx.await {
                        Ok(DiffDecision::Accept(text)) => json!([
                            { "type": "text", "text": "FILE_SAVED" },
                            { "type": "text", "text": text },
                        ]),
                        _ => json!([{ "type": "text", "text": "DIFF_REJECTED" }]),
                    };
                    Some(reply(id, json!({ "content": content })))
                }
                "close_tab" | "closeAllDiffTabs" => {
                    let _ = tx.send(AppEvent::CloseDiff);
                    Some(reply(id, json!({ "content": [{ "type": "text", "text": "ok" }] })))
                }
                // Stubs for the rest — return empty/sane results.
                _ => Some(reply(id, json!({ "content": [{ "type": "text", "text": "[]" }] }))),
            }
        }
        _ => id.map(|_| reply(id_of(text), json!({}))),
    }
}

fn id_of(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("id").cloned())
}

fn reply(id: Option<Value>, result: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result,
    })
    .to_string()
}

fn tool_defs() -> Value {
    let str_prop = || json!({ "type": "string" });
    json!([
        {
            "name": "openDiff",
            "description": "Show a diff for a proposed file change and return accept/reject.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "old_file_path": str_prop(),
                    "new_file_path": str_prop(),
                    "new_file_contents": str_prop(),
                    "tab_name": str_prop(),
                },
                "required": ["old_file_path", "new_file_path", "new_file_contents", "tab_name"],
            },
        },
        { "name": "close_tab", "description": "Close a diff tab.",
          "inputSchema": { "type": "object", "properties": { "tab_name": str_prop() } } },
        { "name": "closeAllDiffTabs", "description": "Close all diff tabs.",
          "inputSchema": { "type": "object", "properties": {} } },
        // Claude (2.1.x) polls getDiagnostics; warren has no language server, so the call is
        // handled with an empty result. The pull tools getOpenEditors/getCurrentSelection/
        // getWorkspaceFolders aren't called by current Claude — editor state is pushed instead
        // via the `selection_changed`/`at_mentioned` notifications (see `selection_changed`).
        { "name": "getDiagnostics", "description": "Get language diagnostics.",
          "inputSchema": { "type": "object", "properties": { "uri": str_prop() } } },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    // The notification shapes are reverse-engineered from Claude's zod schemas in the CLI bundle;
    // these guard the exact field names/nesting Claude validates against (a typo = silently ignored).

    #[test]
    fn selection_changed_matches_schema() {
        let v: Value =
            serde_json::from_str(&selection_changed("/tmp/a.rs", (3, 5), (4, 0), "hi")).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["method"], "selection_changed");
        // Notification: no `id` field.
        assert!(v.get("id").is_none());
        let p = &v["params"];
        assert_eq!(p["filePath"], "/tmp/a.rs");
        assert_eq!(p["text"], "hi");
        // 0-based line/character (Claude adds 1 for display).
        assert_eq!(p["selection"]["start"]["line"], 3);
        assert_eq!(p["selection"]["start"]["character"], 5);
        assert_eq!(p["selection"]["end"]["line"], 4);
        assert_eq!(p["selection"]["end"]["character"], 0);
    }

    #[test]
    fn at_mentioned_with_and_without_lines() {
        let with: Value =
            serde_json::from_str(&at_mentioned("/tmp/a.rs", Some((2, 9)))).unwrap();
        assert_eq!(with["method"], "at_mentioned");
        assert_eq!(with["params"]["filePath"], "/tmp/a.rs");
        assert_eq!(with["params"]["lineStart"], 2);
        assert_eq!(with["params"]["lineEnd"], 9);

        // Whole-file mention omits the line range entirely.
        let whole: Value = serde_json::from_str(&at_mentioned("/tmp/a.rs", None)).unwrap();
        assert!(whole["params"].get("lineStart").is_none());
        assert!(whole["params"].get("lineEnd").is_none());
    }
}
