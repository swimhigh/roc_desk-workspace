use serde::{Deserialize, Serialize};
use uuid::Uuid;

use roc_desk_core::db::DbPool;
use roc_desk_core::error::AppError;

/// Permission rule verdict (allow/ask/deny three-state). This layer only
/// governs the `run_command`/`webfetch`/`mcp` tool dimensions, not
/// `write_file`/`edit_file` -- those already go through the Diff-Accept
/// flow, equivalent to "always ask", no need for another rule layer on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Ask => "ask",
            Decision::Deny => "deny",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "allow" => Decision::Allow,
            "deny" => Decision::Deny,
            _ => Decision::Ask,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    pub id: Uuid,
    /// One of the fixed values: `"run_command"` / `"webfetch"` / `"mcp"`.
    pub tool: String,
    /// Meaning depends on `tool`: for `run_command` matches the command
    /// text itself; `webfetch` matches the URL; `mcp` matches
    /// `"<server>:<tool>"` (`filesystem:*` allows an entire server,
    /// `filesystem:read_file` allows just one tool). Both support `*`/`?`
    /// wildcards.
    pub pattern: String,
    pub decision: Decision,
    pub enabled: bool,
    pub created_at: String,
}

/// Translates `*` (any length) / `?` (single char) wildcards into an
/// equivalent regex -- no glob crate needed for just these two wildcards.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let mut regex_src = String::from("(?s)^");
    for ch in pattern.chars() {
        match ch {
            '*' => regex_src.push_str(".*"),
            '?' => regex_src.push('.'),
            _ => regex_src.push_str(&regex::escape(&ch.to_string())),
        }
    }
    regex_src.push('$');
    regex::Regex::new(&regex_src)
        .map(|re| re.is_match(text))
        .unwrap_or(false)
}

/// Permission rules persistence (`permission_rules` table), mirroring the
/// host's `db::repo::permission_rules_repo::PermissionRulesRepo`.
pub struct PermissionRulesRepo {
    pool: DbPool,
}

impl PermissionRulesRepo {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub fn ensure_schema(&self) -> Result<(), AppError> {
        let conn = self.pool.get()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS permission_rules (
                id TEXT PRIMARY KEY,
                tool TEXT NOT NULL,
                pattern TEXT NOT NULL,
                decision TEXT NOT NULL,
                enabled INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );",
        )?;
        Ok(())
    }

    pub fn create(&self, rule: &PermissionRule) -> Result<(), AppError> {
        let conn = self.pool.get()?;
        conn.execute(
            "INSERT INTO permission_rules (id, tool, pattern, decision, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                rule.id.to_string(),
                rule.tool,
                rule.pattern,
                rule.decision.as_str(),
                rule.enabled as i64,
                rule.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: Uuid) -> Result<(), AppError> {
        let conn = self.pool.get()?;
        conn.execute(
            "DELETE FROM permission_rules WHERE id = ?1",
            rusqlite::params![id.to_string()],
        )?;
        Ok(())
    }

    /// Returns rows ordered by `created_at` ascending -- the caller
    /// (`PermissionEngine::decide`) walks them in reverse so later-created
    /// rules win, no separate priority field needed.
    pub fn list(&self) -> Result<Vec<PermissionRule>, AppError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT id, tool, pattern, decision, enabled, created_at
             FROM permission_rules WHERE enabled = 1 ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map([], Self::map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn map_row(row: &rusqlite::Row) -> rusqlite::Result<PermissionRule> {
        let id: String = row.get(0)?;
        let decision: String = row.get(3)?;
        Ok(PermissionRule {
            id: Uuid::parse_str(&id).unwrap_or_else(|_| Uuid::nil()),
            tool: row.get(1)?,
            pattern: row.get(2)?,
            decision: Decision::from_str(&decision),
            enabled: row.get::<_, i64>(4)? != 0,
            created_at: row.get(5)?,
        })
    }
}

/// A per-tool-call snapshot of the rules -- `CodingSession::send_message`
/// takes a fresh one before every tool invocation (not one shared, cached
/// snapshot for the whole conversation loop), so rule changes (add/remove)
/// take effect immediately within a turn that's still running, no need to
/// wait for the next user message or build a cache-invalidation mechanism.
pub struct PermissionEngine {
    rules: Vec<PermissionRule>,
}

impl PermissionEngine {
    pub fn load(repo: &PermissionRulesRepo) -> Result<Self, AppError> {
        Ok(Self {
            rules: repo.list()?,
        })
    }

    /// Later-created rules win (`rules` is already ascending by
    /// `created_at`, searched in reverse here), so a newly added rule
    /// always overrides an older one without a separate priority field. No
    /// matching rule returns `None`, and the caller falls back to each
    /// tool's existing default policy (allowlist/always-ask/...).
    pub fn decide(&self, tool: &str, text: &str) -> Option<Decision> {
        self.rules
            .iter()
            .rev()
            .find(|rule| rule.enabled && rule.tool == tool && wildcard_match(&rule.pattern, text))
            .map(|rule| rule.decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matches_prefix_pattern() {
        assert!(wildcard_match("git *", "git status"));
        assert!(wildcard_match("git *", "git push origin main"));
        assert!(!wildcard_match("git *", "npm install"));
    }

    #[test]
    fn wildcard_matches_exact_and_question_mark() {
        assert!(wildcard_match("ls", "ls"));
        assert!(!wildcard_match("ls", "ls -la"));
        assert!(wildcard_match("rm f??.txt", "rm foo.txt"));
    }

    fn rule(tool: &str, pattern: &str, decision: Decision, created_at: &str) -> PermissionRule {
        PermissionRule {
            id: Uuid::new_v4(),
            tool: tool.into(),
            pattern: pattern.into(),
            decision,
            enabled: true,
            created_at: created_at.into(),
        }
    }

    #[test]
    fn later_rule_wins_over_earlier_conflicting_rule() {
        let engine = PermissionEngine {
            rules: vec![
                rule("run_command", "git *", Decision::Allow, "2026-01-01"),
                rule("run_command", "git push *", Decision::Ask, "2026-01-02"),
            ],
        };
        assert_eq!(
            engine.decide("run_command", "git push origin main"),
            Some(Decision::Ask)
        );
        assert_eq!(
            engine.decide("run_command", "git status"),
            Some(Decision::Allow)
        );
    }

    #[test]
    fn no_match_returns_none() {
        let engine = PermissionEngine {
            rules: vec![rule("run_command", "git *", Decision::Allow, "2026-01-01")],
        };
        assert_eq!(engine.decide("run_command", "npm install"), None);
    }

    #[test]
    fn disabled_rule_is_ignored() {
        let mut disabled = rule("run_command", "git *", Decision::Allow, "2026-01-01");
        disabled.enabled = false;
        let engine = PermissionEngine {
            rules: vec![disabled],
        };
        assert_eq!(engine.decide("run_command", "git status"), None);
    }
}
