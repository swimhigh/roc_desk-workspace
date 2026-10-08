use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use roc_desk_core::db::DbPool;
use roc_desk_core::error::AppError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingHistoryInput {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
    pub provider_id: Uuid,
    pub provider_label: String,
    pub model: String,
    pub mode: String,
    pub timeline: serde_json::Value,
    pub changes: serde_json::Value,
    /// The real conversation context sent to the AI (`CodingSession`'s
    /// internal `messages`) -- the frontend doesn't know about and doesn't
    /// need to fill this field in (`#[serde(default)]`: deserializes fine
    /// without this key present), `coding_history_save` overwrites it with
    /// the live session's latest `messages` before persisting. Resuming a
    /// history entry uses this to genuinely continue the conversation,
    /// rather than only replaying `changes`/`timeline`.
    #[serde(default)]
    pub messages: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodingHistorySummary {
    pub id: Uuid,
    pub title: String,
    pub provider_id: Uuid,
    pub provider_label: String,
    pub model: String,
    pub mode: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodingHistoryDetail {
    #[serde(flatten)]
    pub summary: CodingHistorySummary,
    pub workspace_id: Uuid,
    pub timeline: serde_json::Value,
    pub changes: serde_json::Value,
    /// Never serialized to the frontend over IPC (it has no need to, and
    /// shouldn't, see raw LLM message context directly) -- only read
    /// internally by `coding_history_resume` to rebuild a session.
    #[serde(skip)]
    pub messages: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceHistorySnapshot {
    pub input: CodingHistoryInput,
    pub created_at: String,
    pub updated_at: String,
}

pub struct CodingHistoryRepo {
    pool: DbPool,
}

impl CodingHistoryRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub fn ensure_schema(&self) -> Result<(), AppError> {
        let conn = self.pool.get()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS coding_history (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                title TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                provider_label TEXT NOT NULL,
                model TEXT NOT NULL,
                mode TEXT NOT NULL,
                timeline_json TEXT NOT NULL,
                changes_json TEXT NOT NULL,
                messages_json TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_coding_history_workspace_updated
            ON coding_history(workspace_id, updated_at DESC);",
        )?;
        Ok(())
    }

    pub fn save(&self, input: &CodingHistoryInput) -> Result<(), AppError> {
        let conn = self.pool.get()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO coding_history (id, workspace_id, title, provider_id, provider_label, model, mode, timeline_json, changes_json, messages_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title, provider_id=excluded.provider_id,
             provider_label=excluded.provider_label, model=excluded.model, mode=excluded.mode,
             timeline_json=excluded.timeline_json, changes_json=excluded.changes_json,
             messages_json=excluded.messages_json, updated_at=excluded.updated_at",
            params![input.id.to_string(), input.workspace_id.to_string(), input.title, input.provider_id.to_string(),
                input.provider_label, input.model, input.mode, input.timeline.to_string(), input.changes.to_string(),
                input.messages.to_string(), now],
        )?;
        Ok(())
    }

    pub fn list(&self, workspace_id: Uuid) -> Result<Vec<CodingHistorySummary>, AppError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare("SELECT id, title, provider_id, provider_label, model, mode, created_at, updated_at FROM coding_history WHERE workspace_id=?1 ORDER BY updated_at DESC")?;
        let rows = stmt
            .query_map([workspace_id.to_string()], |r| {
                Ok(CodingHistorySummary {
                    id: Uuid::parse_str(&r.get::<_, String>(0)?).unwrap_or_default(),
                    title: r.get(1)?,
                    provider_id: Uuid::parse_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                    provider_label: r.get(3)?,
                    model: r.get(4)?,
                    mode: r.get(5)?,
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get(&self, id: Uuid) -> Result<Option<CodingHistoryDetail>, AppError> {
        let conn = self.pool.get()?;
        conn.query_row("SELECT workspace_id, title, provider_id, provider_label, model, mode, timeline_json, changes_json, created_at, updated_at, messages_json FROM coding_history WHERE id=?1", [id.to_string()], |r| {
            let parse_uuid = |s: String| Uuid::parse_str(&s).unwrap_or_default();
            let timeline: String = r.get(6)?;
            let changes: String = r.get(7)?;
            let messages: String = r.get(10)?;
            Ok(CodingHistoryDetail {
                summary: CodingHistorySummary { id, title: r.get(1)?, provider_id: parse_uuid(r.get(2)?), provider_label: r.get(3)?, model: r.get(4)?, mode: r.get(5)?, created_at: r.get(8)?, updated_at: r.get(9)? },
                workspace_id: parse_uuid(r.get(0)?),
                timeline: serde_json::from_str(&timeline).unwrap_or_default(), changes: serde_json::from_str(&changes).unwrap_or_default(),
                messages: serde_json::from_str(&messages).unwrap_or_default(),
            })
        }).optional().map_err(AppError::from)
    }

    pub fn import_snapshot(&self, snapshot: &WorkspaceHistorySnapshot) -> Result<(), AppError> {
        let input = &snapshot.input;
        self.pool.get()?.execute(
            "INSERT INTO coding_history (id, workspace_id, title, provider_id, provider_label, model, mode, timeline_json, changes_json, messages_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title, provider_id=excluded.provider_id,
             provider_label=excluded.provider_label, model=excluded.model, mode=excluded.mode,
             timeline_json=excluded.timeline_json, changes_json=excluded.changes_json,
             messages_json=excluded.messages_json,
             updated_at=excluded.updated_at WHERE excluded.updated_at > coding_history.updated_at",
            params![input.id.to_string(), input.workspace_id.to_string(), input.title, input.provider_id.to_string(),
                input.provider_label, input.model, input.mode, input.timeline.to_string(), input.changes.to_string(),
                input.messages.to_string(), snapshot.created_at, snapshot.updated_at],
        )?;
        Ok(())
    }

    pub fn rename(&self, id: Uuid, title: &str) -> Result<(), AppError> {
        self.pool.get()?.execute(
            "UPDATE coding_history SET title=?2, updated_at=?3 WHERE id=?1",
            params![id.to_string(), title, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: Uuid) -> Result<(), AppError> {
        self.pool
            .get()?
            .execute("DELETE FROM coding_history WHERE id=?1", [id.to_string()])?;
        Ok(())
    }
}
