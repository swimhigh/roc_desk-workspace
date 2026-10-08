//! Local shell command execution for the AI coding agent's `run_command`
//! tool and `git_ops`'s local branch. Ported verbatim from the host's
//! `coding::session` (the free functions at the bottom of that file).

use std::collections::HashMap;

use roc_desk_core::error::AppError;

/// POSIX single-quote escaping: replaces `'` with `'\''`, wraps the whole
/// thing in single quotes -- prevents shell metacharacters (`;`, `` ` ``,
/// `$(...)`) in user-controlled strings (search keywords, git commit
/// messages) from being interpreted/executed. Command injection is an OWASP
/// Top 10 category; this is the mitigation for the local/SSH `git_ops`
/// branches that hand-assemble a shell command line.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub async fn run_local_command(command: &str, cwd: &str) -> Result<String, AppError> {
    let output = run_local_command_output(command, cwd).await?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr))
}

pub async fn run_local_command_output(
    command: &str,
    cwd: &str,
) -> Result<std::process::Output, AppError> {
    run_local_command_output_with_env(command, cwd, &HashMap::new()).await
}

/// A GUI process with no console (like `roc_desk.exe`) spawning a
/// `cmd.exe`/`powershell.exe` child would otherwise briefly flash a visible
/// console window by default on Windows.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// If `command`'s first whitespace-delimited token is `powershell.exe`/
/// `pwsh.exe` (with or without a full path), the caller has already
/// assembled "executable + its own full argument string" -- returns
/// `(executable, remaining argument text as-is)` so the caller can spawn it
/// directly, bypassing `cmd.exe /C`'s mangling of embedded newlines.
#[cfg(target_os = "windows")]
fn split_direct_shell_invocation(command: &str) -> Option<(&str, &str)> {
    let trimmed = command.trim_start();
    let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let exe = &trimmed[..end];
    let exe_lower = exe.trim_matches('"').to_ascii_lowercase();
    if exe_lower.ends_with("powershell.exe") || exe_lower.ends_with("pwsh.exe") {
        Some((exe, trimmed[end..].trim_start()))
    } else {
        None
    }
}

/// Whether `command`'s first token explicitly names `cmd`/`cmd.exe` as the
/// interpreter -- lets `run_local_ai_command_output` know whether to fall
/// back to its PowerShell default or respect the command's own choice.
#[cfg(target_os = "windows")]
fn is_explicit_cmd_invocation(command: &str) -> bool {
    let trimmed = command.trim_start();
    let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let exe = trimmed[..end].trim_matches('"').to_ascii_lowercase();
    exe == "cmd" || exe.ends_with("cmd.exe")
}

#[cfg(target_os = "windows")]
fn windows_command_for(command: &str, default_to_powershell: bool) -> tokio::process::Command {
    // `raw_arg` (not `arg`): if `command` is already a complete command line
    // the caller hand-assembled/escaped for the target shell, `.arg()` would
    // treat it as one opaque value and escape it *again* -- the quotes it
    // already added get treated as characters needing protection, doubling
    // up. `raw_arg` appends the string to the command line verbatim, which
    // is exactly what an already-escaped command line needs.
    //
    // cmd.exe /C can't reliably handle embedded newlines; PowerShell is
    // spawned directly instead when the command already names it.
    if let Some((exe, rest)) = split_direct_shell_invocation(command) {
        let mut c = tokio::process::Command::new(exe);
        if !rest.is_empty() {
            c.raw_arg(rest);
        }
        c.creation_flags(CREATE_NO_WINDOW);
        c
    } else if default_to_powershell && !is_explicit_cmd_invocation(command) {
        // The `run_command` tool's own entry point (see
        // `run_local_ai_command_output`'s doc): when the command doesn't
        // name an interpreter, default to PowerShell per the system
        // prompt's promise instead of cmd.exe. Plain `.arg()` here (not
        // `raw_arg`) -- `command` is a raw PowerShell script the model
        // wrote, not an already-shell-escaped fragment, so it needs the
        // standard library's own escaping to become one argument value to
        // `-Command`.
        let mut c = tokio::process::Command::new("powershell.exe");
        c.arg("-NoProfile").arg("-Command").arg(command);
        c.creation_flags(CREATE_NO_WINDOW);
        c
    } else {
        // Any shell command string (`git status`, explicit `cmd.exe /c
        // dir`), or callers with `default_to_powershell = false`
        // (`git_ops.rs`, which hand-escapes its command per POSIX rules --
        // see `run_local_command_output_with_env`'s doc), keep using
        // cmd.exe as the interpreter.
        let mut c = tokio::process::Command::new("cmd");
        c.raw_arg("/C").raw_arg(command);
        c.creation_flags(CREATE_NO_WINDOW);
        c
    }
}

#[cfg(not(target_os = "windows"))]
fn unix_command_for(command: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("sh");
    c.arg("-c").arg(command);
    c
}

/// Shared timeout for local command execution (`run_command`/`git_ops`'s
/// local branch both go through this). A command that hangs without this
/// would otherwise wedge the whole tool-call turn until the user manually
/// hits "stop". Long-running processes (dev servers, watch mode) should use
/// `run_command_background` instead of this path.
const LOCAL_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

async fn run_prepared_command(
    mut cmd: tokio::process::Command,
    cwd: &str,
    env: &HashMap<String, String>,
) -> Result<std::process::Output, AppError> {
    cmd.current_dir(cwd);
    cmd.envs(env);
    // `Command::output()` only pipes stdout/stderr; stdin is inherited from
    // the parent by default. roc_desk is a console-less GUI process, so an
    // inherited stdin handle is either invalid or a "read and block
    // forever" source -- if anything in the child process/shell parsing
    // chain ever tries to read stdin (even a command that normally needs no
    // input, if the command line gets parsed into an interactive-prompt
    // form), it hangs forever on that read. Pinning stdin to null makes any
    // such read return EOF immediately instead.
    cmd.stdin(std::process::Stdio::null());
    // `kill_on_drop(true)` makes tokio send a termination signal when this
    // future is cancelled by the timeout below and its `Child` handle gets
    // dropped, without needing to hold onto a separate `Child` handle to
    // kill it manually.
    cmd.kill_on_drop(true);
    match tokio::time::timeout(LOCAL_COMMAND_TIMEOUT, cmd.output()).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(AppError::Internal(format!(
            "命令执行超过 {} 秒未结束，已强制终止。如果这是一个需要长期挂起的进程（比如开发服务器、\
             watch 模式），改用 run_command_background 工具而不是 run_command。",
            LOCAL_COMMAND_TIMEOUT.as_secs()
        ))),
    }
}

/// `git_ops.rs`'s path -- the command is hand-escaped per POSIX rules
/// (`shell_quote` above), default interpreter stays cmd.exe (not switched to
/// PowerShell like `run_local_ai_command_output`, which would break that
/// escaping assumption).
pub async fn run_local_command_output_with_env(
    command: &str,
    cwd: &str,
    env: &HashMap<String, String>,
) -> Result<std::process::Output, AppError> {
    #[cfg(target_os = "windows")]
    let cmd = windows_command_for(command, false);
    #[cfg(not(target_os = "windows"))]
    let cmd = unix_command_for(command);
    run_prepared_command(cmd, cwd, env).await
}

/// The `run_command` tool's own entry point -- the only difference from
/// `run_local_command_output_with_env` is that when the command doesn't
/// name an interpreter explicitly, it defaults to PowerShell instead of
/// cmd.exe (matches what the system prompt tells the model the local
/// execution environment is).
pub async fn run_local_ai_command_output(
    command: &str,
    cwd: &str,
    env: &HashMap<String, String>,
) -> Result<std::process::Output, AppError> {
    #[cfg(target_os = "windows")]
    let cmd = windows_command_for(command, true);
    #[cfg(not(target_os = "windows"))]
    let cmd = unix_command_for(command);
    run_prepared_command(cmd, cwd, env).await
}
