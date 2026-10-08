use async_trait::async_trait;
use serde_json::{json, Value};

use roc_desk_core::error::AppError;

use super::http::HttpTransport;
use super::stdio::StdioTransport;
use super::{McpServer, McpTransportKind};

/// One MCP JSON-RPC 2.0 round trip (`initialize`/`tools/list`/`tools/call`),
/// implemented separately for stdio and HTTP. `notify` is a JSON-RPC
/// notification (no `id`, no response awaited), currently only used for the
/// post-handshake `notifications/initialized`.
#[async_trait]
pub trait McpTransport: Send + Sync {
    async fn call(&self, method: &str, params: Value) -> Result<Value, AppError>;
    async fn notify(&self, method: &str, params: Value) -> Result<(), AppError>;
}

#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// An established connection to a single MCP server: handshake complete,
/// tool list cached. `McpServerManager` lazily creates and holds these
/// long-term (see `coding::mcp::McpServerManager`); this type only concerns
/// itself with the protocol.
pub struct McpClient {
    transport: Box<dyn McpTransport>,
    pub tools: Vec<McpTool>,
}

impl McpClient {
    pub async fn connect(server: &McpServer, auth_token: Option<&str>) -> Result<Self, AppError> {
        let transport: Box<dyn McpTransport> = match server.transport {
            McpTransportKind::Stdio => Box::new(StdioTransport::spawn(server).await?),
            McpTransportKind::Http => Box::new(HttpTransport::new(server, auth_token)),
        };

        let init_params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "roc_desk", "version": env!("CARGO_PKG_VERSION") },
        });
        transport.call("initialize", init_params).await?;
        // The MCP handshake requires the client to send a
        // `notifications/initialized` notification after receiving the
        // initialize response; some implementations are lenient about it
        // (no reply, not an error either) -- a failure here doesn't block
        // the subsequent tools/list, what matters is that the handshake
        // itself already completed.
        let _ = transport
            .notify("notifications/initialized", json!({}))
            .await;

        let tools = Self::fetch_tools(transport.as_ref()).await?;
        Ok(Self { transport, tools })
    }

    async fn fetch_tools(transport: &dyn McpTransport) -> Result<Vec<McpTool>, AppError> {
        // Pagination (`nextCursor`) isn't handled -- MCP servers an
        // individual developer hooks up typically expose single digits to
        // low tens of tools; add pagination if a real need for it shows up.
        let result = transport.call("tools/list", json!({})).await?;
        let raw_tools = result["tools"].as_array().cloned().unwrap_or_default();
        Ok(raw_tools
            .into_iter()
            .filter_map(|t| {
                Some(McpTool {
                    name: t["name"].as_str()?.to_string(),
                    description: t["description"].as_str().unwrap_or_default().to_string(),
                    input_schema: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                })
            })
            .collect())
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, AppError> {
        let result = self
            .transport
            .call(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let text = mcp_content_to_text(&result);
        if result["isError"].as_bool().unwrap_or(false) {
            return Err(AppError::Internal(if text.is_empty() {
                "MCP 工具执行失败".to_string()
            } else {
                text
            }));
        }
        Ok(text)
    }
}

/// MCP tool results are uniformly `{ content: [{type: "text", text: "..."}, ...], isError? }`
/// (there can also be image/resource content items, not handled here --
/// a model tool result can only be text anyway) -- this extracts and joins
/// just the text parts to feed back to the model.
fn mcp_content_to_text(result: &Value) -> String {
    let Some(items) = result["content"].as_array() else {
        return result.to_string();
    };
    items
        .iter()
        .filter_map(|item| item["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
