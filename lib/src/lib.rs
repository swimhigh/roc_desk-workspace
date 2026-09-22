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
//! - Remote (SSH/Agent) workspaces and remote terminal sessions -- depend on
//!   `roc_desk-ssh`'s connection pools, which haven't been split out of the
//!   host yet. `roc_desk_core::workspace` and this crate's `pty`/`git`
//!   modules are therefore local-only.
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

use std::path::PathBuf;

use roc_desk_core::error::AppError;
use roc_desk_core::workspace::WorkspaceManager;

/// Shared state for this tool's Tauri commands. Constructed once at startup
/// (see [`WorkspaceAppState::new`]) and registered with `tauri::Builder::manage`.
pub struct WorkspaceAppState {
    pub workspace_manager: WorkspaceManager,
    pub local_pty: pty::SharedLocalPtyManager,
}

impl WorkspaceAppState {
    /// `db_path` is this tool's own SQLite file (the "recent workspaces"
    /// list); `cache_root` is where the fallback `.rock_desk` workspace
    /// metadata cache directory lives (mirrors the host's
    /// `WorkspaceManager::new`).
    pub fn new(db_path: &std::path::Path, cache_root: PathBuf) -> Result<Self, AppError> {
        let pool = roc_desk_core::db::pool::create_pool(db_path)?;
        let repo = roc_desk_core::workspace::WorkspaceRepo::new(pool);
        let workspace_manager = WorkspaceManager::new(repo, cache_root);
        workspace_manager.ensure_schema()?;
        Ok(Self {
            workspace_manager,
            local_pty: std::sync::Arc::new(pty::LocalPtyManager::default()),
        })
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

    use crate::WorkspaceAppState;

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
        state.workspace_manager.open_local(&path)
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
