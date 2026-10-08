import { invoke } from "@tauri-apps/api/core";

// -----------------------------------------------------------------------
// Types (mirror the Rust structs in roc_desk_core::workspace / the local
// filesystem commands re-exported from roc_desk-explorer).
// -----------------------------------------------------------------------

export interface WorkspaceProfile {
  id: string;
  kind: "local" | "remote";
  root_path: string;
  connection_id: string | null;
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
  openRemote(connectionId: string, remotePath: string): Promise<WorkspaceProfile> {
    return invoke("workspace_open_remote", { connectionId, remotePath });
  },
  close(id: string): Promise<void> {
    return invoke("workspace_close", { id });
  },
  removeRecent(id: string): Promise<void> {
    return invoke("workspace_remove_recent", { id });
  },
  updatePath(id: string, newPath: string): Promise<WorkspaceProfile> {
    return invoke("workspace_update_path", { id, newPath });
  },
};

// Workspace-scoped filesystem (local + remote, boundary-checked, backing
// `fs_*`) is `@roc_desk/tool-editor`'s own `fsService` -- that package's
// `editorStore.openPreview(workspaceId, path)` already dispatches through
// it whenever `workspaceId` is non-null, so this tool doesn't need a
// second copy; `ExplorerTree`/`explorerStore` below import it from there
// too, instead of duplicating it here.

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
  /** Image/PDF preview: returns base64 (no `data:` prefix) -- used by the
   * coding agent composer when an attachment arrives via native drag-and-drop
   * (a disk path, not a browser `File`). */
  readBinaryPreview(path: string): Promise<string> {
    return invoke("local_read_binary_preview", { path });
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
// AI provider management -- roc_desk_workspace::cmd::ai_provider_*
// -----------------------------------------------------------------------

export interface AiProvider {
  id: string;
  name: string;
  api_base: string;
  api_key_ref: string | null;
  model: string;
  is_local: boolean;
  wire_api: string;
  reasoning_effort: string | null;
  context_window_tokens: number | null;
  created_at: string;
}

export interface AiProviderInput {
  name: string;
  api_base: string;
  api_key: string | null;
  model: string;
  is_local: boolean;
  wire_api: string;
  reasoning_effort: string | null;
  context_window_tokens: number | null;
}

export const aiProviderService = {
  list(): Promise<AiProvider[]> {
    return invoke("ai_provider_list");
  },
  create(input: AiProviderInput): Promise<AiProvider> {
    return invoke("ai_provider_create", { input });
  },
  update(id: string, input: AiProviderInput): Promise<AiProvider> {
    return invoke("ai_provider_update", { id, input });
  },
  delete(id: string): Promise<void> {
    return invoke("ai_provider_delete", { id });
  },
  /** Fetches the models a provider actually supports (OpenAI-compatible
   * `GET /models`). Not cached -- every call is a real request. */
  listModels(id: string): Promise<string[]> {
    return invoke("ai_provider_list_models", { id });
  },
};

// -----------------------------------------------------------------------
// AI coding agent -- roc_desk_workspace::cmd::coding_*/permission_rule_*/
// mcp_server_*/skill_*
// -----------------------------------------------------------------------

export type CodingMode = "plan" | "build";

export type CodingTarget =
  | { kind: "Local" }
  | { kind: "Remote"; connection_id: string; host_label: string }
  | { kind: "Agent"; connection_id: string; host_label: string };

export type ChangeStatus = "pending" | "applied" | "rejected" | "undone";

export type ChatAttachment =
  | { kind: "image"; name: string; mime: string; data_base64: string }
  | { kind: "file"; name: string; content: string }
  | { kind: "pdf"; name: string; data_base64: string };

export interface DiffLine {
  sign: "+" | "-" | " ";
  content: string;
}

export interface FileChange {
  id: string;
  path: string;
  old_content: string;
  new_content: string;
  diff: DiffLine[];
  status: ChangeStatus;
  turn_id: string;
}

export interface FileSyncInfo {
  change_id: string;
  path: string;
  content: string;
  mtime: number;
}

export type TodoStatus = "pending" | "in_progress" | "completed";

export interface TodoItem {
  id: string;
  content: string;
  status: TodoStatus;
}

export interface CodingSessionInfo {
  id: string;
  provider_id: string;
  mode: CodingMode;
  target: CodingTarget;
  auto_allow_readonly: boolean;
  git_repo: boolean;
  auto_git_commit: boolean;
  full_auto: boolean;
  auto_apply_changes: boolean;
  changes: FileChange[];
  todos: TodoItem[];
  project_memory_loaded: string[];
}

export type PermissionDecision = "allow" | "ask" | "deny";

export interface PermissionRule {
  id: string;
  tool: string;
  pattern: string;
  decision: PermissionDecision;
  enabled: boolean;
  created_at: string;
}

export interface PermissionRuleInput {
  tool: string;
  pattern: string;
  decision: PermissionDecision;
}

export type McpTransportKind = "stdio" | "http";

export interface McpServer {
  id: string;
  name: string;
  transport: McpTransportKind;
  command: string | null;
  args: string[];
  env: Record<string, string>;
  url: string | null;
  headers: Record<string, string>;
  auth_token_ref: string | null;
  enabled: boolean;
  created_at: string;
}

export interface McpServerInput {
  name: string;
  transport: McpTransportKind;
  command: string | null;
  args: string[];
  env: Record<string, string>;
  url: string | null;
  headers: Record<string, string>;
  auth_token: string | null;
  enabled: boolean;
}

export interface SkillMeta {
  name: string;
  description: string;
  dir: string;
}

export interface CodingHistorySummary {
  id: string;
  title: string;
  provider_id: string;
  provider_label: string;
  model: string;
  mode: string;
  created_at: string;
  updated_at: string;
}

export interface CodingHistoryDetail extends CodingHistorySummary {
  workspace_id: string;
  timeline: unknown;
  changes: unknown;
}

export interface CodingTodoUpdateEvent {
  sessionId: string;
  todos: TodoItem[];
}

export interface CodingQuestionRequestEvent {
  sessionId: string;
  requestId: string;
  question: string;
  options: string[];
}

export interface CodingToolCallEvent {
  sessionId: string;
  tool: string;
  detail?: string | null;
  output?: string | null;
}

export interface CodingAssistantNoteEvent {
  sessionId: string;
  text: string;
  kind?: "model" | "status";
}

export interface CodingTokenUsageEvent {
  sessionId: string;
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
}

export interface CodingFileChangeEvent {
  sessionId: string;
  change: FileChange;
  sync?: FileSyncInfo | null;
}

export interface CodingCommandBlockedEvent {
  sessionId: string;
  command: string;
}

export interface CodingCommandConfirmRequestEvent {
  sessionId: string;
  requestId: string;
  command: string;
  host: string | null;
  kind?: "command" | "mcp";
  matchKey?: string;
}

export interface CodingGitCommitResultEvent {
  sessionId: string;
  path: string;
  output: string;
}

export interface CodingAutoContinueStartEvent {
  sessionId: string;
  note: string;
}

export interface CodingAutoContinueDoneEvent {
  sessionId: string;
  reply: string | null;
  error: string | null;
}

export const codingService = {
  start(workspaceId: string, providerId: string): Promise<CodingSessionInfo> {
    return invoke("coding_start", { workspaceId, providerId });
  },
  newSession(workspaceId: string, providerId: string): Promise<CodingSessionInfo> {
    return invoke("coding_new_session", { workspaceId, providerId });
  },
  closeSession(workspaceId: string): Promise<void> {
    return invoke("coding_close", { workspaceId });
  },
  setMode(workspaceId: string, mode: CodingMode): Promise<void> {
    return invoke("coding_set_mode", { workspaceId, mode });
  },
  setProvider(workspaceId: string, providerId: string): Promise<void> {
    return invoke("coding_set_provider", { workspaceId, providerId });
  },
  setAutoAllowReadonly(workspaceId: string, enabled: boolean): Promise<void> {
    return invoke("coding_set_auto_allow_readonly", { workspaceId, enabled });
  },
  setAutoGitCommit(workspaceId: string, enabled: boolean): Promise<void> {
    return invoke("coding_set_auto_git_commit", { workspaceId, enabled });
  },
  setFullAuto(workspaceId: string, enabled: boolean): Promise<void> {
    return invoke("coding_set_full_auto", { workspaceId, enabled });
  },
  setAutoApplyChanges(workspaceId: string, enabled: boolean): Promise<void> {
    return invoke("coding_set_auto_apply_changes", { workspaceId, enabled });
  },
  sendMessage(workspaceId: string, text: string, attachments?: ChatAttachment[]): Promise<string> {
    return invoke("coding_send_message", { workspaceId, text, attachments: attachments?.length ? attachments : null });
  },
  injectMessage(workspaceId: string, text: string, attachments?: ChatAttachment[]): Promise<void> {
    return invoke("coding_inject_message", { workspaceId, text, attachments: attachments?.length ? attachments : null });
  },
  cancelTurn(workspaceId: string): Promise<void> {
    return invoke("coding_cancel_turn", { workspaceId });
  },
  optimizePrompt(workspaceId: string, text: string): Promise<string> {
    return invoke("coding_optimize_prompt", { workspaceId, text });
  },
  acceptChange(workspaceId: string, changeId: string): Promise<FileSyncInfo> {
    return invoke("coding_accept_change", { workspaceId, changeId });
  },
  rejectChange(workspaceId: string, changeId: string): Promise<void> {
    return invoke("coding_reject_change", { workspaceId, changeId });
  },
  undoChange(workspaceId: string, changeId: string): Promise<FileSyncInfo> {
    return invoke("coding_undo_change", { workspaceId, changeId });
  },
  redoChange(workspaceId: string): Promise<FileSyncInfo | null> {
    return invoke("coding_redo_change", { workspaceId });
  },
  revertTurn(workspaceId: string, turnId: string): Promise<FileSyncInfo[]> {
    return invoke("coding_revert_turn", { workspaceId, turnId });
  },
  confirmCommand(requestId: string, allow: boolean): Promise<void> {
    return invoke("coding_confirm_command", { requestId, allow });
  },
  answerQuestion(requestId: string, answer: string): Promise<void> {
    return invoke("coding_answer_question", { requestId, answer });
  },
  historyList(workspaceId: string): Promise<CodingHistorySummary[]> {
    return invoke("coding_history_list", { workspaceId });
  },
  historyGet(id: string): Promise<CodingHistoryDetail | null> {
    return invoke("coding_history_get", { id });
  },
  historyResume(workspaceId: string, historyId: string): Promise<CodingSessionInfo> {
    return invoke("coding_history_resume", { workspaceId, historyId });
  },
  historySave(input: Record<string, unknown>): Promise<void> {
    return invoke("coding_history_save", { input });
  },
  historyRename(id: string, title: string): Promise<void> {
    return invoke("coding_history_rename", { id, title });
  },
  historyDelete(id: string): Promise<void> {
    return invoke("coding_history_delete", { id });
  },
};

export const permissionRuleService = {
  list(): Promise<PermissionRule[]> {
    return invoke("permission_rule_list");
  },
  create(input: PermissionRuleInput): Promise<PermissionRule> {
    return invoke("permission_rule_create", { input });
  },
  delete(id: string): Promise<void> {
    return invoke("permission_rule_delete", { id });
  },
};

export const mcpServerService = {
  list(): Promise<McpServer[]> {
    return invoke("mcp_server_list");
  },
  create(input: McpServerInput): Promise<McpServer> {
    return invoke("mcp_server_create", { input });
  },
  update(id: string, input: McpServerInput): Promise<McpServer> {
    return invoke("mcp_server_update", { id, input });
  },
  delete(id: string): Promise<void> {
    return invoke("mcp_server_delete", { id });
  },
};

export const skillService = {
  list(workspaceId: string): Promise<SkillMeta[]> {
    return invoke("skill_list", { workspaceId });
  },
  import(workspaceId: string, localPath: string): Promise<SkillMeta> {
    return invoke("skill_import", { workspaceId, localPath });
  },
  delete(workspaceId: string, name: string): Promise<void> {
    return invoke("skill_delete", { workspaceId, name });
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
