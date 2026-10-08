use chrono::Utc;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use roc_desk_core::db::DbPool;
use roc_desk_core::error::AppError;

/// Per-entry content cap -- evidence rows feed back into the model's
/// context, so this caps a single row at the same order of magnitude as
/// other tool-result truncation limits in `coding::tools`.
pub const MAX_EVIDENCE_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceEntry {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub target_key: String,
    pub kind: String,
    pub query_hash: String,
    pub path_or_url: String,
    pub version_token: String,
    pub content_hash: String,
    pub payload_json: String,
    pub summary: String,
    pub content: String,
    pub expires_at: Option<String>,
}

/// Caches tool-result "evidence" (file snapshots, search/web-fetch results)
/// the AI coding agent has already gathered, keyed by workspace/target/kind
/// plus a version token (e.g. `mtime=...;size=...` for a file) -- lets a
/// later turn in the same conversation recall prior findings (via FTS5 full-
/// text search over `summary`/`content`) instead of re-reading/re-fetching
/// the same thing. `ai_evidence_cache` is the dedup/freshness index;
/// `ai_evidence` holds the actual summary/content rows, kept in sync with
/// the `ai_evidence_fts` virtual table by triggers.
pub struct AiEvidenceRepo {
    pool: DbPool,
}

impl AiEvidenceRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub fn ensure_schema(&self) -> Result<(), AppError> {
        let conn = self.pool.get()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS ai_evidence_cache (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                target_key TEXT NOT NULL,
                kind TEXT NOT NULL,
                query_hash TEXT NOT NULL,
                path_or_url TEXT NOT NULL,
                version_token TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                bytes INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'fresh',
                created_at TEXT NOT NULL,
                last_used_at TEXT NOT NULL,
                expires_at TEXT,
                UNIQUE(workspace_id, target_key, kind, query_hash, path_or_url, version_token)
            );
            CREATE INDEX IF NOT EXISTS idx_ai_evidence_cache_lookup
            ON ai_evidence_cache(workspace_id, target_key, kind, query_hash, status);
            CREATE INDEX IF NOT EXISTS idx_ai_evidence_cache_lru
            ON ai_evidence_cache(workspace_id, last_used_at);

            CREATE TABLE IF NOT EXISTS ai_evidence (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                target_key TEXT NOT NULL,
                kind TEXT NOT NULL,
                path_or_url TEXT NOT NULL,
                version_token TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                summary TEXT NOT NULL DEFAULT '',
                content TEXT NOT NULL,
                created_at TEXT NOT NULL,
                last_used_at TEXT NOT NULL
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS ai_evidence_fts USING fts5(
                path_or_url,
                summary,
                content,
                content='ai_evidence',
                content_rowid='rowid',
                tokenize='unicode61 remove_diacritics 2'
            );

            CREATE TRIGGER IF NOT EXISTS ai_evidence_ai AFTER INSERT ON ai_evidence BEGIN
                INSERT INTO ai_evidence_fts(rowid, path_or_url, summary, content)
                VALUES (new.rowid, new.path_or_url, new.summary, new.content);
            END;

            CREATE TRIGGER IF NOT EXISTS ai_evidence_ad AFTER DELETE ON ai_evidence BEGIN
                INSERT INTO ai_evidence_fts(ai_evidence_fts, rowid, path_or_url, summary, content)
                VALUES ('delete', old.rowid, old.path_or_url, old.summary, old.content);
            END;

            CREATE TRIGGER IF NOT EXISTS ai_evidence_au AFTER UPDATE ON ai_evidence BEGIN
                INSERT INTO ai_evidence_fts(ai_evidence_fts, rowid, path_or_url, summary, content)
                VALUES ('delete', old.rowid, old.path_or_url, old.summary, old.content);
                INSERT INTO ai_evidence_fts(rowid, path_or_url, summary, content)
                VALUES (new.rowid, new.path_or_url, new.summary, new.content);
            END;",
        )?;
        Ok(())
    }

    pub fn get_exact(
        &self,
        workspace_id: Uuid,
        target_key: &str,
        kind: &str,
        query_hash: &str,
        path_or_url: &str,
        version_token: &str,
    ) -> Result<Option<EvidenceEntry>, AppError> {
        let conn = self.pool.get()?;
        let row = conn
            .query_row(
                "SELECT c.id, c.payload_json, c.content_hash, e.summary, e.content, c.expires_at
             FROM ai_evidence_cache c JOIN ai_evidence e ON e.id=c.id
             WHERE c.workspace_id=?1 AND c.target_key=?2 AND c.kind=?3
               AND c.query_hash=?4 AND c.path_or_url=?5 AND c.version_token=?6
               AND c.status='fresh' AND (c.expires_at IS NULL OR c.expires_at > datetime('now'))",
                params![
                    workspace_id.to_string(),
                    target_key,
                    kind,
                    query_hash,
                    path_or_url,
                    version_token
                ],
                |r| {
                    Ok(EvidenceEntry {
                        id: Uuid::parse_str(&r.get::<_, String>(0)?).unwrap_or_default(),
                        workspace_id,
                        target_key: target_key.to_string(),
                        kind: kind.to_string(),
                        query_hash: query_hash.to_string(),
                        path_or_url: path_or_url.to_string(),
                        version_token: version_token.to_string(),
                        content_hash: r.get(2)?,
                        payload_json: r.get(1)?,
                        summary: r.get(3)?,
                        content: r.get(4)?,
                        expires_at: r.get(5)?,
                    })
                },
            )
            .optional()?;
        if let Some(entry) = &row {
            conn.execute(
                "UPDATE ai_evidence_cache SET last_used_at=?2 WHERE id=?1",
                params![entry.id.to_string(), Utc::now().to_rfc3339()],
            )?;
            conn.execute(
                "UPDATE ai_evidence SET last_used_at=?2 WHERE id=?1",
                params![entry.id.to_string(), Utc::now().to_rfc3339()],
            )?;
        }
        Ok(row)
    }

    pub fn get_latest_path(
        &self,
        workspace_id: Uuid,
        target_key: &str,
        path: &str,
        size: u64,
    ) -> Result<Option<EvidenceEntry>, AppError> {
        let conn = self.pool.get()?;
        let suffix = format!("%size={size}");
        let row = conn
            .query_row(
                "SELECT c.id,c.payload_json,c.content_hash,c.version_token,e.summary,e.content,c.expires_at,c.query_hash
             FROM ai_evidence_cache c JOIN ai_evidence e ON e.id=c.id
             WHERE c.workspace_id=?1 AND c.target_key=?2 AND c.kind='file_snapshot'
               AND c.path_or_url=?3 AND c.version_token LIKE ?4 AND c.status='fresh'
             ORDER BY c.last_used_at DESC LIMIT 1",
                params![workspace_id.to_string(), target_key, path, suffix],
                |r| {
                    Ok(EvidenceEntry {
                        id: Uuid::parse_str(&r.get::<_, String>(0)?).unwrap_or_default(),
                        workspace_id,
                        target_key: target_key.into(),
                        kind: "file_snapshot".into(),
                        query_hash: r.get(7)?,
                        path_or_url: path.into(),
                        version_token: r.get(3)?,
                        content_hash: r.get(2)?,
                        payload_json: r.get(1)?,
                        summary: r.get(4)?,
                        content: r.get(5)?,
                        expires_at: r.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn upsert(&self, entry: &EvidenceEntry) -> Result<(), AppError> {
        let conn = self.pool.get()?;
        let now = Utc::now().to_rfc3339();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            conn.execute(
                "INSERT INTO ai_evidence (id,workspace_id,target_key,kind,path_or_url,version_token,content_hash,summary,content,created_at,last_used_at)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)
                ON CONFLICT(id) DO UPDATE SET summary=excluded.summary,content=excluded.content,last_used_at=excluded.last_used_at",
                params![entry.id.to_string(), entry.workspace_id.to_string(), entry.target_key, entry.kind, entry.path_or_url, entry.version_token, entry.content_hash, entry.summary, entry.content, now])?;
            conn.execute(
                "INSERT INTO ai_evidence_cache (id,workspace_id,target_key,kind,query_hash,path_or_url,version_token,content_hash,payload_json,bytes,status,created_at,last_used_at,expires_at)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'fresh',?11,?11,?12)
                ON CONFLICT(workspace_id,target_key,kind,query_hash,path_or_url,version_token) DO UPDATE SET id=excluded.id,content_hash=excluded.content_hash,payload_json=excluded.payload_json,bytes=excluded.bytes,status='fresh',last_used_at=excluded.last_used_at,expires_at=excluded.expires_at",
                params![entry.id.to_string(), entry.workspace_id.to_string(), entry.target_key, entry.kind, entry.query_hash, entry.path_or_url, entry.version_token, entry.content_hash, entry.payload_json, entry.content.len() as i64, now, entry.expires_at])?;
            Ok::<(), rusqlite::Error>(())
        })();
        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e.into())
            }
        }
    }

    pub fn invalidate_path(
        &self,
        workspace_id: Uuid,
        target_key: &str,
        path: &str,
    ) -> Result<(), AppError> {
        self.pool.get()?.execute(
            "UPDATE ai_evidence_cache SET status='invalidated' WHERE workspace_id=?1 AND target_key=?2 AND path_or_url=?3",
            params![workspace_id.to_string(), target_key, path],
        )?;
        Ok(())
    }

    pub fn invalidate_target(&self, workspace_id: Uuid, target_key: &str) -> Result<(), AppError> {
        self.pool.get()?.execute(
            "UPDATE ai_evidence_cache SET status='invalidated' WHERE workspace_id=?1 AND target_key=?2",
            params![workspace_id.to_string(), target_key],
        )?;
        Ok(())
    }

    pub fn get_by_id(&self, id: Uuid) -> Result<Option<EvidenceEntry>, AppError> {
        let conn = self.pool.get()?;
        conn.query_row(
            "SELECT id,workspace_id,target_key,kind,content_hash,payload_json,path_or_url,version_token,summary,content,NULL FROM ai_evidence WHERE id=?1",
            [id.to_string()],
            |r| {
                Ok(EvidenceEntry {
                    id,
                    workspace_id: Uuid::parse_str(&r.get::<_, String>(1)?).unwrap_or_default(),
                    target_key: r.get(2)?,
                    kind: r.get(3)?,
                    query_hash: String::new(),
                    path_or_url: r.get(6)?,
                    version_token: r.get(7)?,
                    content_hash: r.get(4)?,
                    payload_json: r.get(5)?,
                    summary: r.get(8)?,
                    content: r.get(9)?,
                    expires_at: r.get(10)?,
                })
            },
        )
        .optional()
        .map_err(AppError::from)
    }

    pub fn search_fts(
        &self,
        workspace_id: Uuid,
        target_key: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(Uuid, String, String)>, AppError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT e.id,e.path_or_url,e.summary FROM ai_evidence_fts f JOIN ai_evidence e ON e.rowid=f.rowid WHERE ai_evidence_fts MATCH ?1 AND e.workspace_id=?2 AND e.target_key=?3 ORDER BY bm25(ai_evidence_fts) LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(
                params![query, workspace_id.to_string(), target_key, limit as i64],
                |r| {
                    Ok((
                        Uuid::parse_str(&r.get::<_, String>(0)?).unwrap_or_default(),
                        r.get(1)?,
                        r.get(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(name: &str) -> AiEvidenceRepo {
        let path = std::env::temp_dir().join(format!(
            "roc_desk-evidence-test-{name}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let pool = roc_desk_core::db::pool::create_pool(&path).unwrap();
        let repo = AiEvidenceRepo::new(pool);
        repo.ensure_schema().unwrap();
        repo
    }

    #[test]
    fn upsert_then_get_exact_round_trips() {
        let repo = temp_repo("upsert-exact");
        let workspace_id = Uuid::new_v4();
        let entry = EvidenceEntry {
            id: Uuid::new_v4(),
            workspace_id,
            target_key: "local".into(),
            kind: "file_snapshot".into(),
            query_hash: "qh".into(),
            path_or_url: "src/main.rs".into(),
            version_token: "mtime=1;size=10".into(),
            content_hash: "ch".into(),
            payload_json: "{}".into(),
            summary: "入口文件".into(),
            content: "fn main() {}".into(),
            expires_at: None,
        };
        repo.upsert(&entry).unwrap();
        let fetched = repo
            .get_exact(
                workspace_id,
                "local",
                "file_snapshot",
                "qh",
                "src/main.rs",
                "mtime=1;size=10",
            )
            .unwrap()
            .expect("entry should be found");
        assert_eq!(fetched.content, "fn main() {}");
    }

    #[test]
    fn search_fts_finds_matching_summary() {
        let repo = temp_repo("search-fts");
        let workspace_id = Uuid::new_v4();
        let entry = EvidenceEntry {
            id: Uuid::new_v4(),
            workspace_id,
            target_key: "local".into(),
            kind: "file_snapshot".into(),
            query_hash: "qh2".into(),
            path_or_url: "src/lib.rs".into(),
            version_token: "mtime=2;size=20".into(),
            content_hash: "ch2".into(),
            payload_json: "{}".into(),
            summary: "lib root module".into(),
            content: "pub mod coding;".into(),
            expires_at: None,
        };
        repo.upsert(&entry).unwrap();
        let hits = repo
            .search_fts(workspace_id, "local", "root", 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1, "src/lib.rs");
    }
}
