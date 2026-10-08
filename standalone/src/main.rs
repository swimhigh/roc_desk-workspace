#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use roc_desk_workspace::roc_desk_editor::symbols::SymbolIndexState;
use roc_desk_workspace::WorkspaceAppState;
use tauri::Manager;

fn main() {
    // Portable, exe-relative `.rock_desk` dir (see
    // `roc_desk_core::paths::portable_data_dir` docs) instead of an
    // OS-AppData path keyed by this tool's own name — keeps this standalone
    // tool's data in the same place/layout the full `roc_desk.exe` host
    // uses, so copying several standalone tool exes into one directory
    // makes them share it automatically.
    let data_dir = roc_desk_core::paths::portable_data_dir().expect("failed to resolve app data dir");
    let db_path = data_dir.join("workspace.db");
    let cache_root = data_dir.join("workspace-cache");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(SymbolIndexState::default())
        .setup(move |app| {
            // Standalone now owns its own SSH/Agent connection pools (a
            // fresh `ssh.db`, independent of `workspace.db`) and feeds them
            // into `WorkspaceAppState::with_ssh` -- this is what makes both
            // `workspace_open_remote` (and, incidentally, local workspaces
            // too: `CodingSession` always needs *some* pools passed to it,
            // see `WorkspaceAppState::ssh`'s doc comment) actually work,
            // instead of permanently returning "功能未启用" like before.
            let ssh_db_path = data_dir.join("ssh.db");
            let ssh_state = roc_desk_ssh::RocDeskSshAppState::new(&ssh_db_path, app.handle().clone())
                .expect("failed to init ssh state");
            let workspace_state = WorkspaceAppState::new(&db_path, cache_root.clone())
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
            // SSH/Agent connection management + remote directory browsing --
            // only the subset "连接远程主机并选择目录" actually needs (no
            // terminal/RDP/SFTP dual-pane browser/transfer log; the coding
            // workspace screen doesn't use those even in the full host app).
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
