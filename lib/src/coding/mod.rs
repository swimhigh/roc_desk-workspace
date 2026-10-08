//! AI coding agent building blocks, ported from the host's `coding::*`
//! modules. See `docs/MULTI_REPO_SPLIT_PROGRESS.md` ("AI 编程助手迁移")
//! in the host repository for the full migration history -- this module is
//! assembled incrementally, piece by piece, in dependency order (target ->
//! local_exec -> diff/guard/permission -> git_ops -> ... -> session, the
//! last of which doesn't exist here yet).

pub mod audit;
pub mod commands;
pub mod evidence;
pub mod git_ops;
pub mod guard;
pub mod history;
pub mod local_exec;
pub mod mcp;
pub mod permission;
pub mod session;
pub mod skills;
pub mod tools;
pub mod webfetch;

/// `ChangeStore`/`FileChange`/`diff`/`CodingTarget` live in
/// `roc_desk_common::change_store` -- the host's SQL AI assist panel shares
/// this exact same Diff/Accept/Undo primitive with the coding agent, so it
/// moved to the common crate rather than staying tool-specific here. See
/// `coding::git_ops::SshGitCommitter` for this crate's `GitCommitter`
/// implementation (the one piece `change_store` itself can't provide,
/// since it must never depend on `roc_desk-ssh`).
pub use roc_desk_common::change_store::{
    ChangeStatus, ChangeStore, CodingTarget, FileChange, FileSyncInfo, GitCommitOutcome,
};
pub use session::{CodingMode, CodingSession, PendingInjection};
