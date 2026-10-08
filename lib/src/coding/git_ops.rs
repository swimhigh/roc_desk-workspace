use std::sync::Arc;

use async_trait::async_trait;
use roc_desk_common::change_store::{CodingTarget, GitCommitter};
use roc_desk_core::error::AppError;
use roc_desk_ssh::agent::AgentConnectionPool;
use roc_desk_ssh::ssh::SshConnectionPool;

use super::local_exec::{run_local_command, shell_quote};

/// AI coding agent's Git integration: commits exactly once per Accept'd
/// change, nothing more -- no branch management, no conflict handling, no
/// push. Those need an explicit user decision; automation stops at
/// "recording a change you've already explicitly confirmed into history".
///
/// Remote execution opens a new channel per `exec` with no persistent cwd
/// like an interactive shell, so every command has to assemble its own
/// `cd <dir> && git ...`; local execution just uses `run_local_command`'s
/// `current_dir`. `args` are raw, unquoted git subcommand words -- the
/// Local/SSH branches still join them into one string for `sh -c`/the
/// remote shell (`shell_quote` escapes per POSIX rules); the Agent branch
/// passes `args` directly as a `CreateProcess` argument array to `git.exe`,
/// bypassing shell parsing entirely, so there's no POSIX-escaping-on-a-
/// Windows-target mismatch to worry about (AGENT_DESIGN.md §一).
async fn run_git(
    target: &CodingTarget,
    cwd: &str,
    args: &[&str],
    ssh_pool: &SshConnectionPool,
    agent_pool: &AgentConnectionPool,
) -> Result<String, AppError> {
    match target {
        CodingTarget::Local => {
            let quoted = args
                .iter()
                .map(|a| shell_quote(a))
                .collect::<Vec<_>>()
                .join(" ");
            run_local_command(&format!("git {quoted}"), cwd).await
        }
        CodingTarget::Remote { connection_id, .. } => {
            let session = ssh_pool.get_or_connect(*connection_id).await?;
            let quoted_cwd = shell_quote(cwd);
            let quoted = args
                .iter()
                .map(|a| shell_quote(a))
                .collect::<Vec<_>>()
                .join(" ");
            session
                .exec(&format!("cd {quoted_cwd} && git {quoted}"))
                .await
        }
        // The Agent `Exec` request natively carries a `cwd` field, no need
        // to hand-assemble `cd <dir> &&` like SSH -- a concrete instance of
        // AGENT_DESIGN.md §四.4's "execution primitives are designed
        // natively around Windows semantics".
        CodingTarget::Agent { connection_id, .. } => {
            let session = agent_pool.get_or_connect(*connection_id).await?;
            let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            session.exec_argv("git", &argv, cwd).await
        }
    }
}

/// Probes whether the workspace root is inside a Git repo -- the frontend
/// should disable "auto-commit" entirely when it's not (or the target
/// machine doesn't have git installed), rather than letting the user enable
/// it and only discover it doesn't work on the first Accept.
pub async fn is_git_repo(
    target: &CodingTarget,
    cwd: &str,
    ssh_pool: &SshConnectionPool,
    agent_pool: &AgentConnectionPool,
) -> bool {
    match run_git(
        target,
        cwd,
        &["rev-parse", "--is-inside-work-tree"],
        ssh_pool,
        agent_pool,
    )
    .await
    {
        Ok(out) => out.trim() == "true",
        Err(_) => false,
    }
}

/// Commits a single file's change, returning `git commit`'s raw output
/// (success, "nothing to commit", or a failure like missing
/// `user.name`/`user.email` -- all show up in this text). Deliberately not
/// parsed to infer success/failure: `SshSession::exec`/`run_local_command`
/// don't expose an exit code (only combined stdout+stderr text), and
/// string-matching on top of that foundation would only be "usually right,
/// occasionally wrong in a hard-to-notice way" -- e.g. a missing-git-
/// identity error doesn't contain "nothing to commit" and would get
/// misread as success. Better to hand the raw output to the caller as-is;
/// a developer reading git's own output can judge it better than a guess.
pub async fn commit_file(
    target: &CodingTarget,
    cwd: &str,
    path: &str,
    message: &str,
    ssh_pool: &SshConnectionPool,
    agent_pool: &AgentConnectionPool,
) -> Result<String, AppError> {
    run_git(target, cwd, &["add", "--", path], ssh_pool, agent_pool).await?;
    run_git(
        target,
        cwd,
        &["commit", "-m", message],
        ssh_pool,
        agent_pool,
    )
    .await
}

/// `git_commit` tool's multi-path version -- one `add` of several explicit
/// paths then one commit. Deliberately doesn't share code with
/// `commit_file` (used when the AI applies a single change and auto-commits
/// it): here the path list is model-specified, semantically "commit these
/// specific files", not "commit the change that just happened".
pub async fn commit_paths(
    target: &CodingTarget,
    cwd: &str,
    paths: &[String],
    message: &str,
    ssh_pool: &SshConnectionPool,
    agent_pool: &AgentConnectionPool,
) -> Result<String, AppError> {
    let mut add_args: Vec<&str> = vec!["add", "--"];
    add_args.extend(paths.iter().map(|p| p.as_str()));
    run_git(target, cwd, &add_args, ssh_pool, agent_pool).await?;
    run_git(target, cwd, &["commit", "-m", message], ssh_pool, agent_pool).await
}

/// `git_status` tool -- `--porcelain=v1` is a fixed two-char-status + path
/// machine-readable format, easier for the model to parse than the default
/// human-readable output, and doesn't vary with git version/locale.
pub async fn status(
    target: &CodingTarget,
    cwd: &str,
    path: Option<&str>,
    ssh_pool: &SshConnectionPool,
    agent_pool: &AgentConnectionPool,
) -> Result<String, AppError> {
    let mut args = vec!["status", "--porcelain=v1"];
    if let Some(p) = path {
        args.push("--");
        args.push(p);
    }
    let out = run_git(target, cwd, &args, ssh_pool, agent_pool).await?;
    Ok(if out.trim().is_empty() {
        "工作区干净，没有未提交的改动".to_string()
    } else {
        out
    })
}

/// `git_diff` tool -- unstaged changes only (no `--cached`), same optional
/// `path` narrowing as `git_status`, to avoid stuffing a large unrelated
/// diff into the model's context in a big repo.
pub async fn diff(
    target: &CodingTarget,
    cwd: &str,
    path: Option<&str>,
    ssh_pool: &SshConnectionPool,
    agent_pool: &AgentConnectionPool,
) -> Result<String, AppError> {
    let mut args = vec!["diff"];
    if let Some(p) = path {
        args.push("--");
        args.push(p);
    }
    let out = run_git(target, cwd, &args, ssh_pool, agent_pool).await?;
    Ok(if out.trim().is_empty() {
        "没有未暂存的改动（可能已经全部 add 过，或者本来就没有改动）".to_string()
    } else {
        out
    })
}

/// This crate's [`GitCommitter`] implementation for `roc_desk_common::change_store::ChangeStore`
/// -- the whole reason that trait exists is so `roc_desk_common` never needs
/// a direct dependency on `roc_desk-ssh`; this is where the two actually
/// get wired together, using the exact same connection pools the rest of
/// this crate's coding-agent tools already share.
pub struct SshGitCommitter {
    ssh_pool: Arc<SshConnectionPool>,
    agent_pool: Arc<AgentConnectionPool>,
}

impl SshGitCommitter {
    pub fn new(ssh_pool: Arc<SshConnectionPool>, agent_pool: Arc<AgentConnectionPool>) -> Self {
        Self {
            ssh_pool,
            agent_pool,
        }
    }
}

#[async_trait]
impl GitCommitter for SshGitCommitter {
    async fn commit_file(
        &self,
        target: &CodingTarget,
        workspace_root: &str,
        path: &str,
        message: &str,
    ) -> Result<String, AppError> {
        commit_file(
            target,
            workspace_root,
            path,
            message,
            &self.ssh_pool,
            &self.agent_pool,
        )
        .await
    }
}
