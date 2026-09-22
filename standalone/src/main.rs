#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use roc_desk_workspace::roc_desk_editor::symbols::SymbolIndexState;
use roc_desk_workspace::WorkspaceAppState;

fn app_data_dir() -> std::path::PathBuf {
    dirs_next_data_dir().join("roc_desk-workspace")
}

/// Minimal stand-in for `tauri::AppHandle::path().app_data_dir()` at the
/// point this state is constructed (before the app is fully built) --
/// mirrors the pattern used by the other standalone tool shells
/// (`roc_desk-sql`, `roc_desk-ssh`).
fn dirs_next_data_dir() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME")
            .map(|h| std::path::PathBuf::from(h).join(".local/share"))
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
    }
}

fn main() {
    let data_dir = app_data_dir();
    std::fs::create_dir_all(&data_dir).expect("failed to create app data dir");
    let db_path = data_dir.join("workspace.db");
    let cache_root = data_dir.join("workspace-cache");
    let state = WorkspaceAppState::new(&db_path, cache_root).expect("failed to init workspace state");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .manage(SymbolIndexState::default())
        .invoke_handler(tauri::generate_handler![
            // This tool's own commands.
            roc_desk_workspace::cmd::workspace_list_recent,
            roc_desk_workspace::cmd::workspace_open_local,
            roc_desk_workspace::cmd::workspace_remove_recent,
            roc_desk_workspace::cmd::workspace_update_path,
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
