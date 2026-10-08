use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Which machine/connection an AI coding session's tools (file ops, `git`,
/// `run_command`) act against. Ported verbatim from the host's
/// `coding::session::CodingTarget`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CodingTarget {
    Local,
    Remote {
        connection_id: Uuid,
        host_label: String,
    },
    /// Remote Windows Agent workspace (AGENT_DESIGN.md §四.4): `run_command`/
    /// `search_files` go through the Agent protocol instead of SSH `exec`/
    /// `grep`, and command syntax is native Windows (`cmd.exe /C` + an
    /// argument array, not "a hand-assembled POSIX shell string").
    Agent {
        connection_id: Uuid,
        host_label: String,
    },
}
