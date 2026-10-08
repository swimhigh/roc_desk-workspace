use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use roc_desk_core::error::AppError;

use super::client::McpTransport;
use super::McpServer;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// HTTP transport: MCP's "Streamable HTTP" spec. Confirmed against a real
/// internal MCP server (FastMCP/uvicorn stack) that even a single-message
/// response always gets wrapped in `Content-Type: text/event-stream`
/// (`event: message\ndata: {...}\n\n`) rather than a plain JSON body --
/// SSE is the default framing for that stack, not something reserved for
/// genuinely streaming responses. Skipping SSE parsing entirely would fail
/// to connect to most real-world MCP HTTP deployments, so this includes a
/// minimal SSE parser: split on blank lines, pull the `data:` field, parse
/// it as a JSON-RPC message (not a full EventSource implementation --
/// doesn't handle `id:`/`retry:`/multi-line `data:` concatenation, just
/// enough for the one framing MCP actually uses). Returns as soon as the
/// first message whose `id` matches the request's own id arrives, no need
/// to wait for the connection to close -- servers typically close the
/// stream shortly after sending the result.
///
/// The handshake response may carry an `Mcp-Session-Id` response header
/// that subsequent requests must echo back, or some server implementations
/// reject them outright -- minimal support here: remember the first
/// session id seen, attach it to every later request.
pub struct HttpTransport {
    client: reqwest::Client,
    url: String,
    headers: HashMap<String, String>,
    auth_token: Option<String>,
    session_id: Mutex<Option<String>>,
    next_id: AtomicI64,
}

impl HttpTransport {
    pub fn new(server: &McpServer, auth_token: Option<&str>) -> Self {
        Self {
            client: reqwest::Client::new(),
            url: server.url.clone().unwrap_or_default(),
            headers: server.headers.clone(),
            auth_token: auth_token.map(str::to_string),
            session_id: Mutex::new(None),
            next_id: AtomicI64::new(1),
        }
    }

    async fn post(
        &self,
        body: Value,
        expect_response: bool,
        expected_id: Option<i64>,
    ) -> Result<Option<Value>, AppError> {
        let mut req = self
            .client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .json(&body);
        for (key, value) in &self.headers {
            req = req.header(key.as_str(), value.as_str());
        }
        if let Some(token) = &self.auth_token {
            req = req.bearer_auth(token);
        }
        if let Some(session) = self.session_id.lock().await.clone() {
            req = req.header("Mcp-Session-Id", session);
        }

        let resp = tokio::time::timeout(CALL_TIMEOUT, req.send())
            .await
            .map_err(|_| AppError::Connection("MCP HTTP 请求超时".into()))??;

        if let Some(session) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().await = Some(session.to_string());
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::Connection(format!("MCP HTTP {status}: {text}")));
        }
        if !expect_response {
            return Ok(None);
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        if content_type.starts_with("text/event-stream") {
            return tokio::time::timeout(CALL_TIMEOUT, Self::read_sse_response(resp, expected_id))
                .await
                .map_err(|_| AppError::Connection("MCP SSE 响应超时".into()))?;
        }

        let value: Value = resp.json().await.map_err(AppError::from)?;
        Ok(Some(value))
    }

    /// Splits on blank lines, parses each frame's `data:` field as JSON --
    /// only a frame carrying an `id` that matches this request's id is
    /// treated as the final result; id-less frames (server-pushed
    /// logs/progress notifications) are ignored, waiting for the next
    /// frame. Returns as soon as a matching frame is found, dropping `resp`
    /// (the connection closes naturally via reqwest/hyper) rather than
    /// waiting for the server to end the stream on its own.
    async fn read_sse_response(
        resp: reqwest::Response,
        expected_id: Option<i64>,
    ) -> Result<Option<Value>, AppError> {
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(AppError::from)?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            // Confirmed this server (uvicorn/FastMCP) frames with `\r\n\r\n`,
            // not bare `\n\n` -- normalize line endings first, otherwise the
            // `\r` sitting between the two `\n`s never matches and the SSE
            // response just waits until it times out.
            buf = buf.replace("\r\n", "\n");
            while let Some(pos) = buf.find("\n\n") {
                let frame = buf[..pos].to_string();
                buf.drain(..pos + 2);
                for line in frame.lines() {
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
                        continue;
                    };
                    let frame_id = value.get("id").and_then(|v| v.as_i64());
                    if frame_id.is_some() && (expected_id.is_none() || frame_id == expected_id) {
                        return Ok(Some(value));
                    }
                }
            }
        }
        Err(AppError::Connection(
            "MCP SSE 响应流结束但未收到匹配的响应帧".into(),
        ))
    }
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn call(&self, method: &str, params: Value) -> Result<Value, AppError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let value = self
            .post(body, true, Some(id))
            .await?
            .ok_or_else(|| AppError::Internal("MCP 服务器未返回结果".into()))?;
        if let Some(err) = value.get("error") {
            return Err(AppError::Internal(format!("MCP 调用 {method} 失败：{err}")));
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), AppError> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.post(body, false, None).await?;
        Ok(())
    }
}
