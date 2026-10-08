//! Tauri-command-layer support for the AI coding agent, ported from the
//! host's `commands/coding.rs`. Mirrors the host's split between pure
//! helper logic (this file) and thin `#[tauri::command]` wrappers (in
//! [`crate::cmd`]) -- the command macro itself must live at a module that
//! isn't the crate root (see `lib.rs`'s doc comment on `pub mod cmd`), but
//! there's no requirement that *every* helper live alongside the commands,
//! so the bulk of the logic lives here where it can be read start to finish
//! without the `#[tauri::command]` boilerplate interleaved.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Instant;

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;
use uuid::Uuid;

use roc_desk_common::change_store::{ChangeStatus, ChangeStore, CodingTarget, FileChange};
use roc_desk_common::fsops::FileOps;
use roc_desk_core::error::AppError;
use roc_desk_core::workspace::WorkspaceKind;

use super::git_ops::SshGitCommitter;
use super::history::{CodingHistoryRepo, CodingHistorySummary, WorkspaceHistorySnapshot};
use super::session::{CodingMode, CodingSession};
use super::skills::SkillMeta;
use super::tools::TodoItem;
use crate::WorkspaceAppState;

/// Shared timeout for every probing/best-effort SFTP·Agent round trip on
/// the "open workspace/switch history" paths -- these operations are all
/// "read a couple of small files/list one small directory", which should
/// complete in well under a second on a healthy connection; hitting this
/// cap means the connection is already unusable, no reason to make the
/// user wait for the minutes-level timeout `exec`/transfers use.
const WORKSPACE_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Max number of workspace history snapshots (`.rock_desk/sessions/{id}.json`)
/// a single "list history" call will synchronously wait to import, newest
/// (by mtime) first. The rest are left for a future call to pick up
/// incrementally -- a history list with dozens of sessions, each carrying a
/// full conversation timeline and file changes (easily several MB), would
/// otherwise make "list history" itself take many seconds.
const INLINE_SNAPSHOT_SYNC_LIMIT: usize = 8;
/// How long a session-start probe (project memory + skills) result stays
/// cached, see [`probe_workspace`].
const PROBE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone)]
struct SnapshotCandidate {
    path: String,
    mtime: Option<i64>,
}

struct ProbeCache {
    root_path: String,
    at: Instant,
    memory: Vec<(String, String)>,
    skills: Vec<SkillMeta>,
}

/// Process-wide in-memory caches for the "open AI tool/new session/switch
/// history" paths -- purely caches, losing them just costs one extra
/// network round trip, never a correctness issue, so this doesn't need to
/// live on [`WorkspaceAppState`] itself.
#[derive(Default)]
struct CodingCaches {
    /// `(workspace_id, history_id) -> remote mtime` last synced by this
    /// process -- lets `history_list_with_import` skip re-reading a
    /// snapshot file whose remote mtime it's already accounted for,
    /// instead of comparing against local SQLite's `updated_at` (which can
    /// false-positive "changed" on nothing more than clock skew between
    /// two machines, forcing a full re-download of every snapshot in the
    /// directory on every history list).
    synced_snapshots: HashMap<(Uuid, Uuid), i64>,
    probes: HashMap<Uuid, ProbeCache>,
}

fn caches() -> &'static StdMutex<CodingCaches> {
    static CACHES: OnceLock<StdMutex<CodingCaches>> = OnceLock::new();
    CACHES.get_or_init(|| StdMutex::new(CodingCaches::default()))
}

fn mark_snapshot_synced(workspace_id: Uuid, history_id: Uuid, mtime: i64) {
    if let Ok(mut guard) = caches().lock() {
        guard
            .synced_snapshots
            .insert((workspace_id, history_id), mtime);
    }
}

fn snapshot_already_synced(workspace_id: Uuid, history_id: Uuid, mtime: i64) -> bool {
    caches()
        .lock()
        .map(|guard| guard.synced_snapshots.get(&(workspace_id, history_id)) == Some(&mtime))
        .unwrap_or(false)
}

async fn import_workspace_snapshot(
    file_ops: &dyn FileOps,
    history_repo: &CodingHistoryRepo,
    workspace_id: Uuid,
    candidate: &SnapshotCandidate,
) {
    let Ok(Ok(file)) =
        tokio::time::timeout(WORKSPACE_PROBE_TIMEOUT, file_ops.read_file(&candidate.path)).await
    else {
        return;
    };
    let Ok(snapshot) = serde_json::from_str::<WorkspaceHistorySnapshot>(&file.text) else {
        return;
    };
    if snapshot.input.workspace_id != workspace_id {
        return;
    }
    let history_id = snapshot.input.id;
    if history_repo.import_snapshot(&snapshot).is_ok() {
        if file.mtime > 0 {
            mark_snapshot_synced(workspace_id, history_id, file.mtime);
        } else if let Some(mtime) = candidate.mtime {
            mark_snapshot_synced(workspace_id, history_id, mtime);
        }
    }
}

/// Session-start remote probing -- project memory (`AGENTS.md`/`CLAUDE.md`)
/// and skill discovery, run **concurrently, not sequentially** (SSH `exec`/
/// SFTP handshakes have minutes-level timeout ceilings; sequential in the
/// worst case pays their sum), each with its own shorter
/// [`WORKSPACE_PROBE_TIMEOUT`] since both are "read two small files/list one
/// small directory" and shouldn't normally take anywhere near that long --
/// these are "nice to have" prompt enrichments, a timeout just means the
/// probe failed, never blocks the session from being usable.
///
/// Cached per workspace for [`PROBE_CACHE_TTL`]: repeatedly clicking "new
/// session", or switching between history entries, shouldn't re-probe every
/// time within such a short window.
async fn probe_workspace(
    workspace_id: Uuid,
    root_path: &str,
    file_ops: &dyn FileOps,
) -> (Vec<(String, String)>, Vec<SkillMeta>) {
    let cached = caches()
        .lock()
        .ok()
        .and_then(|guard| match guard.probes.get(&workspace_id) {
            Some(probe) if probe.root_path == root_path && probe.at.elapsed() < PROBE_CACHE_TTL => {
                Some((probe.memory.clone(), probe.skills.clone()))
            }
            _ => None,
        });
    if let Some(cached) = cached {
        return cached;
    }

    let (memory, skills) = tokio::join!(
        async {
            tokio::time::timeout(
                WORKSPACE_PROBE_TIMEOUT,
                super::session::fetch_project_memory(file_ops),
            )
            .await
            .unwrap_or_default()
        },
        async {
            tokio::time::timeout(
                WORKSPACE_PROBE_TIMEOUT,
                super::skills::discover_skills(file_ops, root_path),
            )
            .await
            .unwrap_or_default()
        },
    );

    if let Ok(mut guard) = caches().lock() {
        guard.probes.insert(
            workspace_id,
            ProbeCache {
                root_path: root_path.to_string(),
                at: Instant::now(),
                memory: memory.clone(),
                skills: skills.clone(),
            },
        );
    }
    (memory, skills)
}

#[derive(Serialize)]
pub struct CodingSessionInfo {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub mode: CodingMode,
    pub target: CodingTarget,
    pub auto_allow_readonly: bool,
    pub git_repo: bool,
    pub auto_git_commit: bool,
    pub full_auto: bool,
    pub auto_apply_changes: bool,
    pub changes: Vec<FileChange>,
    pub todos: Vec<TodoItem>,
    pub project_memory_loaded: Vec<String>,
}

pub(crate) async fn session_info(session: &CodingSession) -> CodingSessionInfo {
    let store = session.change_store.lock().await;
    CodingSessionInfo {
        id: session.id,
        provider_id: session.provider_id,
        mode: session.mode,
        target: session.target.clone(),
        auto_allow_readonly: store
            .auto_allow_readonly
            .load(std::sync::atomic::Ordering::Relaxed),
        git_repo: store.git_repo(),
        auto_git_commit: store.auto_git_commit,
        full_auto: store.full_auto.load(std::sync::atomic::Ordering::Relaxed),
        auto_apply_changes: store
            .auto_apply_changes
            .load(std::sync::atomic::Ordering::Relaxed),
        changes: store.changes().to_vec(),
        todos: session.todos.clone(),
        project_memory_loaded: session.project_memory_loaded.clone(),
    }
}

/// Builds a brand-new (never reusing any existing in-memory session)
/// `CodingSession`. `resume_recent` controls whether the most recently
/// persisted change record gets fed back into `session.changes` --
/// `coding_start` (auto-bind-to-workspace, including "come back after an
/// app restart/switching workspaces") passes `true`; `coding_new_session`
/// (user explicitly clicked "new session") passes `false` -- the user's
/// intent there is a clean slate, old changes shouldn't quietly carry over.
pub(crate) async fn build_new_session(
    state: &State<'_, WorkspaceAppState>,
    workspace_id: Uuid,
    provider_id: Uuid,
    resume_recent: bool,
    override_id: Option<Uuid>,
) -> Result<(CodingSession, Arc<Mutex<ChangeStore>>), AppError> {
    let workspaces = state.open_workspaces.read().await;
    let handle = workspaces
        .get(&workspace_id)
        .ok_or_else(|| AppError::NotFound(format!("workspace not opened: {workspace_id}")))?;
    let profile = handle.profile.clone();
    let file_ops = handle.file_ops.clone();
    drop(workspaces);

    let target = match profile.kind {
        WorkspaceKind::Local => CodingTarget::Local,
        WorkspaceKind::Remote => {
            let connection_id = profile.connection_id.ok_or_else(|| {
                AppError::Internal("remote workspace missing connection_id".into())
            })?;
            let ssh = state
                .ssh
                .as_ref()
                .ok_or_else(|| AppError::Internal("远程工作区功能未启用".into()))?;
            let connection = ssh
                .connection_manager
                .get(connection_id)?
                .ok_or_else(|| {
                    AppError::NotFound(format!("connection not found: {connection_id}"))
                })?;
            match connection.protocol {
                roc_desk_ssh::connection::Protocol::Agent => CodingTarget::Agent {
                    connection_id,
                    host_label: profile.display_name.clone(),
                },
                _ => CodingTarget::Remote {
                    connection_id,
                    host_label: profile.display_name.clone(),
                },
            }
        }
    };

    let (memory, skills) =
        probe_workspace(workspace_id, &profile.root_path, file_ops.as_ref()).await;

    let id = override_id.unwrap_or_else(Uuid::new_v4);
    let mut change_store = ChangeStore::new(
        id,
        profile.root_path.clone(),
        target.clone(),
        file_ops.clone(),
        false,
    );
    if let Some(ssh) = state.ssh.as_ref() {
        change_store = change_store.with_committer(Arc::new(SshGitCommitter::new(
            ssh.ssh_pool.clone(),
            ssh.agent_pool.clone(),
        )));
    }
    let change_store = Arc::new(Mutex::new(change_store));
    let mut session = CodingSession::new(
        id,
        workspace_id,
        profile.root_path.clone(),
        target,
        provider_id,
        file_ops,
        change_store.clone(),
        state.ai_evidence.clone(),
    );
    session.apply_project_memory(memory);
    session.apply_skills(skills);

    if resume_recent {
        if let Ok(histories) = history_list_with_import(state, workspace_id).await {
            if let Some(latest) = histories.first() {
                let recent = chrono::DateTime::parse_from_rfc3339(&latest.updated_at)
                    .map(|t| {
                        chrono::Utc::now().signed_duration_since(t.with_timezone(&chrono::Utc))
                            <= chrono::Duration::hours(12)
                    })
                    .unwrap_or(false);
                if recent {
                    if let Ok(Some(detail)) = state.coding_history.get(latest.id) {
                        if let Ok(changes) =
                            serde_json::from_value::<Vec<FileChange>>(detail.changes)
                        {
                            change_store.lock().await.restore(changes);
                        }
                    }
                }
            }
        }
    }

    Ok((session, change_store))
}

pub(crate) async fn get_session(
    state: &State<'_, WorkspaceAppState>,
    workspace_id: Uuid,
) -> Result<Arc<Mutex<CodingSession>>, AppError> {
    state
        .coding_sessions
        .read()
        .await
        .get(&workspace_id)
        .cloned()
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "no coding session for workspace {workspace_id}, call coding_start first"
            ))
        })
}

pub(crate) async fn get_change_store(
    state: &State<'_, WorkspaceAppState>,
    workspace_id: Uuid,
) -> Result<Arc<Mutex<ChangeStore>>, AppError> {
    state
        .coding_changes
        .read()
        .await
        .get(&workspace_id)
        .cloned()
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "no coding session for workspace {workspace_id}, call coding_start first"
            ))
        })
}

/// Called by accept/reject once they've confirmed "this turn has no more
/// Pending changes left": checks in the background whether `CodingSession`
/// is currently stuck waiting on this turn's confirmation, and if so,
/// automatically continues the conversation for the user. Spawned and not
/// awaited -- `coding_accept_change`/`coding_reject_change` must return the
/// write result to the frontend immediately, not block the "apply" button
/// itself behind a potentially slow AI continuation request.
pub(crate) fn maybe_auto_continue(
    state: &State<'_, WorkspaceAppState>,
    app_handle: &AppHandle,
    workspace_id: Uuid,
    turn_id: Uuid,
) {
    let coding_sessions = state.coding_sessions.clone();
    let coding_changes = state.coding_changes.clone();
    let ai_provider_manager = state.ai_provider_manager.clone();
    let Some(ssh) = state.ssh.as_ref() else {
        return;
    };
    let ssh_pool = ssh.ssh_pool.clone();
    let agent_pool = ssh.agent_pool.clone();
    let audit_log = state.audit_log.clone();
    let command_confirms = state.command_confirms.clone();
    let permission_rules = state.permission_rules.clone();
    let question_confirms = state.question_confirms.clone();
    let mcp_manager = state.mcp_manager.clone();
    let coding_cancel_tokens = state.coding_cancel_tokens.clone();
    let coding_pending_injections = state.coding_pending_injections.clone();
    let symbol_indexes = state.symbol_indexes.clone();
    let app_handle = app_handle.clone();

    tokio::spawn(async move {
        let Some(session) = coding_sessions.read().await.get(&workspace_id).cloned() else {
            return;
        };
        let mut session_guard = session.lock().await;
        if !session_guard.resolve_awaiting_confirmation(turn_id) {
            return;
        }

        let summary = match coding_changes.read().await.get(&workspace_id).cloned() {
            Some(store) => {
                let guard = store.lock().await;
                summarize_turn_changes(guard.changes().iter().filter(|c| c.turn_id == turn_id))
            }
            None => "改动已处理".to_string(),
        };
        let continuation_text =
            format!("[系统自动继续] 你上一轮提议的文件改动已经全部处理完：{summary}。请据此继续完成任务。");
        let session_id = session_guard.id;

        let _ = app_handle.emit(
            "coding:auto-continue-start",
            json!({ "sessionId": session_id, "note": format!("变更已确认（{summary}），AI 正在自动继续任务…") }),
        );

        let cancel_token = tokio_util::sync::CancellationToken::new();
        coding_cancel_tokens
            .lock()
            .unwrap()
            .insert(workspace_id, cancel_token.clone());
        let result = session_guard
            .send_message(
                &continuation_text,
                &[],
                &ai_provider_manager,
                &ssh_pool,
                &agent_pool,
                &audit_log,
                &command_confirms,
                &permission_rules,
                &question_confirms,
                &mcp_manager,
                &app_handle,
                &cancel_token,
                &coding_pending_injections,
                &symbol_indexes,
            )
            .await;
        coding_cancel_tokens.lock().unwrap().remove(&workspace_id);

        let payload = match &result {
            Ok(reply) => json!({ "sessionId": session_id, "reply": reply, "error": Option::<String>::None }),
            Err(e) => json!({ "sessionId": session_id, "reply": Option::<String>::None, "error": e.to_string() }),
        };
        let _ = app_handle.emit("coding:auto-continue-done", payload);
    });
}

fn summarize_turn_changes<'a>(changes: impl Iterator<Item = &'a FileChange>) -> String {
    let mut applied = Vec::new();
    let mut rejected = Vec::new();
    for c in changes {
        match c.status {
            ChangeStatus::Applied => applied.push(c.path.as_str()),
            ChangeStatus::Rejected => rejected.push(c.path.as_str()),
            _ => {}
        }
    }
    let mut parts = Vec::new();
    if !applied.is_empty() {
        parts.push(format!("已应用 {} 个（{}）", applied.len(), applied.join("、")));
    }
    if !rejected.is_empty() {
        parts.push(format!("已拒绝 {} 个（{}）", rejected.len(), rejected.join("、")));
    }
    if parts.is_empty() {
        "没有改动被处理".to_string()
    } else {
        parts.join("；")
    }
}

/// Imports the workspace's own `.rock_desk/sessions/*.json` snapshots (see
/// `coding_history_save`'s doc comment -- a second, workspace-portable copy
/// of history, on top of local SQLite) into the local cache before listing
/// from it. `build_new_session` reuses this same function to find "the most
/// recent session" to restore `session.changes` from -- both call sites
/// must agree on what "most recent" means.
pub(crate) async fn history_list_with_import(
    state: &State<'_, WorkspaceAppState>,
    workspace_id: Uuid,
) -> Result<Vec<CodingHistorySummary>, AppError> {
    if let Some(handle) = state.open_workspaces.read().await.get(&workspace_id).cloned() {
        let dir = format!(
            "{}/.rock_desk/sessions",
            handle.profile.root_path.trim_end_matches(['/', '\\'])
        );
        let entries = tokio::time::timeout(WORKSPACE_PROBE_TIMEOUT, handle.file_ops.list_dir(&dir))
            .await
            .unwrap_or(Err(AppError::Internal("list_dir timed out".into())));
        if let Ok(entries) = entries {
            let mut to_fetch: Vec<SnapshotCandidate> = entries
                .into_iter()
                .filter(|entry| !entry.is_dir && entry.name.ends_with(".json"))
                .filter_map(|entry| {
                    let id = entry.name.strip_suffix(".json").and_then(|value| Uuid::parse_str(value).ok());
                    let synced = matches!((id, entry.modified), (Some(id), Some(mtime)) if snapshot_already_synced(workspace_id, id, mtime));
                    (!synced).then_some(SnapshotCandidate { path: entry.path, mtime: entry.modified })
                })
                .collect();
            to_fetch
                .sort_by_key(|candidate| std::cmp::Reverse(candidate.mtime.unwrap_or_default()));
            to_fetch.truncate(INLINE_SNAPSHOT_SYNC_LIMIT);
            let fetches = to_fetch.iter().map(|candidate| {
                import_workspace_snapshot(
                    handle.file_ops.as_ref(),
                    state.coding_history.as_ref(),
                    workspace_id,
                    candidate,
                )
            });
            futures_util::future::join_all(fetches).await;
        }
        let fallback = handle.fallback_cache_dir.join("sessions");
        if let Ok(entries) = std::fs::read_dir(fallback) {
            for entry in entries.flatten() {
                if let Ok(bytes) = std::fs::read(entry.path()) {
                    if let Ok(snapshot) = serde_json::from_slice::<WorkspaceHistorySnapshot>(&bytes)
                    {
                        if snapshot.input.workspace_id == workspace_id {
                            let _ = state.coding_history.import_snapshot(&snapshot);
                        }
                    }
                }
            }
        }
    }
    state.coding_history.list(workspace_id)
}

/// Per-history-entry "workspace directory is the source of truth" refresh --
/// `history_list_with_import` is the batch reconciliation used for listing
/// (list the directory once, skip unchanged files); this is the precise
/// single-file reconciliation used when opening/viewing one specific
/// history entry. A read failure (network hiccup/file missing) isn't
/// treated as an error, silently falls back to whatever's already in the
/// local cache -- this is "try to get the latest", not "must get the
/// latest".
pub(crate) async fn refresh_history_from_workspace(
    state: &State<'_, WorkspaceAppState>,
    workspace_id: Uuid,
    history_id: Uuid,
) {
    let Some(handle) = state.open_workspaces.read().await.get(&workspace_id).cloned() else {
        return;
    };
    let path = format!(
        "{}/.rock_desk/sessions/{history_id}.json",
        handle.profile.root_path.trim_end_matches(['/', '\\'])
    );
    if let Ok(Ok(file)) =
        tokio::time::timeout(WORKSPACE_PROBE_TIMEOUT, handle.file_ops.read_file(&path)).await
    {
        if let Ok(snapshot) = serde_json::from_str::<WorkspaceHistorySnapshot>(&file.text) {
            if snapshot.input.workspace_id == workspace_id {
                let _ = state.coding_history.import_snapshot(&snapshot);
                return;
            }
        }
    }
    let fallback = handle
        .fallback_cache_dir
        .join("sessions")
        .join(format!("{history_id}.json"));
    if let Ok(bytes) = std::fs::read(fallback) {
        if let Ok(snapshot) = serde_json::from_slice::<WorkspaceHistorySnapshot>(&bytes) {
            if snapshot.input.workspace_id == workspace_id {
                let _ = state.coding_history.import_snapshot(&snapshot);
            }
        }
    }
}

/// `skill_import`'s temp-extraction-directory guard -- cleaned up on both
/// success and any early-`?`-return error path via `Drop`, instead of
/// duplicating cleanup code at every fallible branch.
pub(crate) struct TempDirGuard(pub std::path::PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn clear_probe_cache(workspace_id: Uuid) {
    if let Ok(mut guard) = caches().lock() {
        guard.probes.remove(&workspace_id);
    }
}
