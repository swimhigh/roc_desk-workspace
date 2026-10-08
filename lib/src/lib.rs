//! Coding workspace: local terminal, Git panel, and workspace/recent-folder
//! tracking for opening a local folder as a coding workspace.
//!
//! ## Migration status (see the repository README / host's
//! `docs/MULTI_REPO_SPLIT_PROGRESS.md` "编程工作区" section for the full
//! writeup)
//!
//! Ported and command-tested via `cargo check`:
//! - Workspace concept (open a local folder, remember it in a "recent"
//!   list) -- backed by the newly added `roc_desk_core::workspace` (this
//!   crate is its first consumer; the module lives in `roc_desk-common`
//!   because the plan classifies it as a kernel concept shared by multiple
//!   tools, even though only this tool uses it today).
//! - Local terminal (`pty`), ported 1:1 from the host.
//! - A local-only Git panel backend (`git`): status/diff/log/commit,
//!   talking to the `git` binary directly via argv.
//! - Local filesystem browsing/editing + a root-path-keyed symbol index +
//!   an embeddable `<EditorPane/>` are **not reimplemented here** -- they
//!   come from depending on `roc_desk-editor` (which itself re-exports
//!   `roc_desk-explorer`). `standalone/src/main.rs` registers
//!   `roc_desk_explorer::cmd::*` and `roc_desk_editor::symbols::editor_symbols_*`
//!   directly alongside this crate's own commands.
//!
//! **Not ported in this pass** (left as host-only, not stubbed with fake
//! implementations):
//! - The AI programming assistant's multi-turn Agent loop and tool
//!   definitions (host's `coding::session`/`coding::tools`, ~3500 lines) --
//!   these depend on host-only `crate::ai`/`crate::agent_llm` (Provider
//!   management, LLM call/streaming/retry) that has not been ported into
//!   `roc_desk_core` yet, and the Agent loop itself is deeply coding-specific
//!   (tool definitions, permission gating, change-staging with undo). This
//!   was judged too large to attempt safely alongside everything else in
//!   this pass -- see the task's final report for the full reasoning.
//! - Remote (SSH/Agent) *workspace opening* is now supported (see
//!   `WorkspaceAppState::with_ssh`, `cmd::workspace_open_remote`) -- but
//!   remote *terminal sessions* (`pty_*` only ever spawns a local shell) and
//!   the Git panel (`git_*` only ever shells out locally) are still
//!   local-only; those would need their own remote execution path
//!   (`roc_desk_ssh::ssh::session`/`agent::session` exec), not attempted
//!   here.
//! - Skills archive import (`coding::skills`) and web fetch
//!   (`coding::webfetch`) -- lower priority per the task brief, skipped to
//!   keep scope manageable.
//! - File-change diff/undo staging (`coding::changes`/`coding::diff`) --
//!   this exists in the host specifically to stage the AI Agent's proposed
//!   edits for accept/reject/undo; without the Agent loop there is no
//!   producer for it in this pass, so it was not ported either.

pub const TOOL_NAME: &str = "roc_desk-workspace";
pub const TOOL_DESCRIPTION: &str = "编程工作区：代码、终端与 Git";

pub use roc_desk_core::connection::{ConnectionKind, ConnectionProfile};

pub fn tool_info() -> (&'static str, &'static str) {
    (TOOL_NAME, TOOL_DESCRIPTION)
}

pub mod git;
pub mod pty;

/// Re-exported so downstream crates (host, `standalone`) can reach the
/// local-filesystem/editor/symbol-index commands via
/// `roc_desk_workspace::roc_desk_editor::*` without adding their own
/// explicit dependency, the same pattern `roc_desk-editor` uses for
/// `roc_desk-explorer`.
pub use roc_desk_editor;
pub use roc_desk_explorer;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use roc_desk_core::error::AppError;
use roc_desk_core::workspace::{WorkspaceManager, WorkspaceProfile};
use roc_desk_common::fsops::FileOps;

/// A currently-*open* workspace's runtime handle -- not persisted, rebuilt
/// every time a workspace is opened (mirrors the host's
/// `workspace::WorkspaceHandle`). Consumers that need to act on an open
/// workspace (a future ported AI coding agent, first and foremost) look
/// this up from [`WorkspaceAppState::open_workspaces`] by workspace id
/// instead of re-deriving a `FileOps` from the profile every time.
#[derive(Clone)]
pub struct WorkspaceHandle {
    pub profile: WorkspaceProfile,
    pub file_ops: Arc<dyn FileOps>,
}

/// Remote (SSH/Agent) connection pools, needed to resolve a `connection_id`
/// into an actual connection/`FileOps` -- kept as a separate, optional group
/// of fields (not required by [`WorkspaceAppState::new`]) because *who owns
/// these pools* is a caller decision this crate shouldn't make for them:
///
/// - When wired into the host, `workspace_open_remote` must resolve through
///   the *same* pools the host's SSH panel and AI coding agent already use
///   (`roc_desk_ssh::RocDeskSshAppState`'s), not a second, disconnected set
///   -- otherwise a connection opened in the SSH panel wouldn't be visible
///   here, the exact "split registry" bug this migration has repeatedly had
///   to design around.
/// - A standalone build of this tool has no such existing state to share,
///   and would need to construct its own (which also means it needs its own
///   connection-management UI/commands -- out of scope here, see the module
///   doc's "not ported" list).
///
/// [`WorkspaceAppState::with_ssh`] is how a caller that already has a
/// `RocDeskSshAppState` (or equivalent pools) opts into remote workspace
/// support after construction.
pub struct SshPools {
    pub connection_manager: Arc<roc_desk_ssh::connection::ConnectionManager>,
    pub ssh_pool: Arc<roc_desk_ssh::ssh::SshConnectionPool>,
    pub agent_pool: Arc<roc_desk_ssh::agent::AgentConnectionPool>,
}

/// Shared state for this tool's Tauri commands. Constructed once at startup
/// (see [`WorkspaceAppState::new`]) and registered with `tauri::Builder::manage`.
pub struct WorkspaceAppState {
    pub workspace_manager: WorkspaceManager,
    pub local_pty: pty::SharedLocalPtyManager,
    /// Workspaces currently open in this process, keyed by `WorkspaceProfile.id`.
    /// `workspace_open_local`/`workspace_open_remote` insert into this on
    /// open; `workspace_close` removes.
    pub open_workspaces: Arc<RwLock<HashMap<Uuid, WorkspaceHandle>>>,
    /// `None` until [`WorkspaceAppState::with_ssh`] is called -- remote
    /// workspace commands return a clear "not enabled" error rather than
    /// panicking when this hasn't been wired up.
    pub ssh: Option<SshPools>,
}

impl WorkspaceAppState {
    /// `db_path` is this tool's own SQLite file (the "recent workspaces"
    /// list); `cache_root` is where the fallback `.rock_desk` workspace
    /// metadata cache directory lives (mirrors the host's
    /// `WorkspaceManager::new`). Remote workspace support starts disabled --
    /// see [`WorkspaceAppState::with_ssh`].
    pub fn new(db_path: &std::path::Path, cache_root: PathBuf) -> Result<Self, AppError> {
        let pool = roc_desk_core::db::pool::create_pool(db_path)?;
        let repo = roc_desk_core::workspace::WorkspaceRepo::new(pool);
        let workspace_manager = WorkspaceManager::new(repo, cache_root);
        workspace_manager.ensure_schema()?;
        Ok(Self {
            workspace_manager,
            local_pty: std::sync::Arc::new(pty::LocalPtyManager::default()),
            open_workspaces: Arc::new(RwLock::new(HashMap::new())),
            ssh: None,
        })
    }

    /// Enables remote (SSH/Agent) workspace support using the given
    /// connection pools -- see [`SshPools`]'s doc for why these are supplied
    /// rather than constructed here.
    pub fn with_ssh(
        mut self,
        connection_manager: Arc<roc_desk_ssh::connection::ConnectionManager>,
        ssh_pool: Arc<roc_desk_ssh::ssh::SshConnectionPool>,
        agent_pool: Arc<roc_desk_ssh::agent::AgentConnectionPool>,
    ) -> Self {
        self.ssh = Some(SshPools {
            connection_manager,
            ssh_pool,
            agent_pool,
        });
        self
    }

    /// SSH/Agent are both "remote" connections; which pool/`FileOps` impl to
    /// use depends on the connection profile's `protocol` field. Mirrors the
    /// host's old `workspace::WorkspaceManager::remote_file_ops`.
    async fn remote_file_ops(
        &self,
        connection: &roc_desk_ssh::connection::ConnectionProfile,
    ) -> Result<Arc<dyn FileOps>, AppError> {
        let ssh = self
            .ssh
            .as_ref()
            .ok_or_else(|| AppError::Internal("远程工作区功能未启用".into()))?;
        match connection.protocol {
            roc_desk_ssh::connection::Protocol::Agent => {
                let session = ssh.agent_pool.get_or_connect(connection.id).await?;
                Ok(Arc::new(roc_desk_ssh::agent::fsops::AgentFileOps::new(session)))
            }
            roc_desk_ssh::connection::Protocol::Ssh => {
                let session = ssh.ssh_pool.get_or_connect(connection.id).await?;
                Ok(Arc::new(roc_desk_ssh::fsops::remote::RemoteFileOps::new(session)))
            }
            roc_desk_ssh::connection::Protocol::Rdp => {
                Err(AppError::Internal("RDP 连接不能作为文件工作区".into()))
            }
        }
    }
}

/// The `#[tauri::command]` functions must live in a submodule, not at the
/// crate root -- Tauri's command macro emits a `#[macro_export]` macro_rules
/// item *and* a self-referential `pub use` of the same name in the enclosing
/// scope, which collide at crate root (`E0255`). `standalone/src/main.rs`
/// and the host reference these as `roc_desk_workspace::cmd::pty_open`, etc.
pub mod cmd {
    use tauri::{AppHandle, State};
    use uuid::Uuid;

    use roc_desk_core::error::AppError;
    use roc_desk_core::workspace::WorkspaceProfile;

    use crate::{WorkspaceAppState, WorkspaceHandle};

    // -----------------------------------------------------------------------
    // Workspace (recent-folder tracking)
    // -----------------------------------------------------------------------

    #[tauri::command]
    pub async fn workspace_list_recent(
        state: State<'_, WorkspaceAppState>,
        limit: Option<usize>,
    ) -> Result<Vec<WorkspaceProfile>, AppError> {
        state.workspace_manager.list_recent(limit.unwrap_or(20))
    }

    #[tauri::command]
    pub async fn workspace_open_local(
        state: State<'_, WorkspaceAppState>,
        path: String,
    ) -> Result<WorkspaceProfile, AppError> {
        let profile = state.workspace_manager.open_local(&path)?;
        let handle = WorkspaceHandle {
            profile: profile.clone(),
            file_ops: std::sync::Arc::new(roc_desk_common::fsops::local::LocalFileOps),
        };
        state.open_workspaces.write().await.insert(profile.id, handle);
        Ok(profile)
    }

    /// Connects to `connection_id` (via the SSH/Agent pools passed to
    /// [`WorkspaceAppState::with_ssh`]) and opens `remote_path` on it as a
    /// workspace -- mirrors the host's old `commands::workspace::
    /// workspace_open_remote`. Probes for an embedded `.rock_desk/
    /// workspace.json` marker on the remote host first (so re-opening the
    /// same remote directory, even from a different machine/profile that
    /// shares the directory but not this tool's local database, keeps the
    /// same workspace id), and writes one back after opening (best-effort --
    /// a read-only remote directory shouldn't make opening it as a
    /// *read-only* workspace fail).
    #[tauri::command]
    pub async fn workspace_open_remote(
        state: State<'_, WorkspaceAppState>,
        connection_id: Uuid,
        remote_path: String,
    ) -> Result<WorkspaceProfile, AppError> {
        let ssh = state
            .ssh
            .as_ref()
            .ok_or_else(|| AppError::Internal("远程工作区功能未启用".into()))?;
        let connection = ssh
            .connection_manager
            .get(connection_id)?
            .ok_or_else(|| AppError::NotFound(format!("connection not found: {connection_id}")))?;

        let file_ops = state.remote_file_ops(&connection).await?;

        let metadata_path = format!("{}/.rock_desk/workspace.json", remote_path.trim_end_matches('/'));
        let embedded_workspace_id = file_ops
            .read_file(&metadata_path)
            .await
            .ok()
            .and_then(|file| serde_json::from_str::<serde_json::Value>(&file.text).ok())
            .filter(|meta| {
                meta["kind"].as_str() == Some("remote") && meta["root_path"].as_str() == Some(remote_path.as_str())
            })
            .and_then(|meta| meta["workspace_id"].as_str().and_then(|s| Uuid::parse_str(s).ok()));

        let display_name = format!(
            "{} ({}@{})",
            remote_path.trim_end_matches('/').rsplit('/').next().unwrap_or(&remote_path),
            connection.username,
            connection.host
        );

        let profile = state
            .workspace_manager
            .open_remote(connection_id, &remote_path, display_name, embedded_workspace_id)?;

        // Best-effort: a read-only remote directory shouldn't make opening it
        // fail, it just won't carry the "re-open keeps the same id" marker.
        let metadata_dir = format!("{}/.rock_desk", remote_path.trim_end_matches('/'));
        if file_ops.create_dir(&metadata_dir).await.is_ok() {
            let metadata_json = serde_json::json!({
                "workspace_id": profile.id,
                "kind": "remote",
                "root_path": profile.root_path,
                "connection_id": connection_id,
            });
            let _ = file_ops
                .write_file(&format!("{metadata_dir}/workspace.json"), &metadata_json.to_string(), None)
                .await;
        }

        let handle = WorkspaceHandle {
            profile: profile.clone(),
            file_ops,
        };
        state.open_workspaces.write().await.insert(profile.id, handle);
        Ok(profile)
    }

    /// Drops a workspace's runtime handle (does *not* touch its "recent
    /// workspaces" entry -- that's `workspace_remove_recent`'s job). Call
    /// when the frontend closes a workspace tab/window so `open_workspaces`
    /// doesn't grow unbounded across a long-running session.
    #[tauri::command]
    pub async fn workspace_close(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
    ) -> Result<(), AppError> {
        state.open_workspaces.write().await.remove(&id);
        Ok(())
    }

    #[tauri::command]
    pub async fn workspace_remove_recent(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
    ) -> Result<(), AppError> {
        state.workspace_manager.remove_from_recent(id)
    }

    #[tauri::command]
    pub async fn workspace_update_path(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
        new_path: String,
    ) -> Result<WorkspaceProfile, AppError> {
        state.workspace_manager.update_path(id, &new_path)
    }

    #[tauri::command]
    pub async fn workspace_update_last_sftp_paths(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
        local_path: String,
        remote_path: String,
    ) -> Result<(), AppError> {
        state
            .workspace_manager
            .update_last_sftp_paths(id, &local_path, &remote_path)
    }

    // -----------------------------------------------------------------------
    // Local terminal
    // -----------------------------------------------------------------------

    #[tauri::command]
    pub async fn pty_open(
        state: State<'_, WorkspaceAppState>,
        app_handle: AppHandle,
        cwd: String,
        rows: u16,
        cols: u16,
    ) -> Result<Uuid, AppError> {
        state.local_pty.open(cwd, rows, cols, app_handle).await
    }

    #[tauri::command]
    pub async fn pty_write(
        state: State<'_, WorkspaceAppState>,
        channel_id: Uuid,
        data: Vec<u8>,
    ) -> Result<(), AppError> {
        state.local_pty.write(channel_id, data).await
    }

    #[tauri::command]
    pub async fn pty_resize(
        state: State<'_, WorkspaceAppState>,
        channel_id: Uuid,
        rows: u16,
        cols: u16,
    ) -> Result<(), AppError> {
        state.local_pty.resize(channel_id, rows, cols).await
    }

    #[tauri::command]
    pub async fn pty_close(
        state: State<'_, WorkspaceAppState>,
        channel_id: Uuid,
    ) -> Result<(), AppError> {
        state.local_pty.close(channel_id).await
    }

    // -----------------------------------------------------------------------
    // Git panel (local-only)
    // -----------------------------------------------------------------------

    #[tauri::command]
    pub async fn git_is_repo(cwd: String) -> bool {
        crate::git::is_git_repo(&cwd).await
    }

    #[tauri::command]
    pub async fn git_status(cwd: String, path: Option<String>) -> Result<String, AppError> {
        crate::git::status(&cwd, path.as_deref()).await
    }

    #[tauri::command]
    pub async fn git_diff(cwd: String, path: Option<String>) -> Result<String, AppError> {
        crate::git::diff(&cwd, path.as_deref()).await
    }

    #[tauri::command]
    pub async fn git_log(cwd: String, limit: Option<u32>) -> Result<String, AppError> {
        crate::git::log(&cwd, limit.unwrap_or(50)).await
    }

    #[tauri::command]
    pub async fn git_current_branch(cwd: String) -> Result<String, AppError> {
        crate::git::current_branch(&cwd).await
    }

    #[tauri::command]
    pub async fn git_commit_file(
        cwd: String,
        path: String,
        message: String,
    ) -> Result<String, AppError> {
        crate::git::commit_file(&cwd, &path, &message).await
    }

    #[tauri::command]
    pub async fn git_commit_paths(
        cwd: String,
        paths: Vec<String>,
        message: String,
    ) -> Result<String, AppError> {
        crate::git::commit_paths(&cwd, &paths, &message).await
    }
}
