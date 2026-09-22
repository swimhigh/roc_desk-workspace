import { invoke } from "@tauri-apps/api/core";

// -----------------------------------------------------------------------
// Types (mirror the Rust structs in roc_desk_core::workspace / the local
// filesystem commands re-exported from roc_desk-explorer).
// -----------------------------------------------------------------------

export interface WorkspaceProfile {
  id: string;
  kind: "local";
  root_path: string;
  display_name: string;
  last_opened_at: string | null;
}

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number | null;
  modified: number | null;
}

export interface FileContent {
  text: string;
  encoding: string;
  mtime: number;
  total_size: number;
  truncated: boolean;
}

export type WriteOutcome =
  | { type: "Written"; mtime: number }
  | { type: "Conflict"; current_mtime: number; current_preview: string };

// -----------------------------------------------------------------------
// Workspace (recent-folder tracking) -- roc_desk_workspace::cmd::workspace_*
// -----------------------------------------------------------------------

export const workspaceService = {
  listRecent(limit = 20): Promise<WorkspaceProfile[]> {
    return invoke("workspace_list_recent", { limit });
  },
  openLocal(path: string): Promise<WorkspaceProfile> {
    return invoke("workspace_open_local", { path });
  },
  removeRecent(id: string): Promise<void> {
    return invoke("workspace_remove_recent", { id });
  },
  updatePath(id: string, newPath: string): Promise<WorkspaceProfile> {
    return invoke("workspace_update_path", { id, newPath });
  },
};

// -----------------------------------------------------------------------
// Local filesystem -- roc_desk_explorer::cmd::local_*, re-exported by
// roc_desk-editor and this tool's standalone shell.
// -----------------------------------------------------------------------

export const localFsService = {
  listDir(path: string): Promise<FileEntry[]> {
    return invoke("local_list_dir", { path });
  },
  isDir(path: string): Promise<boolean> {
    return invoke("local_is_dir", { path });
  },
  readFile(path: string): Promise<FileContent> {
    return invoke("local_read_file", { path });
  },
  writeFile(path: string, content: string, expectedMtime: number | null): Promise<WriteOutcome> {
    return invoke("local_write_file", { path, content, expectedMtime });
  },
  createDir(path: string): Promise<void> {
    return invoke("local_create_dir", { path });
  },
  rename(from: string, to: string): Promise<void> {
    return invoke("local_rename", { from, to });
  },
  deletePath(path: string, isDir: boolean): Promise<void> {
    return invoke("local_delete", { path, isDir });
  },
};

// -----------------------------------------------------------------------
// Local terminal -- roc_desk_workspace::cmd::pty_*
// -----------------------------------------------------------------------

export const ptyService = {
  open(cwd: string, rows: number, cols: number): Promise<string> {
    return invoke("pty_open", { cwd, rows, cols });
  },
  write(channelId: string, data: Uint8Array): Promise<void> {
    return invoke("pty_write", { channelId, data: Array.from(data) });
  },
  resize(channelId: string, rows: number, cols: number): Promise<void> {
    return invoke("pty_resize", { channelId, rows, cols });
  },
  close(channelId: string): Promise<void> {
    return invoke("pty_close", { channelId });
  },
};

// -----------------------------------------------------------------------
// Git panel -- roc_desk_workspace::cmd::git_*
// -----------------------------------------------------------------------

export const gitService = {
  isRepo(cwd: string): Promise<boolean> {
    return invoke("git_is_repo", { cwd });
  },
  status(cwd: string): Promise<string> {
    return invoke("git_status", { cwd, path: null });
  },
  diff(cwd: string): Promise<string> {
    return invoke("git_diff", { cwd, path: null });
  },
  log(cwd: string, limit = 50): Promise<string> {
    return invoke("git_log", { cwd, limit });
  },
  currentBranch(cwd: string): Promise<string> {
    return invoke("git_current_branch", { cwd });
  },
  commitPaths(cwd: string, paths: string[], message: string): Promise<string> {
    return invoke("git_commit_paths", { cwd, paths, message });
  },
};
