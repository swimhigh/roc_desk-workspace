//! Local-only Git integration for the workspace's Git panel, ported from the
//! host's `src-tauri/src/coding/git_ops.rs` with the SSH/Agent remote
//! branches dropped (this tool doesn't own a connection pool -- see the
//! module doc in `lib.rs`).
//!
//! Unlike the host's version (which shelled out through `cmd.exe`/a remote
//! shell and had to quote arguments as a single command string), this talks
//! to `git` directly via argv, so there's no quoting to get right.

use roc_desk_core::error::AppError;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

async fn run_git(cwd: &str, args: &[&str]) -> Result<String, AppError> {
    let cwd = cwd.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || -> Result<String, AppError> {
        let mut cmd = std::process::Command::new("git");
        cmd.args(&args).current_dir(&cwd);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let output = cmd
            .output()
            .map_err(|e| AppError::Internal(format!("git 执行失败: {e}")))?;
        let mut text = String::from_utf8_lossy(&output.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok(text)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
}

/// Probes whether `cwd` is inside a Git repository -- the frontend should
/// disable "auto commit" up front rather than let the user turn it on and
/// discover it doesn't apply on first use.
pub async fn is_git_repo(cwd: &str) -> bool {
    match run_git(cwd, &["rev-parse", "--is-inside-work-tree"]).await {
        Ok(out) => out.trim() == "true",
        Err(_) => false,
    }
}

/// Commits a single file's changes, returning `git commit`'s raw output
/// (success, "nothing to commit", or a failure such as missing
/// `user.name`/`user.email`, all show up as text here). Deliberately not
/// parsed into a success/failure boolean -- git's own exit status isn't
/// captured by this "merge stdout+stderr into text" helper, so guessing from
/// text content would be "usually right, occasionally silently wrong"; the
/// raw output is shown to the user instead.
pub async fn commit_file(cwd: &str, path: &str, message: &str) -> Result<String, AppError> {
    run_git(cwd, &["add", "--", path]).await?;
    run_git(cwd, &["commit", "-m", message]).await
}

pub async fn commit_paths(cwd: &str, paths: &[String], message: &str) -> Result<String, AppError> {
    let mut add_args: Vec<&str> = vec!["add", "--"];
    add_args.extend(paths.iter().map(|p| p.as_str()));
    run_git(cwd, &add_args).await?;
    run_git(cwd, &["commit", "-m", message]).await
}

/// `--porcelain=v1`: a stable, machine-readable two-column status format,
/// independent of git version/locale.
pub async fn status(cwd: &str, path: Option<&str>) -> Result<String, AppError> {
    let mut args = vec!["status", "--porcelain=v1"];
    if let Some(p) = path {
        args.push("--");
        args.push(p);
    }
    let out = run_git(cwd, &args).await?;
    Ok(if out.trim().is_empty() {
        "工作区干净，没有未提交的改动".to_string()
    } else {
        out
    })
}

/// Unstaged diff only (no `--cached`), optionally scoped to `path`.
pub async fn diff(cwd: &str, path: Option<&str>) -> Result<String, AppError> {
    let mut args = vec!["diff"];
    if let Some(p) = path {
        args.push("--");
        args.push(p);
    }
    let out = run_git(cwd, &args).await?;
    Ok(if out.trim().is_empty() {
        "没有未暂存的改动（可能已经全部 add 过，或者本来就没有改动）".to_string()
    } else {
        out
    })
}

/// Recent commit log, `--oneline` for a compact, easy-to-scan history panel.
pub async fn log(cwd: &str, limit: u32) -> Result<String, AppError> {
    run_git(
        cwd,
        &["log", "--oneline", "-n", &limit.to_string(), "--decorate"],
    )
    .await
}

/// Current branch name (empty string in detached HEAD).
pub async fn current_branch(cwd: &str) -> Result<String, AppError> {
    let out = run_git(cwd, &["branch", "--show-current"]).await?;
    Ok(out.trim().to_string())
}
