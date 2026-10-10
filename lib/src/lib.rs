//! Coding workspace: local terminal, Git panel, workspace/recent-folder
//! tracking, and a full AI coding agent (multi-turn tool-calling loop,
//! file-change staging/undo, permission rules, MCP servers, conversation
//! history).
//!
//! ## Migration status (see the repository README / host's
//! `docs/MULTI_REPO_SPLIT_PROGRESS.md` "编程工作区"/"AI 编程助手迁移"
//! sections for the full writeup)
//!
//! Ported and command-tested via `cargo check`/`cargo test`:
//! - Workspace concept (open a local or remote folder, remember it in a
//!   "recent" list) -- backed by `roc_desk_core::workspace`.
//! - Local terminal (`pty`), ported 1:1 from the host.
//! - A local-only Git panel backend (`git`): status/diff/log/commit,
//!   talking to the `git` binary directly via argv.
//! - Local filesystem browsing/editing + a root-path-keyed symbol index +
//!   an embeddable `<EditorPane/>` are **not reimplemented here** -- they
//!   come from depending on `roc_desk-editor` (which itself re-exports
//!   `roc_desk-explorer`). `standalone/src/main.rs` registers
//!   `roc_desk_explorer::cmd::*` and `roc_desk_editor::symbols::editor_symbols_*`
//!   directly alongside this crate's own commands.
//! - The AI coding agent (`coding::session::CodingSession`'s multi-turn
//!   tool-calling loop, all ~24 tools, permission-gated command execution,
//!   file-change Diff/Accept/Undo/Redo staging, MCP server management,
//!   Skills import, conversation history persisted both to this tool's own
//!   SQLite file and as a workspace-portable `.rock_desk/sessions/*.json`
//!   mirror) -- the full `commands/coding.rs` command surface is wired up
//!   in [`cmd`], backed by [`WorkspaceAppState`]'s `coding_*`/`ai_*`/
//!   `mcp_manager`/`permission_rules` fields. Requires
//!   [`WorkspaceAppState::with_ssh`] to have been called even for a purely
//!   local coding session -- see that field's doc comment for why.
//!
//! **Not ported** (left as host-only, not stubbed with fake implementations):
//! - Remote *terminal sessions* (`pty_*` only ever spawns a local shell) and
//!   the Git panel (`git_*` only ever shells out locally) are still
//!   local-only, independent of the AI coding agent being able to target a
//!   remote/Agent workspace; those would need their own remote execution
//!   path (`roc_desk_ssh::ssh::session`/`agent::session` exec), not
//!   attempted here.

pub const TOOL_NAME: &str = "roc_desk-workspace";
pub const TOOL_DESCRIPTION: &str = "编程工作区：代码、终端与 Git";

pub use roc_desk_core::connection::{ConnectionKind, ConnectionProfile};

pub fn tool_info() -> (&'static str, &'static str) {
    (TOOL_NAME, TOOL_DESCRIPTION)
}

pub mod coding;
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

use tokio::sync::{Mutex, RwLock};
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
    /// Best-effort local mirror directory for data that's primarily stored
    /// in the workspace itself (e.g. `coding::history`'s workspace-portable
    /// session snapshots) -- `WorkspaceManager::cache_root().join(profile.id)`,
    /// the same per-workspace fallback directory `WorkspaceManager` itself
    /// uses for its own `workspace.json` fallback copy.
    pub fallback_cache_dir: PathBuf,
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
    /// panicking when this hasn't been wired up. The AI coding agent's
    /// command layer also requires this to be set even for a purely local
    /// session -- `CodingSession::send_message`'s signature always takes
    /// concrete `SshConnectionPool`/`AgentConnectionPool` references (unused
    /// on the `CodingTarget::Local` path, but still part of the call), so
    /// there is no meaningful way to call it without *some* pools to pass,
    /// and this crate has no way to construct placeholder ones of its own
    /// (see [`SshPools`]'s doc for why constructing them here would be
    /// wrong regardless).
    pub ssh: Option<SshPools>,
    /// AI coding agent session, keyed by workspace id -- at most one active
    /// session per workspace.
    pub coding_sessions: Arc<RwLock<HashMap<Uuid, Arc<Mutex<coding::CodingSession>>>>>,
    /// File-change (Diff/Accept/Undo/Redo) state, keyed by workspace id same
    /// as `coding_sessions` but deliberately a separate lock -- so that
    /// accepting/rejecting one file change never has to wait behind a
    /// possibly minutes-long AI conversation turn holding `coding_sessions`'s
    /// lock. See `coding::changes::ChangeStore`'s doc comment.
    pub coding_changes: Arc<RwLock<HashMap<Uuid, Arc<Mutex<coding::ChangeStore>>>>>,
    pub coding_history: Arc<coding::history::CodingHistoryRepo>,
    pub ai_evidence: Arc<coding::evidence::AiEvidenceRepo>,
    pub audit_log: Arc<coding::audit::AuditLogRepo>,
    pub permission_rules: Arc<coding::permission::PermissionRulesRepo>,
    pub mcp_manager: Arc<coding::mcp::McpServerManager>,
    pub ai_provider_manager: Arc<roc_desk_common::ai::AiProviderManager>,
    pub command_confirms: roc_desk_common::agent_confirm::CommandConfirmRegistry,
    pub question_confirms: roc_desk_common::agent_confirm::QuestionRegistry,
    /// "Stop" button cancellation signal, keyed by workspace id -- lives
    /// independent of `coding_sessions`'s lock for the same reason as the
    /// host's `AppState.coding_cancel_tokens`: `coding_send_message` holds
    /// that lock for the whole turn, so a cancel command sharing it would
    /// be stuck behind the very thing it's trying to interrupt.
    pub coding_cancel_tokens:
        Arc<std::sync::Mutex<HashMap<Uuid, tokio_util::sync::CancellationToken>>>,
    pub coding_pending_injections:
        Arc<std::sync::Mutex<HashMap<Uuid, Vec<coding::PendingInjection>>>>,
    pub symbol_indexes: Arc<RwLock<HashMap<Uuid, roc_desk_common::symbols::SymbolIndex>>>,
}

impl WorkspaceAppState {
    /// `db_path` is this tool's own SQLite file for the AI-coding-agent
    /// tables that are genuinely this crate's own (coding history/MCP
    /// servers/permission rules/audit log/evidence cache -- schemas ported
    /// from, but not literally shared code with, the host's own original
    /// implementations of these, so pointing this at the host's `roc_desk.db`
    /// would risk silent schema drift between two independently-maintained
    /// copies).
    ///
    /// `ai_providers_db_path` is separate: the host has its own
    /// `db::repo::ai_providers_repo::AiProvidersRepo` (a different type
    /// than this crate's `roc_desk_common::ai::AiProvidersRepo`), but its
    /// `ai_providers` table schema is kept column-for-column identical on
    /// purpose (verified against host's `migrations/0007_ai_providers.sql`
    /// + `0018`/`0019`/`0024`), so this one is safe to point at the host's
    /// `roc_desk.db` and get a real shared provider list, unlike the other
    /// tables above.
    ///
    /// `workspace_db_path` is also separate: just the "recent workspaces"
    /// list, safe to point at the host's `workspaces/workspaces.db` for the
    /// same reason (`roc_desk_core::workspace::WorkspaceRepo` genuinely is
    /// the same type/schema the host's own `WorkspaceManager` uses).
    ///
    /// Passing the same path for all three is fine too (every caller did
    /// that before this split) -- every `ensure_schema` here uses
    /// `CREATE TABLE IF NOT EXISTS`, so sharing one file never collides.
    ///
    /// `cache_root` is where the fallback `.rock_desk` workspace metadata
    /// cache directory lives (mirrors the host's `WorkspaceManager::new`).
    /// Remote workspace support starts disabled -- see
    /// [`WorkspaceAppState::with_ssh`].
    pub fn new(
        db_path: &std::path::Path,
        ai_providers_db_path: &std::path::Path,
        workspace_db_path: &std::path::Path,
        cache_root: PathBuf,
    ) -> Result<Self, AppError> {
        let pool = roc_desk_core::db::pool::create_pool(db_path)?;
        let workspace_pool = if workspace_db_path == db_path {
            pool.clone()
        } else {
            roc_desk_core::db::pool::create_pool(workspace_db_path)?
        };
        let repo = roc_desk_core::workspace::WorkspaceRepo::new(workspace_pool);
        let workspace_manager = WorkspaceManager::new(repo, cache_root);
        workspace_manager.ensure_schema()?;

        let credential_store: Arc<dyn roc_desk_core::credential::CredentialStore> =
            Arc::new(roc_desk_core::credential::KeyringStore);

        let ai_providers_pool = if ai_providers_db_path == db_path {
            pool.clone()
        } else {
            roc_desk_core::db::pool::create_pool(ai_providers_db_path)?
        };
        let ai_providers_repo = Arc::new(roc_desk_common::ai::AiProvidersRepo::new(ai_providers_pool));
        ai_providers_repo.ensure_schema()?;
        let ai_provider_manager = Arc::new(roc_desk_common::ai::AiProviderManager::new(
            ai_providers_repo,
            credential_store.clone(),
        ));

        let mcp_servers_repo = Arc::new(coding::mcp::McpServersRepo::new(pool.clone()));
        mcp_servers_repo.ensure_schema()?;
        let mcp_manager = Arc::new(coding::mcp::McpServerManager::new(
            mcp_servers_repo,
            credential_store,
        ));

        let permission_rules = Arc::new(coding::permission::PermissionRulesRepo::new(pool.clone()));
        permission_rules.ensure_schema()?;

        let audit_log = Arc::new(coding::audit::AuditLogRepo::new(pool.clone()));
        audit_log.ensure_schema()?;

        let ai_evidence = Arc::new(coding::evidence::AiEvidenceRepo::new(pool.clone()));
        ai_evidence.ensure_schema()?;

        let coding_history = Arc::new(coding::history::CodingHistoryRepo::new(pool));
        coding_history.ensure_schema()?;

        Ok(Self {
            workspace_manager,
            local_pty: std::sync::Arc::new(pty::LocalPtyManager::default()),
            open_workspaces: Arc::new(RwLock::new(HashMap::new())),
            ssh: None,
            coding_sessions: Arc::new(RwLock::new(HashMap::new())),
            coding_changes: Arc::new(RwLock::new(HashMap::new())),
            coding_history,
            ai_evidence,
            audit_log,
            permission_rules,
            mcp_manager,
            ai_provider_manager,
            command_confirms: roc_desk_common::agent_confirm::CommandConfirmRegistry::default(),
            question_confirms: roc_desk_common::agent_confirm::QuestionRegistry::default(),
            coding_cancel_tokens: Arc::new(std::sync::Mutex::new(HashMap::new())),
            coding_pending_injections: Arc::new(std::sync::Mutex::new(HashMap::new())),
            symbol_indexes: Arc::new(RwLock::new(HashMap::new())),
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
    use std::sync::Arc;

    use tauri::{AppHandle, Emitter, State};
    use tokio::sync::Mutex;
    use uuid::Uuid;

    use roc_desk_core::error::AppError;
    use roc_desk_core::workspace::WorkspaceProfile;

    use roc_desk_common::change_store::{ChangeStatus, CodingTarget, FileChange, FileSyncInfo};
    use crate::coding::commands::{
        build_new_session, clear_probe_cache, get_change_store, get_session,
        history_list_with_import, load_history_snapshot, maybe_auto_continue, session_info,
        CodingHistoryDetail, CodingSessionInfo, TempDirGuard,
    };
    use crate::coding::history::{
        CodingHistoryInput, CodingHistorySummary, WorkspaceHistorySnapshot,
    };
    use crate::coding::mcp::{McpServer, McpServerInput};
    use crate::coding::permission::{Decision, PermissionRule};
    use crate::coding::session::{CodingMode, PendingInjection};
    use crate::coding::skills::SkillMeta;
    use roc_desk_common::ai::attachments::ChatAttachment;
    use roc_desk_common::ai::{AiProvider, AiProviderInput};
    use roc_desk_common::fsops::FileOps;

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
            fallback_cache_dir: state
                .workspace_manager
                .cache_root()
                .join(profile.id.to_string()),
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
            fallback_cache_dir: state
                .workspace_manager
                .cache_root()
                .join(profile.id.to_string()),
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
    // Workspace-scoped filesystem (`fs_*`) -- the counterpart to
    // `roc_desk_explorer::cmd::local_*` for a workspace opened via
    // `workspace_open_local`/`workspace_open_remote`: every command here
    // goes through `WorkspaceHandle.file_ops` (already Local/Remote/Agent
    // dispatching, same trait object `ChangeStore` uses) instead of a
    // hard-coded `LocalFileOps`, and is boundary-checked for local
    // workspaces via `guard_local_path` (mirrors the host's old
    // `commands/fs.rs`; remote boundary-checking is deferred to the
    // `FileOps` impl itself, same note as the host's version). This is what
    // lets `EditorPane`/`ExplorerTree` work against a workspace id instead
    // of a bare local root path -- the one thing standalone was missing for
    // genuine remote-workspace editing (AI-agent edits already went through
    // `ChangeStore`/`FileOps` and didn't need this).
    use base64::Engine;
    use roc_desk_common::binary_info::{self, BinaryInfo};
    use roc_desk_common::fsops::{BINARY_PREVIEW_MAX_BYTES, EXECUTABLE_INSPECT_MAX_BYTES};
    use roc_desk_common::jar_info::{self, JarInfo};
    use roc_desk_common::{encoding, office_convert};
    use roc_desk_core::workspace::WorkspaceKind;

    async fn get_fs_handle(
        state: &State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
    ) -> Result<WorkspaceHandle, AppError> {
        state
            .open_workspaces
            .read()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("工作区未打开: {workspace_id}")))
    }

    fn guard_local_path(handle: &WorkspaceHandle, path: &str) -> Result<(), AppError> {
        if handle.profile.kind != WorkspaceKind::Local {
            return Ok(());
        }
        let root = std::path::Path::new(&handle.profile.root_path);
        let root_canon = root.canonicalize().map_err(AppError::from)?;

        let candidate = std::path::PathBuf::from(path);
        let candidate_canon = match candidate.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                let parent = candidate
                    .parent()
                    .ok_or_else(|| AppError::PermissionDenied(format!("非法路径: {path}")))?;
                let parent_canon = parent.canonicalize().map_err(|_| {
                    AppError::PermissionDenied(format!("路径 {path} 不在工作区范围内"))
                })?;
                parent_canon.join(candidate.file_name().unwrap_or_default())
            }
        };

        if !candidate_canon.starts_with(&root_canon) {
            return Err(AppError::PermissionDenied(format!(
                "路径 {path} 不在工作区 {} 范围内",
                handle.profile.root_path
            )));
        }
        Ok(())
    }

    fn emit_fs_changed(app_handle: &AppHandle, workspace_id: Uuid) {
        let _ = app_handle.emit(
            "fs:changed",
            serde_json::json!({ "workspaceId": workspace_id }),
        );
    }

    #[tauri::command]
    pub async fn fs_list_dir(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        path: String,
    ) -> Result<Vec<roc_desk_common::fsops::FileEntry>, AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;
        handle.file_ops.list_dir(&path).await
    }

    #[tauri::command]
    pub async fn fs_read_file(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        path: String,
    ) -> Result<roc_desk_common::fsops::FileContent, AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;
        handle.file_ops.read_file_for_editor(&path).await
    }

    #[tauri::command]
    pub async fn fs_write_file(
        state: State<'_, WorkspaceAppState>,
        app_handle: AppHandle,
        workspace_id: Uuid,
        path: String,
        content: String,
        expected_mtime: Option<i64>,
    ) -> Result<roc_desk_common::fsops::WriteOutcome, AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;
        let outcome = handle
            .file_ops
            .write_file(&path, &content, expected_mtime)
            .await?;
        if matches!(outcome, roc_desk_common::fsops::WriteOutcome::Written { .. }) {
            emit_fs_changed(&app_handle, workspace_id);
        }
        Ok(outcome)
    }

    #[tauri::command]
    pub async fn fs_read_file_with_encoding(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        path: String,
        encoding_label: String,
    ) -> Result<roc_desk_common::fsops::FileContent, AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;
        let (bytes, mtime, total_size, truncated) = handle.file_ops.read_bytes_for_editor(&path).await?;
        let text = encoding::decode_with(&bytes, &encoding_label).map_err(AppError::Internal)?;
        Ok(roc_desk_common::fsops::FileContent { text, encoding: encoding_label, mtime, total_size, truncated })
    }

    #[tauri::command]
    pub async fn fs_write_file_with_encoding(
        state: State<'_, WorkspaceAppState>,
        app_handle: AppHandle,
        workspace_id: Uuid,
        path: String,
        content: String,
        encoding_label: String,
        expected_mtime: Option<i64>,
    ) -> Result<roc_desk_common::fsops::WriteOutcome, AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;
        let bytes = encoding::encode_with(&content, &encoding_label).map_err(AppError::Internal)?;
        let outcome = handle
            .file_ops
            .write_file_bytes(&path, &bytes, expected_mtime)
            .await?;
        if matches!(outcome, roc_desk_common::fsops::WriteOutcome::Written { .. }) {
            emit_fs_changed(&app_handle, workspace_id);
        }
        Ok(outcome)
    }

    #[tauri::command]
    pub fn fs_supported_encodings() -> Vec<&'static str> {
        encoding::SUPPORTED_ENCODINGS.to_vec()
    }

    #[tauri::command]
    pub async fn fs_read_binary_preview(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        path: String,
    ) -> Result<String, AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;
        let bytes = handle.file_ops.read_binary_for_preview(&path, BINARY_PREVIEW_MAX_BYTES).await?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    /// "用系统默认程序打开"：本地工作区直接开原路径；远程工作区先下载到本地临时
    /// 目录再开（系统程序不认识 SSH/Agent 路径）。复用 `roc_desk-explorer` 的
    /// `open_path_or_launch_exe`（可执行文件单独 spawn 并设置工作目录，其余交给
    /// Tauri opener 插件）而不是自己再写一份。
    #[tauri::command]
    pub async fn fs_open_externally(
        app_handle: AppHandle,
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        path: String,
    ) -> Result<(), AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;

        let target = if handle.profile.kind == WorkspaceKind::Local {
            path.clone()
        } else {
            let file_name = path.rsplit('/').next().unwrap_or(&path);
            let tmp_dir = std::env::temp_dir().join("roc_desk_open");
            std::fs::create_dir_all(&tmp_dir)?;
            let local_path = tmp_dir.join(file_name);
            handle
                .file_ops
                .download_to_local_file(&path, &local_path.to_string_lossy())
                .await?;
            local_path.to_string_lossy().to_string()
        };

        crate::roc_desk_explorer::cmd::open_path_or_launch_exe(&app_handle, &target)
    }

    #[tauri::command]
    pub async fn fs_convert_legacy_office_to_pdf(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        path: String,
    ) -> Result<String, AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;

        let tmp_dir = std::env::temp_dir().join("roc_desk_office_convert");
        let source_path = if handle.profile.kind == WorkspaceKind::Local {
            std::path::PathBuf::from(&path)
        } else {
            std::fs::create_dir_all(&tmp_dir)?;
            let file_name = path.rsplit('/').next().unwrap_or(&path);
            let local_path = tmp_dir.join(file_name);
            handle
                .file_ops
                .download_to_local_file(&path, &local_path.to_string_lossy())
                .await?;
            local_path
        };

        let pdf_path = office_convert::convert_to_pdf(&source_path, &tmp_dir).await?;
        let bytes = tokio::fs::read(&pdf_path).await.map_err(AppError::from)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    #[tauri::command]
    pub async fn fs_inspect_binary(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        path: String,
    ) -> Result<BinaryInfo, AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;
        let bytes = handle.file_ops.read_binary_for_preview(&path, EXECUTABLE_INSPECT_MAX_BYTES).await?;
        binary_info::inspect(&bytes)
    }

    #[tauri::command]
    pub async fn fs_peek_is_binary(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        path: String,
    ) -> Result<bool, AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;
        let (head, _mtime) = handle.file_ops.read_file_raw_bounded(&path, 64).await?;
        Ok(binary_info::looks_like_binary(&head))
    }

    #[tauri::command]
    pub async fn fs_inspect_jar(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        path: String,
    ) -> Result<JarInfo, AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;
        let bytes = handle.file_ops.read_binary_for_preview(&path, EXECUTABLE_INSPECT_MAX_BYTES).await?;
        jar_info::inspect(&bytes)
    }

    #[tauri::command]
    pub async fn fs_delete(
        state: State<'_, WorkspaceAppState>,
        app_handle: AppHandle,
        workspace_id: Uuid,
        path: String,
        is_dir: bool,
    ) -> Result<(), AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;
        handle.file_ops.delete(&path, is_dir).await?;
        emit_fs_changed(&app_handle, workspace_id);
        Ok(())
    }

    #[tauri::command]
    pub async fn fs_rename(
        state: State<'_, WorkspaceAppState>,
        app_handle: AppHandle,
        workspace_id: Uuid,
        from: String,
        to: String,
    ) -> Result<(), AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &from)?;
        guard_local_path(&handle, &to)?;
        handle.file_ops.rename(&from, &to).await?;
        emit_fs_changed(&app_handle, workspace_id);
        Ok(())
    }

    #[tauri::command]
    pub async fn fs_copy(
        state: State<'_, WorkspaceAppState>,
        app_handle: AppHandle,
        workspace_id: Uuid,
        from: String,
        to: String,
        is_dir: bool,
    ) -> Result<(), AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &from)?;
        guard_local_path(&handle, &to)?;
        handle.file_ops.copy(&from, &to, is_dir).await?;
        emit_fs_changed(&app_handle, workspace_id);
        Ok(())
    }

    #[tauri::command]
    pub async fn fs_create_dir(
        state: State<'_, WorkspaceAppState>,
        app_handle: AppHandle,
        workspace_id: Uuid,
        path: String,
    ) -> Result<(), AppError> {
        let handle = get_fs_handle(&state, workspace_id).await?;
        guard_local_path(&handle, &path)?;
        handle.file_ops.create_dir(&path).await?;
        emit_fs_changed(&app_handle, workspace_id);
        Ok(())
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

    // -----------------------------------------------------------------------
    // AI provider management -- not part of the host's `commands/coding.rs`
    // (it lives in the host's separate `commands/ai.rs`, shared by the
    // coding agent and the general-purpose AI chat panel), but the coding
    // agent has no usable session without at least one configured provider,
    // so this tool needs its own copy of the same thin CRUD wrappers.
    // -----------------------------------------------------------------------

    #[tauri::command]
    pub async fn ai_provider_list(
        state: State<'_, WorkspaceAppState>,
    ) -> Result<Vec<AiProvider>, AppError> {
        state.ai_provider_manager.list()
    }

    #[tauri::command]
    pub async fn ai_provider_create(
        state: State<'_, WorkspaceAppState>,
        input: AiProviderInput,
    ) -> Result<AiProvider, AppError> {
        state.ai_provider_manager.create(input).await
    }

    #[tauri::command]
    pub async fn ai_provider_update(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
        input: AiProviderInput,
    ) -> Result<AiProvider, AppError> {
        state.ai_provider_manager.update(id, input).await
    }

    #[tauri::command]
    pub async fn ai_provider_delete(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
    ) -> Result<(), AppError> {
        state.ai_provider_manager.delete(id).await
    }

    #[tauri::command]
    pub async fn ai_provider_list_models(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
    ) -> Result<Vec<String>, AppError> {
        state.ai_provider_manager.list_models(id).await
    }

    // -----------------------------------------------------------------------
    // AI coding agent -- session lifecycle
    // -----------------------------------------------------------------------

    fn require_ssh(state: &State<'_, WorkspaceAppState>) -> Result<(), AppError> {
        if state.ssh.is_none() {
            return Err(AppError::Internal(
                "AI 编程助手功能未启用（远程连接池未配置）".into(),
            ));
        }
        Ok(())
    }

    #[tauri::command]
    pub async fn coding_set_provider(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        provider_id: Uuid,
    ) -> Result<(), AppError> {
        if state.ai_provider_manager.get(provider_id)?.is_none() {
            return Err(AppError::NotFound(format!(
                "ai provider not found: {provider_id}"
            )));
        }
        let session = get_session(&state, workspace_id).await?;
        let mut guard = session.lock().await;
        guard.provider_id = provider_id;
        Ok(())
    }

    /// Auto-binds (or reuses an existing) coding agent session to the
    /// current workspace.
    #[tauri::command]
    pub async fn coding_start(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        provider_id: Uuid,
    ) -> Result<CodingSessionInfo, AppError> {
        require_ssh(&state)?;
        if state.ai_provider_manager.get(provider_id)?.is_none() {
            return Err(AppError::NotFound(format!(
                "ai provider not found: {provider_id}"
            )));
        }

        let workspaces = state.open_workspaces.read().await;
        let handle = workspaces
            .get(&workspace_id)
            .ok_or_else(|| AppError::NotFound(format!("workspace not opened: {workspace_id}")))?;
        let profile = handle.profile.clone();
        drop(workspaces);

        if let Some(existing) = state
            .coding_sessions
            .read()
            .await
            .get(&workspace_id)
            .cloned()
        {
            let guard = existing.lock().await;
            let target_matches = match (&guard.target, profile.kind, profile.connection_id) {
                (CodingTarget::Local, roc_desk_core::workspace::WorkspaceKind::Local, None) => true,
                (
                    CodingTarget::Remote { connection_id, .. },
                    roc_desk_core::workspace::WorkspaceKind::Remote,
                    Some(expected),
                ) => *connection_id == expected,
                (
                    CodingTarget::Agent { connection_id, .. },
                    roc_desk_core::workspace::WorkspaceKind::Remote,
                    Some(expected),
                ) => *connection_id == expected,
                _ => false,
            };
            if target_matches && guard.workspace_root == profile.root_path {
                if state.ai_provider_manager.get(guard.provider_id)?.is_some() {
                    return Ok(session_info(&guard).await);
                }
                drop(guard);
                let mut guard = existing.lock().await;
                guard.provider_id = provider_id;
                return Ok(session_info(&guard).await);
            }
            drop(guard);
            state.coding_sessions.write().await.remove(&workspace_id);
        }

        let (session, change_store) =
            build_new_session(&state, workspace_id, provider_id, true, None).await?;
        let info = session_info(&session).await;
        state
            .coding_sessions
            .write()
            .await
            .insert(workspace_id, Arc::new(Mutex::new(session)));
        state
            .coding_changes
            .write()
            .await
            .insert(workspace_id, change_store);
        Ok(info)
    }

    #[tauri::command]
    pub async fn coding_new_session(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        provider_id: Uuid,
    ) -> Result<CodingSessionInfo, AppError> {
        require_ssh(&state)?;
        if state.ai_provider_manager.get(provider_id)?.is_none() {
            return Err(AppError::NotFound(format!(
                "ai provider not found: {provider_id}"
            )));
        }
        state.coding_sessions.write().await.remove(&workspace_id);
        state.coding_changes.write().await.remove(&workspace_id);
        state
            .coding_pending_injections
            .lock()
            .unwrap()
            .remove(&workspace_id);
        let (session, change_store) =
            build_new_session(&state, workspace_id, provider_id, false, None).await?;
        let info = session_info(&session).await;
        state
            .coding_sessions
            .write()
            .await
            .insert(workspace_id, Arc::new(Mutex::new(session)));
        state
            .coding_changes
            .write()
            .await
            .insert(workspace_id, change_store);
        Ok(info)
    }

    /// Releases a workspace's resident coding agent session (frontend's
    /// bounded-LRU eviction, or when the workspace itself is closed).
    /// Session memory is simply dropped -- conversation content is already
    /// persisted after every `sendMessage`/`acceptChange` etc. via
    /// `coding_history_save`, no "save before closing" step needed here.
    #[tauri::command]
    pub async fn coding_close(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
    ) -> Result<(), AppError> {
        state.coding_sessions.write().await.remove(&workspace_id);
        state.coding_changes.write().await.remove(&workspace_id);
        state
            .coding_pending_injections
            .lock()
            .unwrap()
            .remove(&workspace_id);
        Ok(())
    }

    #[tauri::command]
    pub async fn coding_set_mode(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        mode: CodingMode,
    ) -> Result<(), AppError> {
        let session = get_session(&state, workspace_id).await?;
        let mut session = session.lock().await;
        session.mode = mode;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // AI coding agent -- per-session toggles (own `ChangeStore` lock, never
    // the `CodingSession` lock `send_message` may hold for a long turn)
    // -----------------------------------------------------------------------

    #[tauri::command]
    pub async fn coding_set_auto_allow_readonly(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        enabled: bool,
    ) -> Result<(), AppError> {
        let store = get_change_store(&state, workspace_id).await?;
        store
            .lock()
            .await
            .auto_allow_readonly
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    #[tauri::command]
    pub async fn coding_set_auto_git_commit(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        enabled: bool,
    ) -> Result<(), AppError> {
        let ssh = state
            .ssh
            .as_ref()
            .ok_or_else(|| AppError::Internal("远程工作区功能未启用".into()))?;
        let ssh_pool = ssh.ssh_pool.clone();
        let agent_pool = ssh.agent_pool.clone();
        let store = get_change_store(&state, workspace_id).await?;
        {
            let mut guard = store.lock().await;
            if enabled && !guard.git_repo() {
                let target = guard.target().clone();
                let workspace_root = guard.workspace_root().to_string();
                drop(guard);
                let git_repo = tokio::time::timeout(
                    std::time::Duration::from_secs(15),
                    crate::coding::git_ops::is_git_repo(
                        &target,
                        &workspace_root,
                        &ssh_pool,
                        &agent_pool,
                    ),
                )
                .await
                .unwrap_or(false);
                if !git_repo {
                    return Err(AppError::Internal(
                        "当前工作区不是 Git 仓库，无法开启自动提交".to_string(),
                    ));
                }
                guard = store.lock().await;
                guard.set_git_repo(true);
            }
            guard.auto_git_commit = enabled;
        }
        Ok(())
    }

    #[tauri::command]
    pub async fn coding_set_full_auto(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        enabled: bool,
    ) -> Result<(), AppError> {
        let store = get_change_store(&state, workspace_id).await?;
        let session_id = {
            let store = store.lock().await;
            store
                .full_auto
                .store(enabled, std::sync::atomic::Ordering::Relaxed);
            store.session_id()
        };
        if enabled {
            state.command_confirms.allow_session(session_id).await;
        }
        Ok(())
    }

    #[tauri::command]
    pub async fn coding_set_auto_apply_changes(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        enabled: bool,
    ) -> Result<(), AppError> {
        let store = get_change_store(&state, workspace_id).await?;
        store
            .lock()
            .await
            .auto_apply_changes
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // AI coding agent -- conversation
    // -----------------------------------------------------------------------

    #[tauri::command]
    pub async fn coding_send_message(
        state: State<'_, WorkspaceAppState>,
        app_handle: AppHandle,
        workspace_id: Uuid,
        text: String,
        attachments: Option<Vec<ChatAttachment>>,
    ) -> Result<String, AppError> {
        let session = get_session(&state, workspace_id).await?;
        let ssh = state
            .ssh
            .as_ref()
            .ok_or_else(|| AppError::Internal("远程工作区功能未启用".into()))?;
        let ssh_pool = ssh.ssh_pool.clone();
        let agent_pool = ssh.agent_pool.clone();

        let cancel_token = tokio_util::sync::CancellationToken::new();
        state
            .coding_cancel_tokens
            .lock()
            .unwrap()
            .insert(workspace_id, cancel_token.clone());
        let mut session = session.lock().await;
        let result = session
            .send_message(
                &text,
                &attachments.unwrap_or_default(),
                &state.ai_provider_manager,
                &ssh_pool,
                &agent_pool,
                &state.audit_log,
                &state.command_confirms,
                &state.permission_rules,
                &state.question_confirms,
                &state.mcp_manager,
                &app_handle,
                &cancel_token,
                &state.coding_pending_injections,
                &state.symbol_indexes,
            )
            .await;
        state
            .coding_cancel_tokens
            .lock()
            .unwrap()
            .remove(&workspace_id);
        result
    }

    #[tauri::command]
    pub async fn coding_cancel_turn(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
    ) -> Result<(), AppError> {
        if let Some(token) = state.coding_cancel_tokens.lock().unwrap().get(&workspace_id) {
            token.cancel();
        }
        Ok(())
    }

    #[tauri::command]
    pub async fn coding_inject_message(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        text: String,
        attachments: Option<Vec<ChatAttachment>>,
    ) -> Result<(), AppError> {
        let attachments = attachments.unwrap_or_default();
        if text.trim().is_empty() && attachments.is_empty() {
            return Ok(());
        }
        state
            .coding_pending_injections
            .lock()
            .unwrap()
            .entry(workspace_id)
            .or_default()
            .push(PendingInjection { text, attachments });
        Ok(())
    }

    const OPTIMIZE_PROMPT_SYSTEM: &str =
        "你是一个提示词优化助手，任务是把用户写给 AI 编程助手的草稿指令改写得更清晰、具体、可执行。\
         要求：1) 保留用户的原始意图，不要编造用户没提到的具体文件名/路径/技术选型等事实性细节；\
         2) 把模糊的描述具体化，必要时补充\"预期效果\"\"验收标准\"这类结构，让编程助手能一次理解到位；\
         3) 只输出改写后的指令本身，不要输出任何解释、前后缀说明或引号。";

    #[tauri::command]
    pub async fn coding_optimize_prompt(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        text: String,
    ) -> Result<String, AppError> {
        let session = get_session(&state, workspace_id).await?;
        let provider_id = session.lock().await.provider_id;
        let provider = state
            .ai_provider_manager
            .get(provider_id)?
            .ok_or_else(|| AppError::NotFound(format!("ai provider not found: {provider_id}")))?;
        let api_key = state.ai_provider_manager.resolve_api_key(&provider).await?;
        roc_desk_common::ai::AiChatClient::new()
            .complete_once(&provider, api_key.as_deref(), OPTIMIZE_PROMPT_SYSTEM, &text)
            .await
    }

    #[tauri::command]
    pub async fn coding_answer_question(
        state: State<'_, WorkspaceAppState>,
        request_id: Uuid,
        answer: String,
    ) -> Result<(), AppError> {
        state.question_confirms.resolve(request_id, answer).await;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // AI coding agent -- permission rules
    // -----------------------------------------------------------------------

    #[tauri::command]
    pub async fn permission_rule_list(
        state: State<'_, WorkspaceAppState>,
    ) -> Result<Vec<PermissionRule>, AppError> {
        state.permission_rules.list()
    }

    #[derive(serde::Deserialize)]
    pub struct PermissionRuleInput {
        pub tool: String,
        pub pattern: String,
        pub decision: String,
    }

    #[tauri::command]
    pub async fn permission_rule_create(
        state: State<'_, WorkspaceAppState>,
        input: PermissionRuleInput,
    ) -> Result<PermissionRule, AppError> {
        let rule = PermissionRule {
            id: Uuid::new_v4(),
            tool: input.tool,
            pattern: input.pattern,
            decision: Decision::from_str(&input.decision),
            enabled: true,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        state.permission_rules.create(&rule)?;
        Ok(rule)
    }

    #[tauri::command]
    pub async fn permission_rule_delete(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
    ) -> Result<(), AppError> {
        state.permission_rules.delete(id)
    }

    // -----------------------------------------------------------------------
    // AI coding agent -- MCP server management
    // -----------------------------------------------------------------------

    #[tauri::command]
    pub async fn mcp_server_list(
        state: State<'_, WorkspaceAppState>,
    ) -> Result<Vec<McpServer>, AppError> {
        state.mcp_manager.list()
    }

    #[tauri::command]
    pub async fn mcp_server_create(
        state: State<'_, WorkspaceAppState>,
        input: McpServerInput,
    ) -> Result<McpServer, AppError> {
        state.mcp_manager.create(input).await
    }

    #[tauri::command]
    pub async fn mcp_server_update(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
        input: McpServerInput,
    ) -> Result<McpServer, AppError> {
        state.mcp_manager.update(id, input).await
    }

    #[tauri::command]
    pub async fn mcp_server_delete(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
    ) -> Result<(), AppError> {
        state.mcp_manager.delete(id).await
    }

    // -----------------------------------------------------------------------
    // AI coding agent -- Skills view/import
    // -----------------------------------------------------------------------

    #[tauri::command]
    pub async fn skill_list(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
    ) -> Result<Vec<SkillMeta>, AppError> {
        let handle = state
            .open_workspaces
            .read()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("workspace not opened: {workspace_id}")))?;
        Ok(crate::coding::skills::discover_skills(handle.file_ops.as_ref(), &handle.profile.root_path).await)
    }

    #[tauri::command]
    pub async fn skill_delete(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        name: String,
    ) -> Result<(), AppError> {
        let handle = state
            .open_workspaces
            .read()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("workspace not opened: {workspace_id}")))?;
        let skills =
            crate::coding::skills::discover_skills(handle.file_ops.as_ref(), &handle.profile.root_path).await;
        let skill = skills
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| AppError::NotFound(format!("未找到技能：{name}")))?;
        handle.file_ops.delete(&skill.dir, true).await?;
        clear_probe_cache(workspace_id);
        Ok(())
    }

    #[tauri::command]
    pub async fn skill_import(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        local_path: String,
    ) -> Result<SkillMeta, AppError> {
        let handle = state
            .open_workspaces
            .read()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("workspace not opened: {workspace_id}")))?;

        let raw_path = local_path.trim_end_matches(['/', '\\']).to_string();
        let is_archive = crate::coding::skills::is_archive_path(&raw_path);
        let (effective_path, _temp_guard) = if is_archive {
            let extract_dir =
                std::env::temp_dir().join(format!("roc_desk-skill-{}", Uuid::new_v4()));
            let skill_root = crate::coding::skills::extract_skill_archive(
                std::path::Path::new(&raw_path),
                &extract_dir,
            )?;
            (
                skill_root.to_string_lossy().into_owned(),
                Some(TempDirGuard(extract_dir)),
            )
        } else {
            (raw_path.clone(), None)
        };

        let local_ops = roc_desk_common::fsops::local::LocalFileOps;
        let skill_md_content = local_ops
            .read_file(&format!("{effective_path}/SKILL.md"))
            .await
            .map_err(|_| AppError::Internal(format!("{effective_path} 下没有找到 SKILL.md，不是一个合法的技能目录")))?;
        let (fields, _) = crate::coding::skills::parse_frontmatter(&skill_md_content.text);
        let folder_name = if is_archive {
            let base = raw_path.rsplit(['/', '\\']).next().unwrap_or(&raw_path);
            base.trim_end_matches(".zip")
                .trim_end_matches(".tar.gz")
                .trim_end_matches(".tgz")
                .to_string()
        } else {
            effective_path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&effective_path)
                .to_string()
        };
        let name = fields.get("name").cloned().unwrap_or(folder_name);
        let description = fields.get("description").cloned().unwrap_or_default();

        let root = handle.profile.root_path.trim_end_matches(['/', '\\']);
        let rock_desk_dir = format!("{root}/.rock_desk");
        let skills_root = format!("{rock_desk_dir}/skills");
        let dest = format!("{skills_root}/{name}");
        let _ = handle.file_ops.create_dir(&rock_desk_dir).await;
        let _ = handle.file_ops.create_dir(&skills_root).await;
        if handle.file_ops.list_dir(&dest).await.is_ok() {
            handle.file_ops.delete(&dest, true).await?;
        }

        let should_cancel = || false;
        let file_count = std::sync::atomic::AtomicU64::new(0);
        roc_desk_common::fsops::copy_between(
            &local_ops,
            &effective_path,
            handle.file_ops.as_ref(),
            &dest,
            true,
            &None,
            &should_cancel,
            &file_count,
        )
        .await?;

        clear_probe_cache(workspace_id);
        Ok(SkillMeta {
            name,
            description,
            dir: dest,
        })
    }

    // -----------------------------------------------------------------------
    // AI coding agent -- Accept/Reject/Undo/Redo/RevertTurn (own
    // `ChangeStore` lock, never `CodingSession`'s -- see `coding_changes`'s
    // field doc comment)
    // -----------------------------------------------------------------------

    #[tauri::command]
    pub async fn coding_accept_change(
        state: State<'_, WorkspaceAppState>,
        app_handle: AppHandle,
        workspace_id: Uuid,
        change_id: Uuid,
    ) -> Result<FileSyncInfo, AppError> {
        let store = get_change_store(&state, workspace_id).await?;
        let mut guard = store.lock().await;
        let result = guard.accept(change_id).await;
        if let Ok((_, Some(commit))) = &result {
            let _ = app_handle.emit(
                "coding:git-commit-result",
                serde_json::json!({ "sessionId": guard.session_id(), "path": commit.path, "output": commit.output }),
            );
        }
        if result.is_ok() {
            if let Some(turn_id) = guard.changes().iter().find(|c| c.id == change_id).map(|c| c.turn_id) {
                let still_pending = guard
                    .changes()
                    .iter()
                    .any(|c| c.turn_id == turn_id && c.status == ChangeStatus::Pending);
                drop(guard);
                if !still_pending {
                    maybe_auto_continue(&state, &app_handle, workspace_id, turn_id);
                }
            }
        }
        result.map(|(sync, _)| sync)
    }

    #[tauri::command]
    pub async fn coding_reject_change(
        state: State<'_, WorkspaceAppState>,
        app_handle: AppHandle,
        workspace_id: Uuid,
        change_id: Uuid,
    ) -> Result<(), AppError> {
        let store = get_change_store(&state, workspace_id).await?;
        let mut guard = store.lock().await;
        let result = guard.reject(change_id);
        if result.is_ok() {
            if let Some(turn_id) = guard.changes().iter().find(|c| c.id == change_id).map(|c| c.turn_id) {
                let still_pending = guard
                    .changes()
                    .iter()
                    .any(|c| c.turn_id == turn_id && c.status == ChangeStatus::Pending);
                drop(guard);
                if !still_pending {
                    maybe_auto_continue(&state, &app_handle, workspace_id, turn_id);
                }
            }
        }
        result
    }

    #[tauri::command]
    pub async fn coding_undo_change(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        change_id: Uuid,
    ) -> Result<FileSyncInfo, AppError> {
        let store = get_change_store(&state, workspace_id).await?;
        let mut guard = store.lock().await;
        guard.undo(change_id).await
    }

    #[tauri::command]
    pub async fn coding_redo_change(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
    ) -> Result<Option<FileSyncInfo>, AppError> {
        let store = get_change_store(&state, workspace_id).await?;
        let mut guard = store.lock().await;
        guard.redo().await
    }

    #[tauri::command]
    pub async fn coding_revert_turn(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        turn_id: Uuid,
    ) -> Result<Vec<FileSyncInfo>, AppError> {
        let store = get_change_store(&state, workspace_id).await?;
        let mut guard = store.lock().await;
        guard.revert_turn(turn_id).await
    }

    #[tauri::command]
    pub async fn coding_confirm_command(
        state: State<'_, WorkspaceAppState>,
        request_id: Uuid,
        allow: bool,
    ) -> Result<(), AppError> {
        state.command_confirms.resolve(request_id, allow).await;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // AI coding agent -- history
    // -----------------------------------------------------------------------

    #[tauri::command]
    pub async fn coding_history_save(
        state: State<'_, WorkspaceAppState>,
        input: CodingHistoryInput,
    ) -> Result<(), AppError> {
        let mut input = input;
        if let Some(session) = state
            .coding_sessions
            .read()
            .await
            .get(&input.workspace_id)
            .cloned()
        {
            let messages = session.lock().await.messages_snapshot();
            input.messages = serde_json::to_value(&messages).unwrap_or_default();
        }
        state.coding_history.save(&input)?;
        if let Some(location) = state.coding_history.get_location(input.id)? {
            let snapshot = WorkspaceHistorySnapshot {
                input: input.clone(),
                created_at: location.summary.created_at,
                updated_at: location.summary.updated_at,
            };
            if let Some(handle) = state
                .open_workspaces
                .read()
                .await
                .get(&input.workspace_id)
                .cloned()
            {
                let dir = format!(
                    "{}/.rock_desk/sessions",
                    handle.profile.root_path.trim_end_matches(['/', '\\'])
                );
                let path = format!("{dir}/{}.json", input.id);
                if let Ok(json) = serde_json::to_string_pretty(&snapshot) {
                    let history_id = input.id;
                    tokio::spawn(async move {
                        let mirrored = tokio::time::timeout(
                            std::time::Duration::from_secs(15),
                            async {
                                handle
                                    .file_ops
                                    .create_dir(&format!(
                                        "{}/.rock_desk",
                                        handle.profile.root_path.trim_end_matches(['/', '\\'])
                                    ))
                                    .await?;
                                handle.file_ops.create_dir(&dir).await?;
                                handle.file_ops.write_file(&path, &json, None).await?;
                                Ok::<(), AppError>(())
                            },
                        )
                        .await;
                        if !matches!(mirrored, Ok(Ok(()))) {
                            tracing::warn!(%path, "failed to mirror coding history into workspace cache");
                            let fallback = handle.fallback_cache_dir.join("sessions");
                            if std::fs::create_dir_all(&fallback).is_ok() {
                                let _ = std::fs::write(
                                    fallback.join(format!("{history_id}.json")),
                                    json,
                                );
                            }
                        }
                    });
                }
            }
        }
        Ok(())
    }

    #[tauri::command]
    pub async fn coding_history_list(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
    ) -> Result<Vec<CodingHistorySummary>, AppError> {
        history_list_with_import(&state, workspace_id).await
    }

    #[tauri::command]
    pub async fn coding_history_get(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
    ) -> Result<Option<CodingHistoryDetail>, AppError> {
        let Some(location) = state.coding_history.get_location(id)? else {
            return Ok(None);
        };
        match load_history_snapshot(&state, location.workspace_id, id).await {
            Some(snapshot) => Ok(Some(CodingHistoryDetail {
                summary: location.summary,
                workspace_id: location.workspace_id,
                timeline: snapshot.input.timeline,
                changes: snapshot.input.changes,
                messages: snapshot.input.messages,
                content_available: true,
            })),
            None => Ok(Some(CodingHistoryDetail {
                summary: location.summary,
                workspace_id: location.workspace_id,
                timeline: serde_json::Value::Null,
                changes: serde_json::Value::Null,
                messages: serde_json::Value::Null,
                content_available: false,
            })),
        }
    }

    /// Opens a history entry and genuinely continues the conversation (not
    /// a read-only replay) -- feeds the persisted `messages`/`changes` back
    /// into a freshly constructed `CodingSession`/`ChangeStore`, replacing
    /// this workspace's current active session; `session.id` reuses the
    /// history entry's own id so later `saveCurrentHistory` updates keep
    /// writing the same row instead of forking a new history entry.
    #[tauri::command]
    pub async fn coding_history_resume(
        state: State<'_, WorkspaceAppState>,
        workspace_id: Uuid,
        history_id: Uuid,
    ) -> Result<CodingSessionInfo, AppError> {
        require_ssh(&state)?;
        let location = state
            .coding_history
            .get_location(history_id)?
            .ok_or_else(|| AppError::NotFound(format!("history not found: {history_id}")))?;
        if location.workspace_id != workspace_id {
            return Err(AppError::Internal("这条历史记录不属于当前工作区".into()));
        }
        if state
            .ai_provider_manager
            .get(location.summary.provider_id)?
            .is_none()
        {
            return Err(AppError::NotFound(
                "这条历史记录关联的 AI 供应商已被删除，请先在模型管理里重新配置后再试".into(),
            ));
        }
        // 这几份内容现在只存工作区目录一份，续聊必须真的读到它们才有意义——
        // 读不到（工作区断连/文件被删）不能悄悄当成"空对话"续上。
        let snapshot = load_history_snapshot(&state, workspace_id, history_id)
            .await
            .ok_or_else(|| {
                AppError::Internal(
                    "无法连接到该历史记录所在的工作区，暂时读取不到完整的对话内容，请检查连接后重试"
                        .into(),
                )
            })?;

        let (mut session, change_store) = build_new_session(
            &state,
            workspace_id,
            location.summary.provider_id,
            false,
            Some(history_id),
        )
        .await?;
        session.mode = if location.summary.mode == "build" {
            CodingMode::Build
        } else {
            CodingMode::Plan
        };
        let messages: Vec<serde_json::Value> =
            serde_json::from_value(snapshot.input.messages).unwrap_or_default();
        session.restore_messages(messages);
        let changes: Vec<FileChange> =
            serde_json::from_value(snapshot.input.changes).unwrap_or_default();
        change_store.lock().await.restore(changes);

        let info = session_info(&session).await;
        state
            .coding_sessions
            .write()
            .await
            .insert(workspace_id, Arc::new(Mutex::new(session)));
        state
            .coding_changes
            .write()
            .await
            .insert(workspace_id, change_store);
        Ok(info)
    }

    #[tauri::command]
    pub async fn coding_history_rename(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
        title: String,
    ) -> Result<(), AppError> {
        state.coding_history.rename(id, title.trim())
    }

    #[tauri::command]
    pub async fn coding_history_delete(
        state: State<'_, WorkspaceAppState>,
        id: Uuid,
    ) -> Result<(), AppError> {
        state.coding_history.delete(id)
    }
}
