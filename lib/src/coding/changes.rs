use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use roc_desk_core::error::AppError;
use roc_desk_common::fsops::{FileOps, WriteOutcome};
use roc_desk_ssh::agent::AgentConnectionPool;
use roc_desk_ssh::ssh::SshConnectionPool;

use super::diff::{generate_diff, DiffLine};
use super::git_ops;
use super::target::CodingTarget;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStatus {
    Pending,
    Applied,
    Rejected,
    Undone,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub id: Uuid,
    pub path: String,
    pub old_content: String,
    pub new_content: String,
    pub diff: Vec<DiffLine>,
    pub status: ChangeStatus,
    #[serde(default)]
    pub expected_mtime: Option<i64>,
    /// The user-message turn that produced this change -- the model may
    /// edit several files in a row within one conversation turn; the
    /// frontend groups them by this field to offer "accept/reject/undo this
    /// whole turn" batch operations (not one-by-one), inspired by Cursor/
    /// Windsurf's checkpoint interaction but without depending on git (the
    /// undo mechanism must not be git-based, see `revert_turn`).
    pub turn_id: Uuid,
}

/// Sync info after an AI write (Accept/Undo/Redo/revert-whole-turn) lands on
/// disk -- if this path is currently open in the editor, the frontend uses
/// this to refresh that buffer, otherwise the open tab would drift out of
/// sync with the on-disk content.
#[derive(Debug, Clone, Serialize)]
pub struct FileSyncInfo {
    pub change_id: Uuid,
    pub path: String,
    pub content: String,
    pub mtime: i64,
}

/// Independent container for pending/applied file changes, deliberately
/// locked separately from the conversation-loop state (`messages` etc.) --
/// see the lock-splitting rationale in the host's original doc comment:
/// `send_message` can hold its own lock for a minute or more across many
/// tool calls, and Accept/Reject/Undo must not have to wait behind it.
pub struct ChangeStore {
    session_id: Uuid,
    workspace_root: String,
    target: CodingTarget,
    file_ops: Arc<dyn FileOps>,
    git_repo: bool,
    pub auto_git_commit: bool,
    /// "Full auto" mode's live state: when on, AI-proposed file changes
    /// write straight to disk **and** skip the normal command-confirmation
    /// gate too (`run_command`'s gate checks this field separately, not
    /// affected by `auto_apply_changes` below). Off by default -- this
    /// switch is much broader in scope than just "auto-apply file changes",
    /// turning it on by default would silently also turn off confirmation
    /// for every shell command, which isn't what `auto_apply_changes` alone
    /// asks for.
    pub full_auto: AtomicBool,
    /// Whether file changes auto-apply -- defaults on: AI-proposed changes
    /// write straight to disk, the UI only ever shows "undo", not "apply".
    /// Deliberately a separate field from `full_auto` -- `full_auto` also
    /// gates command confirmation, conflating the two would widen the risk
    /// surface beyond what "auto-apply file changes by default" asks for.
    /// Can still be turned off in the AI toolbar to fall back to the old
    /// "apply manually every time" behavior.
    pub auto_apply_changes: AtomicBool,
    /// "Auto-allow read-only commands" live state -- same reasoning as
    /// `full_auto` for using an atomic instead of a plain field.
    pub auto_allow_readonly: AtomicBool,
    changes: Vec<FileChange>,
    undo_stack: Vec<FileChange>,
}

impl ChangeStore {
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn new(
        session_id: Uuid,
        workspace_root: String,
        target: CodingTarget,
        file_ops: Arc<dyn FileOps>,
        git_repo: bool,
    ) -> Self {
        Self {
            session_id,
            workspace_root,
            target,
            file_ops,
            git_repo,
            auto_git_commit: false,
            full_auto: AtomicBool::new(false),
            auto_apply_changes: AtomicBool::new(true),
            auto_allow_readonly: AtomicBool::new(false),
            changes: Vec::new(),
            undo_stack: Vec::new(),
        }
    }

    pub fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    pub fn target(&self) -> &CodingTarget {
        &self.target
    }

    pub fn git_repo(&self) -> bool {
        self.git_repo
    }

    pub fn workspace_root(&self) -> &str {
        &self.workspace_root
    }

    pub fn set_git_repo(&mut self, git_repo: bool) {
        self.git_repo = git_repo;
    }

    /// Restores a change history from persistence (session resume),
    /// wholesale, not merged incrementally.
    pub fn restore(&mut self, changes: Vec<FileChange>) {
        self.changes = changes;
    }

    /// The latest still-live proposed content for a path in this session --
    /// the model may `write_file`/`edit_file` the same path several times
    /// within one turn, and a subsequent `read_file` shouldn't see "stale"
    /// on-disk content.
    pub fn pending_content_for(&self, path: &str) -> Option<String> {
        self.changes
            .iter()
            .rev()
            .find(|c| {
                c.path == path
                    && c.status != ChangeStatus::Rejected
                    && c.status != ChangeStatus::Undone
            })
            .map(|c| c.new_content.clone())
    }

    async fn write_and_commit(
        &self,
        ssh_pool: &SshConnectionPool,
        agent_pool: &AgentConnectionPool,
        app_handle: &AppHandle,
        path: &str,
        content: &str,
        expected_mtime: Option<i64>,
    ) -> Result<i64, AppError> {
        let outcome = self
            .file_ops
            .write_file(path, content, expected_mtime)
            .await?;
        let mtime = match outcome {
            WriteOutcome::Written { mtime } => mtime,
            WriteOutcome::Conflict {
                current_mtime,
                current_preview,
            } => {
                return Err(AppError::Conflict(format!(
                    "文件 {path} 已被外部修改（当前 mtime：{current_mtime}），未覆盖磁盘内容。当前内容预览：{current_preview}"
                )));
            }
        };
        if self.auto_git_commit && self.git_repo {
            let message = format!("AI 编程助手：修改 {path}");
            let result = git_ops::commit_file(
                &self.target,
                &self.workspace_root,
                path,
                &message,
                ssh_pool,
                agent_pool,
            )
            .await;
            let output = match result {
                Ok(out) => out,
                Err(e) => format!("Git 提交失败：{e}"),
            };
            let _ = app_handle.emit(
                "coding:git-commit-result",
                json!({ "sessionId": self.session_id, "path": path, "output": output }),
            );
        }
        Ok(mtime)
    }

    /// Where `write_file`/`edit_file` land -- with `full_auto` off, same
    /// behavior as before (just generates a Diff, waits for Accept, the
    /// returned `FileSyncInfo` is `None`); with it on, writes immediately
    /// and returns sync info, which the caller (`CodingSession::
    /// stage_change`) broadcasts via `coding:file-change` so any editor
    /// buffer already open for this path refreshes.
    pub async fn stage(
        &mut self,
        path: &str,
        new_content: String,
        turn_id: Uuid,
        ssh_pool: &SshConnectionPool,
        agent_pool: &AgentConnectionPool,
        app_handle: &AppHandle,
    ) -> Result<(FileChange, Option<FileSyncInfo>), AppError> {
        let (old_content, expected_mtime) = match self.changes.iter().rev().find(|c| {
            c.path == path && c.status != ChangeStatus::Rejected && c.status != ChangeStatus::Undone
        }) {
            Some(change) => (change.new_content.clone(), change.expected_mtime),
            None => match self.file_ops.read_file(path).await {
                Ok(file) => (file.text, Some(file.mtime)),
                Err(_) => (String::new(), None),
            },
        };
        let diff = generate_diff(&old_content, &new_content);
        let id = Uuid::new_v4();
        let mut change = FileChange {
            id,
            path: path.to_string(),
            old_content,
            new_content: new_content.clone(),
            diff,
            status: ChangeStatus::Pending,
            expected_mtime,
            turn_id,
        };

        let sync = if self.full_auto.load(Ordering::Relaxed)
            || self.auto_apply_changes.load(Ordering::Relaxed)
        {
            let mtime = self
                .write_and_commit(
                    ssh_pool,
                    agent_pool,
                    app_handle,
                    path,
                    &new_content,
                    expected_mtime,
                )
                .await?;
            change.status = ChangeStatus::Applied;
            change.expected_mtime = Some(mtime);
            self.undo_stack.clear();
            Some(FileSyncInfo {
                change_id: id,
                path: path.to_string(),
                content: new_content,
                mtime,
            })
        } else {
            None
        };

        self.changes.push(change.clone());
        Ok((change, sync))
    }

    /// User clicks FileChangeCard's "apply": only now does it actually
    /// write to disk (`expected_mtime` passed as `None` to force an
    /// overwrite -- the user has already explicitly confirmed this Diff in
    /// the UI, no need for the mtime-conflict check that exists for
    /// "concurrent with some other edit" scenarios).
    pub async fn accept(
        &mut self,
        change_id: Uuid,
        ssh_pool: &SshConnectionPool,
        agent_pool: &AgentConnectionPool,
        app_handle: &AppHandle,
    ) -> Result<FileSyncInfo, AppError> {
        let (path, content, expected_mtime) = {
            let change = self
                .changes
                .iter()
                .find(|c| c.id == change_id)
                .ok_or_else(|| AppError::NotFound(format!("change not found: {change_id}")))?;
            if change.status != ChangeStatus::Pending {
                return Err(AppError::Conflict(format!(
                    "change {change_id} is not pending"
                )));
            }
            (
                change.path.clone(),
                change.new_content.clone(),
                change.expected_mtime,
            )
        };
        let mtime = self
            .write_and_commit(
                ssh_pool,
                agent_pool,
                app_handle,
                &path,
                &content,
                expected_mtime,
            )
            .await?;
        if let Some(change) = self.changes.iter_mut().find(|c| c.id == change_id) {
            change.status = ChangeStatus::Applied;
            change.expected_mtime = Some(mtime);
        }
        self.undo_stack.clear();
        Ok(FileSyncInfo {
            change_id,
            path,
            content,
            mtime,
        })
    }

    pub fn reject(&mut self, change_id: Uuid) -> Result<(), AppError> {
        let change = self
            .changes
            .iter_mut()
            .find(|c| c.id == change_id)
            .ok_or_else(|| AppError::NotFound(format!("change not found: {change_id}")))?;
        if change.status != ChangeStatus::Pending {
            return Err(AppError::Conflict(format!(
                "change {change_id} is not pending"
            )));
        }
        change.status = ChangeStatus::Rejected;
        Ok(())
    }

    pub async fn undo(&mut self, change_id: Uuid) -> Result<FileSyncInfo, AppError> {
        let (path, old_content, expected_mtime) = {
            let change = self
                .changes
                .iter()
                .find(|c| c.id == change_id)
                .ok_or_else(|| AppError::NotFound(format!("change not found: {change_id}")))?;
            if change.status != ChangeStatus::Applied {
                return Err(AppError::Conflict(format!(
                    "change {change_id} is not applied"
                )));
            }
            (
                change.path.clone(),
                change.old_content.clone(),
                change.expected_mtime,
            )
        };
        let outcome = self
            .file_ops
            .write_file(&path, &old_content, expected_mtime)
            .await?;
        let mtime = match outcome {
            WriteOutcome::Written { mtime } => mtime,
            WriteOutcome::Conflict {
                current_mtime,
                current_preview,
            } => {
                return Err(AppError::Conflict(format!(
                    "文件 {path} 已被外部修改（当前 mtime：{current_mtime}），未执行撤销。当前内容预览：{current_preview}"
                )));
            }
        };
        let change = self
            .changes
            .iter_mut()
            .find(|c| c.id == change_id)
            .expect("checked above");
        change.status = ChangeStatus::Undone;
        change.expected_mtime = Some(mtime);
        self.undo_stack.push(change.clone());
        Ok(FileSyncInfo {
            change_id,
            path,
            content: old_content,
            mtime,
        })
    }

    /// Standard editor behavior: once a new, actually-landed write happens
    /// after an undo (`accept`), the existing redo branch is invalidated --
    /// otherwise redo could write back content that's since been
    /// overwritten (see `undo_stack.clear()` in `accept`).
    pub async fn redo(&mut self) -> Result<Option<FileSyncInfo>, AppError> {
        let Some(mut change) = self.undo_stack.last().cloned() else {
            return Ok(None);
        };
        let outcome = self
            .file_ops
            .write_file(&change.path, &change.new_content, change.expected_mtime)
            .await?;
        let mtime = match outcome {
            WriteOutcome::Written { mtime } => mtime,
            WriteOutcome::Conflict {
                current_mtime,
                current_preview,
            } => {
                return Err(AppError::Conflict(format!(
                    "文件 {} 已被外部修改（当前 mtime：{current_mtime}），未执行重做。当前内容预览：{current_preview}",
                    change.path
                )));
            }
        };
        self.undo_stack.pop();
        change.status = ChangeStatus::Applied;
        change.expected_mtime = Some(mtime);
        let id = change.id;
        let path = change.path.clone();
        let content = change.new_content.clone();
        if let Some(existing) = self.changes.iter_mut().find(|c| c.id == id) {
            *existing = change;
        }
        Ok(Some(FileSyncInfo {
            change_id: id,
            path,
            content,
            mtime,
        }))
    }

    /// Undoes every already-applied change from one conversation turn
    /// (git-independent). Walks ids in reverse time order: if the same file
    /// was changed twice within the turn, the later change must be undone
    /// before the earlier one to correctly step back to the content from
    /// before the turn started.
    pub async fn revert_turn(&mut self, turn_id: Uuid) -> Result<Vec<FileSyncInfo>, AppError> {
        let ids: Vec<Uuid> = self
            .changes
            .iter()
            .rev()
            .filter(|c| c.turn_id == turn_id && c.status == ChangeStatus::Applied)
            .map(|c| c.id)
            .collect();
        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            results.push(self.undo(id).await?);
        }
        Ok(results)
    }
}
