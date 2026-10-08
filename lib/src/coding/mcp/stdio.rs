use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

use roc_desk_core::error::AppError;

use super::client::McpTransport;
use super::McpServer;

/// Max wait for a single MCP call -- an MCP server is a machine-to-machine
/// process/connection, not something it's reasonable to wait indefinitely
/// on like a human confirmation dialog; a stuck call should error out
/// rather than dragging down the whole tool loop with it.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// stdio transport: spawns a child process; MCP messages are newline-
/// delimited JSON-RPC 2.0 (`Content-Length` framing is for LSP -- the MCP
/// stdio spec uses the simpler NDJSON). Writes go through a mutex-guarded
/// `ChildStdin` (only one write in flight at a time, so concurrent tool
/// calls can't interleave their request lines); reads run on an independent
/// background task, dispatching by response `id` to whichever caller is
/// waiting -- the same pending-map pattern as `CommandConfirmRegistry`,
/// just keyed by a numeric request id instead of a `Uuid`.
pub struct StdioTransport {
    stdin: Mutex<ChildStdin>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    next_id: AtomicI64,
    /// Only here to tie the child process's lifetime to this struct
    /// (`kill_on_drop(true)` is already set at spawn time, so dropping
    /// this field kills the child) -- never read or written directly.
    _child: Child,
}

impl StdioTransport {
    pub async fn spawn(server: &McpServer) -> Result<Self, AppError> {
        let command = server.command.clone().ok_or_else(|| {
            AppError::Internal(format!("MCP 服务器 {} 未配置可执行命令", server.name))
        })?;

        let mut cmd = Command::new(&command);
        cmd.args(&server.args)
            .envs(&server.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| {
            AppError::Internal(format!("启动 MCP 服务器 {} 失败：{e}", server.name))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::Internal("MCP 子进程没有 stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::Internal("MCP 子进程没有 stdout".into()))?;

        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        let Ok(value) = serde_json::from_str::<Value>(line) else {
                            continue;
                        };
                        // Only handles responses carrying an `id`; server-pushed
                        // notifications without one (e.g. progress hints) are
                        // currently discarded, not consumed.
                        let Some(id) = value.get("id").and_then(|v| v.as_i64()) else {
                            continue;
                        };
                        if let Some(tx) = reader_pending.lock().await.remove(&id) {
                            let payload = if let Some(err) = value.get("error") {
                                json!({ "__mcp_error__": true, "detail": err })
                            } else {
                                value.get("result").cloned().unwrap_or(Value::Null)
                            };
                            let _ = tx.send(payload);
                        }
                    }
                    _ => break, // EOF or a read error: the child process has likely exited, stop reading
                }
            }
        });

        Ok(Self {
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicI64::new(1),
            _child: child,
        })
    }

    async fn write_line(&self, value: &Value) -> Result<(), AppError> {
        let mut line =
            serde_json::to_string(value).map_err(|e| AppError::Internal(e.to_string()))?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(AppError::from)?;
        stdin.flush().await.map_err(AppError::from)
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn call(&self, method: &str, params: Value) -> Result<Value, AppError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.write_line(&request).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

        let result = tokio::time::timeout(CALL_TIMEOUT, rx)
            .await
            .map_err(|_| AppError::Connection(format!("MCP 调用 {method} 超时")))?
            .map_err(|_| AppError::Internal("MCP 子进程提前退出，未收到响应".into()))?;

        if result.get("__mcp_error__").is_some() {
            return Err(AppError::Internal(format!(
                "MCP 调用 {method} 失败：{}",
                result["detail"]
            )));
        }
        Ok(result)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), AppError> {
        self.write_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }
}
