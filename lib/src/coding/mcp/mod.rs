pub mod client;
pub mod http;
pub mod repo;
pub mod stdio;

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use roc_desk_core::credential::CredentialStore;
use roc_desk_core::error::AppError;

pub use client::{McpClient, McpTool};
pub use repo::McpServersRepo;

/// MCP server configuration, mirrors OpenCode's `mcp` config block, but only
/// supports two transports: a local stdio child process, or remote HTTP
/// (Streamable HTTP's single-JSON-response mode -- SSE long connections are
/// not supported, see the top-of-file comment in `coding::mcp::http` for the
/// scope cut).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    Stdio,
    Http,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub id: Uuid,
    pub name: String,
    pub transport: McpTransportKind,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub url: Option<String>,
    pub headers: HashMap<String, String>,
    pub auth_token_ref: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerInput {
    pub name: String,
    pub transport: McpTransportKind,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub url: Option<String>,
    pub headers: HashMap<String, String>,
    /// The frontend only sends a value when the user actually typed a new
    /// token; empty means "keep whatever's already saved" -- same semantics
    /// as the AI provider manager's `api_key` field.
    pub auth_token: Option<String>,
    pub enabled: bool,
}

fn credential_key(id: Uuid) -> String {
    format!("mcp:{id}:auth_token")
}

/// CRUD + lazy-connection cache for MCP servers. Held long-term by the
/// caller (host's `AppState`, or this crate's `WorkspaceAppState` once
/// wired in), the same "connection shared across workspaces/sessions"
/// pattern as `SshConnectionPool` -- a stdio child process, once up, has no
/// reason to be started again per-session, and an HTTP connection is
/// stateless and naturally shareable too.
pub struct McpServerManager {
    repo: Arc<McpServersRepo>,
    credential_store: Arc<dyn CredentialStore>,
    clients: RwLock<HashMap<Uuid, Arc<McpClient>>>,
}

impl McpServerManager {
    pub fn new(repo: Arc<McpServersRepo>, credential_store: Arc<dyn CredentialStore>) -> Self {
        Self {
            repo,
            credential_store,
            clients: RwLock::new(HashMap::new()),
        }
    }

    pub async fn create(&self, input: McpServerInput) -> Result<McpServer, AppError> {
        let id = Uuid::new_v4();
        let auth_token_ref = match &input.auth_token {
            Some(token) if !token.is_empty() => {
                let key = credential_key(id);
                self.credential_store.set(&key, token).await?;
                Some(key)
            }
            _ => None,
        };
        let server = McpServer {
            id,
            name: input.name,
            transport: input.transport,
            command: input.command,
            args: input.args,
            env: input.env,
            url: input.url,
            headers: input.headers,
            auth_token_ref,
            enabled: input.enabled,
            created_at: Utc::now().to_rfc3339(),
        };
        self.repo.create(&server)?;
        Ok(server)
    }

    pub async fn update(&self, id: Uuid, input: McpServerInput) -> Result<McpServer, AppError> {
        let existing = self
            .repo
            .get(id)?
            .ok_or_else(|| AppError::NotFound(format!("mcp server not found: {id}")))?;
        let auth_token_ref = match &input.auth_token {
            Some(token) if !token.is_empty() => {
                let key = existing
                    .auth_token_ref
                    .clone()
                    .unwrap_or_else(|| credential_key(id));
                self.credential_store.set(&key, token).await?;
                Some(key)
            }
            _ => existing.auth_token_ref,
        };
        let server = McpServer {
            id,
            name: input.name,
            transport: input.transport,
            command: input.command,
            args: input.args,
            env: input.env,
            url: input.url,
            headers: input.headers,
            auth_token_ref,
            enabled: input.enabled,
            created_at: existing.created_at,
        };
        self.repo.update(&server)?;
        self.clients.write().await.remove(&id);
        Ok(server)
    }

    pub async fn delete(&self, id: Uuid) -> Result<(), AppError> {
        if let Some(existing) = self.repo.get(id)? {
            if let Some(key) = existing.auth_token_ref {
                self.credential_store.delete(&key).await?;
            }
        }
        self.repo.delete(id)?;
        self.clients.write().await.remove(&id);
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<McpServer>, AppError> {
        self.repo.list()
    }

    /// Enabled servers only (used when assembling the tool-loop's `tools`
    /// array -- disabled servers never appear among the tools the model can
    /// call, and therefore never get lazily connected).
    pub fn list_enabled(&self) -> Result<Vec<McpServer>, AppError> {
        Ok(self.list()?.into_iter().filter(|s| s.enabled).collect())
    }

    /// Lazy connection: the first time a server is actually used, spawn its
    /// child process / set up its HTTP config and run the `initialize`
    /// handshake; later calls reuse the same `McpClient` (including its
    /// cached `tools/list` result). A stdio child process's lifetime
    /// follows this `Arc<McpClient>` -- `coding::mcp::stdio` explicitly sets
    /// `kill_on_drop(true)` at spawn time (tokio defaults to false; without
    /// it the child would become an orphan process once the app exits), so
    /// the child is killed once the `Arc`'s refcount hits zero.
    pub async fn get_or_connect(&self, server_id: Uuid) -> Result<Arc<McpClient>, AppError> {
        if let Some(client) = self.clients.read().await.get(&server_id).cloned() {
            return Ok(client);
        }
        let server = self
            .repo
            .get(server_id)?
            .ok_or_else(|| AppError::NotFound(format!("mcp server not found: {server_id}")))?;
        let auth_token = match &server.auth_token_ref {
            Some(key) => self.credential_store.get(key).await?,
            None => None,
        };
        let client = Arc::new(McpClient::connect(&server, auth_token.as_deref()).await?);
        self.clients.write().await.insert(server_id, client.clone());
        Ok(client)
    }
}
