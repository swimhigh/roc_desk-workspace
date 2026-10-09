#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use roc_desk_workspace::roc_desk_editor::symbols::SymbolIndexState;
use roc_desk_workspace::WorkspaceAppState;
use tauri::Manager;

/// See the identical helper in `roc_desk-ssh/standalone/src/main.rs` for the
/// full rationale -- if `table_name` already exists (this db file was
/// created by the full `roc_desk.exe` host's own migration set under a
/// different name), record `migration_name` as already-applied so this
/// crate's own migration doesn't try to re-run its `CREATE TABLE` against a
/// table that's already there.
fn bridge_migration_if_table_exists(
    db_path: &std::path::Path,
    migration_name: &str,
    table_name: &str,
) -> Result<(), roc_desk_core::error::AppError> {
    let pool = roc_desk_core::db::pool::create_pool(db_path)?;
    let conn = pool.get()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            name TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    let table_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table_name],
        |r| r.get(0),
    )?;
    if table_exists {
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations (name) VALUES (?1)",
            [migration_name],
        )?;
    }
    Ok(())
}

fn main() {
    // Portable, exe-relative `.rock_desk` dir (see
    // `roc_desk_core::paths::portable_data_dir` docs) instead of an
    // OS-AppData path keyed by this tool's own name — keeps this standalone
    // tool's data in the same place/layout the full `roc_desk.exe` host
    // uses, so copying several standalone tool exes into one directory
    // makes them share it automatically.
    let data_dir = roc_desk_core::paths::portable_data_dir().expect("failed to resolve app data dir");
    // This crate's own AI-coding-agent tables (coding history/MCP/
    // permission rules/audit/evidence) -- same filename the host points its
    // own `roc_desk_workspace::WorkspaceAppState` at (`workspace_tool.db`,
    // see host `src-tauri/src/lib.rs`). Kept separate from AI providers and
    // the workspace list below: those two have schemas verified identical
    // to the host's own (independently-implemented) equivalents, these
    // don't, so sharing `roc_desk.db` for them would risk silent drift
    // between two copies of a migration.
    let db_path = data_dir.join("workspace_tool.db");
    // AI provider configs -- same file (`roc_desk.db`, the host's main db)
    // the host's own AI panels (coding agent/SQL assist) read/write, so a
    // provider configured in either place shows up in both. See
    // `WorkspaceAppState::new`'s doc comment for the schema-compatibility
    // check behind this.
    let ai_providers_db_path = data_dir.join("roc_desk.db");
    // The actual "recent workspaces" list -- same file the host's own
    // `WorkspaceManager` reads/writes (`workspaces/workspaces.db`), not a
    // separate `workspace.db` of this exe's own, so folders opened in
    // either place show up in both.
    let workspaces_dir = data_dir.join("workspaces");
    std::fs::create_dir_all(&workspaces_dir).expect("create workspaces dir");
    let workspace_db_path = workspaces_dir.join("workspaces.db");
    let cache_root = data_dir.join("workspace-cache");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(SymbolIndexState::default())
        .setup(move |app| {
            // Same file the host's own SSH/RDP/Agent panel reads/writes
            // (`sessions/sessions.db`), not a separate `ssh.db` of this
            // exe's own, so connections saved in either place show up in
            // both. Feeding these pools into `WorkspaceAppState::with_ssh`
            // is what makes both `workspace_open_remote` (and, incidentally,
            // local workspaces too: `CodingSession` always needs *some*
            // pools passed to it, see `WorkspaceAppState::ssh`'s doc
            // comment) actually work, instead of permanently returning
            // "功能未启用" like before.
            let sessions_dir = data_dir.join("sessions");
            std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
            let ssh_db_path = sessions_dir.join("sessions.db");
            bridge_migration_if_table_exists(&ssh_db_path, "0001_ssh_init", "connections")
                .expect("bridge legacy sessions.db migration state");
            let ssh_state = roc_desk_ssh::RocDeskSshAppState::new(&ssh_db_path, app.handle().clone())
                .expect("failed to init ssh state");
            let workspace_state = WorkspaceAppState::new(&db_path, &ai_providers_db_path, &workspace_db_path, cache_root.clone())
                .expect("failed to init workspace state")
                .with_ssh(
                    ssh_state.connection_manager.clone(),
                    ssh_state.ssh_pool.clone(),
                    ssh_state.agent_pool.clone(),
                );
            app.manage(workspace_state);
            app.manage(ssh_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // This tool's own commands.
            roc_desk_workspace::cmd::workspace_list_recent,
            roc_desk_workspace::cmd::workspace_open_local,
            roc_desk_workspace::cmd::workspace_open_remote,
            roc_desk_workspace::cmd::workspace_close,
            roc_desk_workspace::cmd::workspace_remove_recent,
            roc_desk_workspace::cmd::workspace_update_path,
            roc_desk_workspace::cmd::workspace_update_last_sftp_paths,
            roc_desk_workspace::cmd::pty_open,
            roc_desk_workspace::cmd::pty_write,
            roc_desk_workspace::cmd::pty_resize,
            roc_desk_workspace::cmd::pty_close,
            roc_desk_workspace::cmd::git_is_repo,
            roc_desk_workspace::cmd::git_status,
            roc_desk_workspace::cmd::git_diff,
            roc_desk_workspace::cmd::git_log,
            roc_desk_workspace::cmd::git_current_branch,
            roc_desk_workspace::cmd::git_commit_file,
            roc_desk_workspace::cmd::git_commit_paths,
            // AI provider management (shared backing for the coding agent).
            roc_desk_workspace::cmd::ai_provider_list,
            roc_desk_workspace::cmd::ai_provider_create,
            roc_desk_workspace::cmd::ai_provider_update,
            roc_desk_workspace::cmd::ai_provider_delete,
            roc_desk_workspace::cmd::ai_provider_list_models,
            // AI coding agent.
            roc_desk_workspace::cmd::coding_set_provider,
            roc_desk_workspace::cmd::coding_start,
            roc_desk_workspace::cmd::coding_new_session,
            roc_desk_workspace::cmd::coding_close,
            roc_desk_workspace::cmd::coding_set_mode,
            roc_desk_workspace::cmd::coding_set_auto_allow_readonly,
            roc_desk_workspace::cmd::coding_set_auto_git_commit,
            roc_desk_workspace::cmd::coding_set_full_auto,
            roc_desk_workspace::cmd::coding_set_auto_apply_changes,
            roc_desk_workspace::cmd::coding_send_message,
            roc_desk_workspace::cmd::coding_cancel_turn,
            roc_desk_workspace::cmd::coding_inject_message,
            roc_desk_workspace::cmd::coding_optimize_prompt,
            roc_desk_workspace::cmd::coding_answer_question,
            roc_desk_workspace::cmd::permission_rule_list,
            roc_desk_workspace::cmd::permission_rule_create,
            roc_desk_workspace::cmd::permission_rule_delete,
            roc_desk_workspace::cmd::mcp_server_list,
            roc_desk_workspace::cmd::mcp_server_create,
            roc_desk_workspace::cmd::mcp_server_update,
            roc_desk_workspace::cmd::mcp_server_delete,
            roc_desk_workspace::cmd::skill_list,
            roc_desk_workspace::cmd::skill_delete,
            roc_desk_workspace::cmd::skill_import,
            roc_desk_workspace::cmd::coding_accept_change,
            roc_desk_workspace::cmd::coding_reject_change,
            roc_desk_workspace::cmd::coding_undo_change,
            roc_desk_workspace::cmd::coding_redo_change,
            roc_desk_workspace::cmd::coding_revert_turn,
            roc_desk_workspace::cmd::coding_confirm_command,
            roc_desk_workspace::cmd::coding_history_save,
            roc_desk_workspace::cmd::coding_history_list,
            roc_desk_workspace::cmd::coding_history_get,
            roc_desk_workspace::cmd::coding_history_resume,
            roc_desk_workspace::cmd::coding_history_rename,
            roc_desk_workspace::cmd::coding_history_delete,
            // Workspace-scoped filesystem (local + remote) for `ExplorerTree`/
            // `EditorPane` -- the piece that was missing for genuine remote
            // workspace editing (AI-agent edits already went through
            // `ChangeStore`/`FileOps` and didn't need this).
            roc_desk_workspace::cmd::fs_list_dir,
            roc_desk_workspace::cmd::fs_read_file,
            roc_desk_workspace::cmd::fs_write_file,
            roc_desk_workspace::cmd::fs_read_file_with_encoding,
            roc_desk_workspace::cmd::fs_write_file_with_encoding,
            roc_desk_workspace::cmd::fs_supported_encodings,
            roc_desk_workspace::cmd::fs_read_binary_preview,
            roc_desk_workspace::cmd::fs_open_externally,
            roc_desk_workspace::cmd::fs_convert_legacy_office_to_pdf,
            roc_desk_workspace::cmd::fs_inspect_binary,
            roc_desk_workspace::cmd::fs_peek_is_binary,
            roc_desk_workspace::cmd::fs_inspect_jar,
            roc_desk_workspace::cmd::fs_delete,
            roc_desk_workspace::cmd::fs_rename,
            roc_desk_workspace::cmd::fs_copy,
            roc_desk_workspace::cmd::fs_create_dir,
            // SSH/Agent connection management + remote directory browsing +
            // interactive terminal (the subset "连接远程主机并选择目录" +
            // a remote workspace's own "终端" tab need -- still no RDP/SFTP
            // dual-pane browser/transfer log, those aren't part of the
            // coding workspace screen even in the full host app).
            roc_desk_ssh::cmd::connection_list,
            roc_desk_ssh::cmd::connection_create,
            roc_desk_ssh::cmd::connection_update,
            roc_desk_ssh::cmd::connection_delete,
            roc_desk_ssh::cmd::connection_group_list,
            roc_desk_ssh::cmd::connection_group_create,
            roc_desk_ssh::cmd::connection_group_update,
            roc_desk_ssh::cmd::connection_group_delete,
            roc_desk_ssh::cmd::sftp_list_dir,
            roc_desk_ssh::cmd::agent_test_connection,
            roc_desk_ssh::cmd::agent_list_dir,
            roc_desk_ssh::cmd::agent_list_roots,
            roc_desk_ssh::cmd::ssh_confirm_host_key,
            roc_desk_ssh::cmd::agent_confirm_cert,
            roc_desk_ssh::cmd::ssh_open_shell,
            roc_desk_ssh::cmd::ssh_write,
            roc_desk_ssh::cmd::ssh_resize,
            roc_desk_ssh::cmd::ssh_close_channel,
            roc_desk_ssh::cmd::agent_open_shell,
            roc_desk_ssh::cmd::agent_write,
            roc_desk_ssh::cmd::agent_resize,
            roc_desk_ssh::cmd::agent_close_channel,
            // Local filesystem surface, reused from roc_desk-explorer.
            roc_desk_workspace::roc_desk_explorer::cmd::local_list_dir,
            roc_desk_workspace::roc_desk_explorer::cmd::local_list_drives,
            roc_desk_workspace::roc_desk_explorer::cmd::local_home_dir,
            roc_desk_workspace::roc_desk_explorer::cmd::local_is_dir,
            roc_desk_workspace::roc_desk_explorer::cmd::local_delete,
            roc_desk_workspace::roc_desk_explorer::cmd::local_rename,
            roc_desk_workspace::roc_desk_explorer::cmd::local_copy,
            roc_desk_workspace::roc_desk_explorer::cmd::local_create_dir,
            roc_desk_workspace::roc_desk_explorer::cmd::local_move,
            roc_desk_workspace::roc_desk_explorer::cmd::local_read_file,
            roc_desk_workspace::roc_desk_explorer::cmd::local_write_file,
            roc_desk_workspace::roc_desk_explorer::cmd::local_read_file_with_encoding,
            roc_desk_workspace::roc_desk_explorer::cmd::local_write_file_with_encoding,
            roc_desk_workspace::roc_desk_explorer::cmd::local_read_binary_preview,
            roc_desk_workspace::roc_desk_explorer::cmd::local_open_externally,
            roc_desk_workspace::roc_desk_explorer::cmd::local_convert_legacy_office_to_pdf,
            roc_desk_workspace::roc_desk_explorer::cmd::local_inspect_binary,
            roc_desk_workspace::roc_desk_explorer::cmd::local_peek_is_binary,
            roc_desk_workspace::roc_desk_explorer::cmd::local_inspect_jar,
            // Editor-owned commands (symbol index, OCR).
            roc_desk_workspace::roc_desk_editor::ocr::editor_ocr_image,
            roc_desk_workspace::roc_desk_editor::symbols::editor_symbols_build_index,
            roc_desk_workspace::roc_desk_editor::symbols::editor_symbols_go_to_definition,
            roc_desk_workspace::roc_desk_editor::symbols::editor_symbols_reindex_file,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run standalone tool");
}
