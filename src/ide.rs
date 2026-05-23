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
}

impl Drop for IdeServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lockfile);
    }
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

    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(handle_conn(stream, tx));
        }
    });

    Some(IdeServer { port, lockfile })
}

async fn handle_conn(stream: tokio::net::TcpStream, tx: UnboundedSender<AppEvent>) {
    let ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            log(&format!("ws handshake error: {e}"));
            return;
        }
    };
    log("client connected");
    let (mut sink, mut stream) = ws.split();
    while let Some(msg) = stream.next().await {
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
        "notifications/initialized" => None,
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
        { "name": "getDiagnostics", "description": "Get language diagnostics.",
          "inputSchema": { "type": "object", "properties": { "uri": str_prop() } } },
        { "name": "getOpenEditors", "description": "List open editors.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "getWorkspaceFolders", "description": "List workspace folders.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "getCurrentSelection", "description": "Get the current selection.",
          "inputSchema": { "type": "object", "properties": {} } },
    ])
}
