use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

use roc_desk_core::db::DbPool;
use roc_desk_core::error::AppError;

/// Command execution audit log (every `run_command` attempt gets recorded,
/// including blocked/rejected ones, with timestamp/target host/command/
/// outcome/AI session id, reviewable/exportable from settings).
pub struct AuditLogRepo {
    pool: DbPool,
}

impl AuditLogRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub fn ensure_schema(&self) -> Result<(), AppError> {
        let conn = self.pool.get()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS command_audit_log (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                target_label TEXT NOT NULL,
                command TEXT NOT NULL,
                outcome TEXT NOT NULL,
                output_summary TEXT,
                created_at TEXT NOT NULL
            );",
        )?;
        Ok(())
    }

    /// Records one `run_command` attempt -- a best-effort write: whether the
    /// command was blacklisted, rejected by the user, or actually executed,
    /// it should leave a trace, but a failure to write the audit log itself
    /// must not block the command's own execution flow, so failures are only
    /// logged via `tracing::warn!`, never propagated.
    ///
    /// Runs on `spawn_blocking` rather than inline: a connection-pool
    /// contention/SQLite-busy wait here would otherwise block the calling
    /// tokio task itself (synchronous I/O never yields back to the
    /// executor), which can starve an enclosing `tokio::time::timeout` on
    /// the caller's command-execution path from ever being polled.
    pub fn record(
        &self,
        session_id: Uuid,
        target_label: &str,
        command: &str,
        outcome: &str,
        output_summary: Option<&str>,
    ) {
        let pool = self.pool.clone();
        let target_label = target_label.to_string();
        let command = command.to_string();
        let outcome = outcome.to_string();
        let output_summary = output_summary.map(|s| s.to_string());
        tokio::task::spawn_blocking(move || {
            let result = (|| -> Result<(), AppError> {
                let conn = pool.get()?;
                conn.execute(
                    "INSERT INTO command_audit_log (id, session_id, target_label, command, outcome, output_summary, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        Uuid::new_v4().to_string(),
                        session_id.to_string(),
                        target_label,
                        command,
                        outcome,
                        output_summary,
                        Utc::now().to_rfc3339(),
                    ],
                )?;
                Ok(())
            })();
            if let Err(e) = result {
                tracing::warn!("failed to write command audit log: {e}");
            }
        });
    }
}
