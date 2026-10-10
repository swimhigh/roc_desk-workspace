use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use roc_desk_core::db::DbPool;
use roc_desk_core::error::AppError;

/// 2026-10 用户明确要求（ported from the host's identical change in
/// `roc_desk`'s `coding_history_repo.rs`）: this tool's AI coding agent
/// history content (`timeline`/`changes`/`messages`) should live only in the
/// workspace's own `.rock_desk` subdirectory, never mirrored in full into
/// this process's local `coding_history` table -- the earlier design copied
/// these (potentially tens/hundreds of MB) blobs into local SQLite purely so
/// "list history" could render without a network round trip, but the real
/// cost was keeping two copies of the same content forever.
///
/// This table now only holds the summary -- title/provider/model/mode/
/// timestamps -- so "open history list" doesn't need to scan the workspace
/// directory every time (especially costly for remote workspaces, where
/// listing a directory is itself a network round trip). The actual content
/// has exactly one copy, in the `WorkspaceHistorySnapshot` written to the
/// workspace directory file; it's only read when a specific history entry is
/// opened (local workspace: local disk read; remote workspace: SFTP/Agent
/// round trip, see `coding::commands::load_history_snapshot`) -- accepting
/// "opening a specific history entry may take a moment" in exchange for
/// local storage no longer growing without bound.
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
    /// the live session's latest `messages` before persisting. This content
    /// only ever gets written to the workspace mirror file, never into this
    /// table (see module docs above).
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

/// Looks up which workspace a history id belongs to -- needed before the
/// command layer can go read that workspace's `.rock_desk/sessions/{id}.json`
/// mirror file for the actual content. `summary` comes along so the frontend
/// still has something to show (title/provider/etc.) even if the workspace
/// turns out to be unreachable.
#[derive(Debug, Clone, Serialize)]
pub struct CodingHistoryLocation {
    pub workspace_id: Uuid,
    pub summary: CodingHistorySummary,
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

        // One-time reclaim for installs upgrading from the old "mirror full
        // content locally" design -- this crate has no migration-file system
        // (every `ensure_schema` here is `CREATE TABLE IF NOT EXISTS`, run on
        // every startup), so the cleanup lives here instead, gated by an
        // `EXISTS` check that makes every call after the first a cheap no-op
        // scan rather than repeating the `UPDATE`+`VACUUM` on every launch.
        let has_legacy_content: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM coding_history WHERE timeline_json <> '' OR changes_json <> '' OR messages_json <> '[]')",
            [],
            |r| r.get(0),
        )?;
        if has_legacy_content {
            conn.execute_batch(
                "UPDATE coding_history SET timeline_json = '', changes_json = '', messages_json = '[]'; VACUUM;",
            )?;
        }
        Ok(())
    }

    /// Only persists the summary columns -- `timeline_json`/`changes_json`/
    /// `messages_json` stay at their empty placeholder (changing the schema
    /// to drop these `NOT NULL` columns isn't worth it just to stop writing
    /// three always-empty values).
    pub fn save(&self, input: &CodingHistoryInput) -> Result<(), AppError> {
        let conn = self.pool.get()?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO coding_history (id, workspace_id, title, provider_id, provider_label, model, mode, timeline_json, changes_json, messages_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '', '', '[]', ?8, ?8)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title, provider_id=excluded.provider_id,
             provider_label=excluded.provider_label, model=excluded.model, mode=excluded.mode,
             updated_at=excluded.updated_at",
            params![input.id.to_string(), input.workspace_id.to_string(), input.title, input.provider_id.to_string(),
                input.provider_label, input.model, input.mode, now],
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

    /// Lightweight lookup -- only reads summary columns, never the (now
    /// permanently empty) content columns. Callers use the returned
    /// `workspace_id` to go read the real content from that workspace's
    /// mirror file.
    pub fn get_location(&self, id: Uuid) -> Result<Option<CodingHistoryLocation>, AppError> {
        let conn = self.pool.get()?;
        conn.query_row(
            "SELECT workspace_id, title, provider_id, provider_label, model, mode, created_at, updated_at FROM coding_history WHERE id=?1",
            [id.to_string()],
            |r| {
                let parse_uuid = |s: String| Uuid::parse_str(&s).unwrap_or_default();
                Ok(CodingHistoryLocation {
                    workspace_id: parse_uuid(r.get(0)?),
                    summary: CodingHistorySummary {
                        id,
                        title: r.get(1)?,
                        provider_id: parse_uuid(r.get(2)?),
                        provider_label: r.get(3)?,
                        model: r.get(4)?,
                        mode: r.get(5)?,
                        created_at: r.get(6)?,
                        updated_at: r.get(7)?,
                    },
                })
            },
        )
        .optional()
        .map_err(AppError::from)
    }

    /// Reconciles the local summary cache against a snapshot read from the
    /// workspace directory -- only the summary fields (title/provider/model/
    /// mode/timestamps) are written; the snapshot's `timeline`/`changes`/
    /// `messages` are used and discarded, never persisted here. An older
    /// `updated_at` than what's already cached doesn't overwrite it, so a
    /// network hiccup/concurrent write can't race the list title back to a
    /// stale value.
    pub fn import_snapshot(&self, snapshot: &WorkspaceHistorySnapshot) -> Result<(), AppError> {
        let input = &snapshot.input;
        self.pool.get()?.execute(
            "INSERT INTO coding_history (id, workspace_id, title, provider_id, provider_label, model, mode, timeline_json, changes_json, messages_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '', '', '[]', ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title, provider_id=excluded.provider_id,
             provider_label=excluded.provider_label, model=excluded.model, mode=excluded.mode,
             updated_at=excluded.updated_at WHERE excluded.updated_at > coding_history.updated_at",
            params![input.id.to_string(), input.workspace_id.to_string(), input.title, input.provider_id.to_string(),
                input.provider_label, input.model, input.mode, snapshot.created_at, snapshot.updated_at],
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
