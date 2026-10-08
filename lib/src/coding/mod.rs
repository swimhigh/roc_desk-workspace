//! AI coding agent building blocks, ported from the host's `coding::*`
//! modules. See `docs/MULTI_REPO_SPLIT_PROGRESS.md` ("AI 编程助手迁移")
//! in the host repository for the full migration history -- this module is
//! assembled incrementally, piece by piece, in dependency order (target ->
//! local_exec -> diff/guard/permission -> git_ops -> ... -> session, the
//! last of which doesn't exist here yet).

pub mod audit;
pub mod changes;
pub mod diff;
pub mod evidence;
pub mod git_ops;
pub mod guard;
pub mod local_exec;
pub mod mcp;
pub mod permission;
pub mod session;
pub mod skills;
pub mod target;
pub mod tools;
pub mod webfetch;

pub use target::CodingTarget;
