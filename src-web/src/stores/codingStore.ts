import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { useEditorStore } from "@roc_desk/tool-editor";
import {
  codingService,
  permissionRuleService,
  mcpServerService,
  skillService,
  localFsService,
  type ChatAttachment,
  type CodingAssistantNoteEvent,
  type CodingAutoContinueDoneEvent,
  type CodingAutoContinueStartEvent,
  type CodingCommandBlockedEvent,
  type CodingCommandConfirmRequestEvent,
  type CodingFileChangeEvent,
  type CodingGitCommitResultEvent,
  type CodingMode,
  type CodingQuestionRequestEvent,
  type CodingSessionInfo,
  type CodingToolCallEvent,
  type CodingTodoUpdateEvent,
  type CodingTokenUsageEvent,
  type FileChange,
  type CodingHistorySummary,
  type McpServer,
  type McpServerInput,
  type PermissionRule,
  type PermissionRuleInput,
  type SkillMeta,
} from "../services";
import { formatError } from "../utils/error";
import { useAiProviderStore } from "./aiProviderStore";

/** Front-end-only representation of a "pending to send" attachment, shown
 * above the composer before it's actually sent -- images additionally
 * carry `previewUrl` (a full data URL, fed straight to `<img>`); on send,
 * it's stripped down to the prefix-less `data_base64` the backend expects
 * (see `toChatAttachment`). */
export interface PendingAttachment {
  id: string;
  kind: "image" | "file" | "pdf";
  name: string;
  size: number;
  mime?: string;
  previewUrl?: string;
  base64?: string;
  content?: string;
}

export interface CommandConfirmRequest {
  requestId: string;
  command: string;
  host: string | null;
  kind: "command" | "mcp";
  matchKey?: string;
}

/** Attachment summary used for the timeline's "user message" echo -- only
 * what rendering needs, not a full `ChatAttachment` (already handed to the
 * backend alongside the text at send time, no need to duplicate base64/full
 * file content in frontend state). */
export interface TimelineAttachment {
  kind: "image" | "file" | "pdf";
  name: string;
  previewUrl?: string;
}

export type TimelineEntry =
  | { kind: "user"; id: string; text: string; attachments?: TimelineAttachment[] }
  | { kind: "assistant"; id: string; text: string }
  | { kind: "note"; id: string; text: string }
  | { kind: "progress"; id: string; text: string }
  | { kind: "tool"; id: string; tool: string; running: boolean; detail?: string | null; startedAt?: number; output?: string | null; expanded?: boolean }
  | { kind: "change"; id: string; changeId: string }
  | { kind: "blocked"; id: string; command: string }
  | { kind: "git"; id: string; path: string; output: string }
  | { kind: "usage"; id: string; promptTokens: number; completionTokens: number; totalTokens: number; isTurnTotal: boolean };

/** Bounded-keepalive LRU cap -- how many workspace coding sessions stay
 * "alive" at once. */
const MAX_RESIDENT_WORKSPACES = 3;

/** A workspace's snapshot parked aside (but still kept alive) when the
 * user switches away from it -- not persisted, pure in-memory cache,
 * restored verbatim when switching back without re-running `newSession`/
 * history resume. */
interface WorkspaceSnapshot {
  sessionInfo: CodingSessionInfo | null;
  timeline: TimelineEntry[];
  changesById: Record<string, FileChange>;
  viewingHistoryId: string | null;
}

interface CodingState {
  workspaceId: string | null;
  sessionInfo: CodingSessionInfo | null;
  timeline: TimelineEntry[];
  changesById: Record<string, FileChange>;
  sending: boolean;
  /** Live (in-progress) token usage for the current round -- rendered as an
   * overwrite-in-place indicator next to "AI is working...", not appended
   * to the timeline (a turn can involve dozens of round trips; appending
   * one line per round trip would drown out the actually informative
   * progress notes). Only the whole-turn summary
   * (`coding:token-usage-summary`) gets a permanent timeline entry. */
  liveTokenUsage: { promptTokens: number; completionTokens: number; totalTokens: number } | null;
  error: string | null;
  attachments: PendingAttachment[];
  optimizing: boolean;
  confirmRequest: CommandConfirmRequest | null;
  /** Requests queued behind `confirmRequest` -- the agent can fire several
   * concurrent tool calls (e.g. a burst of `run_command` against a remote
   * target), each triggering its own `coding:command-confirm-request`.
   * Overwriting a single nullable field would silently drop whichever
   * request was showing, leaving its backend-side `CommandConfirmRegistry`
   * wait hung forever with no visible sign anything is still pending. New
   * requests queue here while one is already showing; once the showing one
   * is resolved, the next in line is promoted automatically. */
  confirmQueue: CommandConfirmRequest[];
  questionRequest: { requestId: string; question: string; options: string[] } | null;
  permissionRules: PermissionRule[];
  mcpServers: McpServer[];
  skills: SkillMeta[];
  histories: CodingHistorySummary[];
  viewingHistoryId: string | null;
  /** History entry currently being opened (`historyGet`+`historyResume`) --
   * both now read the real content straight from the workspace mirror file
   * every time (no more full local cache), so this can take a moment on a
   * remote workspace; the dialog shows "opening..." on that row instead of
   * looking stuck. */
  openingHistoryId: string | null;
  /** Most-recently-used workspace ids, newest first -- only used to decide
   * LRU eviction order. */
  residentOrder: string[];
  /** Snapshots for workspaces not currently displayed but still kept alive
   * (not yet evicted). */
  byWorkspace: Record<string, WorkspaceSnapshot>;
  loadHistories: (workspaceId?: string) => Promise<void>;
  saveCurrentHistory: () => Promise<void>;
  /** Returns whether it actually resumed -- callers use this to decide
   * whether to close the history dialog (stay open on failure so the user
   * sees `error` and can try another entry). */
  openHistory: (id: string) => Promise<boolean>;
  deleteHistory: (id: string) => Promise<void>;
  renameHistory: (id: string, title: string) => Promise<void>;
  newSession: (providerId: string) => Promise<void>;
  /** Switches to a workspace's coding session: already kept alive (in
   * `byWorkspace`) restores the snapshot in place with no backend call;
   * otherwise starts a backend session + restores a within-12h history as
   * before. The previously-displayed workspace is recorded into the
   * keepalive set; past `MAX_RESIDENT_WORKSPACES` the least-recently-used
   * one is evicted (frontend drops its snapshot + backend `coding_close`
   * actually releases the session). */
  restoreOrStart: (workspaceId: string, providerId: string) => Promise<void>;
  /** Evicts a workspace's kept-alive session: drops the frontend snapshot,
   * calls backend `coding_close` to actually release the `CodingSession`. */
  evictWorkspace: (workspaceId: string) => Promise<void>;

  start: (workspaceId: string, providerId: string) => Promise<void>;
  setMode: (mode: CodingMode) => Promise<void>;
  setProvider: (providerId: string) => Promise<void>;
  setAutoAllowReadonly: (enabled: boolean) => Promise<void>;
  setAutoGitCommit: (enabled: boolean) => Promise<void>;
  setFullAuto: (enabled: boolean) => Promise<void>;
  setAutoApplyChanges: (enabled: boolean) => Promise<void>;
  sendMessage: (text: string) => Promise<void>;
  cancelTurn: () => Promise<void>;
  addAttachments: (files: File[]) => Promise<void>;
  /** For native Tauri drag-and-drop (`useExternalFileDrop`) -- the payload
   * is a disk path, not a browser `File`, read differently, see
   * `readAttachmentFromPath`. */
  addAttachmentsFromPaths: (paths: string[]) => Promise<void>;
  removeAttachment: (id: string) => void;
  clearAttachments: () => void;
  optimizePrompt: (text: string) => Promise<string>;
  acceptChange: (changeId: string) => Promise<void>;
  rejectChange: (changeId: string) => Promise<void>;
  toggleToolOutput: (id: string) => void;
  undoChange: (changeId: string) => Promise<void>;
  redoChange: () => Promise<void>;
  revertTurn: (turnId: string) => Promise<void>;
  resolveConfirm: (allow: boolean) => Promise<void>;
  resolveConfirmAndRemember: (pattern: string) => Promise<void>;
  answerQuestion: (answer: string) => Promise<void>;
  loadPermissionRules: () => Promise<void>;
  createPermissionRule: (input: PermissionRuleInput) => Promise<void>;
  deletePermissionRule: (id: string) => Promise<void>;
  loadMcpServers: () => Promise<void>;
  createMcpServer: (input: McpServerInput) => Promise<void>;
  updateMcpServer: (id: string, input: McpServerInput) => Promise<void>;
  deleteMcpServer: (id: string) => Promise<void>;
  loadSkills: (workspaceId: string) => Promise<void>;
  importSkill: (workspaceId: string, localPath: string) => Promise<SkillMeta>;
  deleteSkill: (workspaceId: string, name: string) => Promise<void>;
  reset: () => void;
}

let seq = 0;
const nextId = () => `t-${++seq}`;

export const useCodingStore = create<CodingState>((set, get) => ({
  workspaceId: null,
  sessionInfo: null,
  timeline: [],
  changesById: {},
  sending: false,
  liveTokenUsage: null,
  error: null,
  attachments: [],
  optimizing: false,
  confirmRequest: null,
  confirmQueue: [],
  questionRequest: null,
  permissionRules: [],
  mcpServers: [],
  skills: [],
  histories: [],
  viewingHistoryId: null,
  openingHistoryId: null,
  residentOrder: [],
  byWorkspace: {},

  start: async (workspaceId, providerId) => {
    set({ error: null });
    try {
      const info = await codingService.start(workspaceId, providerId);
      const changesById: Record<string, FileChange> = {};
      const timeline: TimelineEntry[] = [];
      for (const change of info.changes) {
        changesById[change.id] = change;
        timeline.push({ kind: "change", id: nextId(), changeId: change.id });
      }
      set({ workspaceId, sessionInfo: info, changesById, timeline });
      await get().loadHistories(workspaceId);
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  setMode: async (mode) => {
    const { workspaceId, sessionInfo, viewingHistoryId } = get();
    if (viewingHistoryId) return;
    if (!workspaceId || !sessionInfo) return;
    await codingService.setMode(workspaceId, mode);
    set({ sessionInfo: { ...sessionInfo, mode } });
    await get().saveCurrentHistory();
  },

  setProvider: async (providerId) => {
    const { workspaceId, sessionInfo, viewingHistoryId } = get();
    if (viewingHistoryId) return;
    if (!workspaceId || !sessionInfo) return;
    await codingService.setProvider(workspaceId, providerId);
    set({ sessionInfo: { ...sessionInfo, provider_id: providerId } });
    await get().saveCurrentHistory();
  },

  setAutoAllowReadonly: async (enabled) => {
    const { workspaceId, sessionInfo, viewingHistoryId } = get();
    if (viewingHistoryId) return;
    if (!workspaceId || !sessionInfo) return;
    await codingService.setAutoAllowReadonly(workspaceId, enabled);
    set({ sessionInfo: { ...sessionInfo, auto_allow_readonly: enabled } });
  },

  setAutoGitCommit: async (enabled) => {
    const { workspaceId, sessionInfo, viewingHistoryId } = get();
    if (viewingHistoryId) return;
    if (!workspaceId || !sessionInfo) return;
    await codingService.setAutoGitCommit(workspaceId, enabled);
    set({ sessionInfo: { ...sessionInfo, auto_git_commit: enabled } });
  },

  setFullAuto: async (enabled) => {
    const { workspaceId, sessionInfo, viewingHistoryId } = get();
    if (viewingHistoryId) return;
    if (!workspaceId || !sessionInfo) return;
    await codingService.setFullAuto(workspaceId, enabled);
    set((s) => ({
      sessionInfo: { ...sessionInfo, full_auto: enabled },
      confirmRequest: enabled ? null : s.confirmRequest,
    }));
  },

  setAutoApplyChanges: async (enabled) => {
    const { workspaceId, sessionInfo, viewingHistoryId } = get();
    if (viewingHistoryId) return;
    if (!workspaceId || !sessionInfo) return;
    await codingService.setAutoApplyChanges(workspaceId, enabled);
    set({ sessionInfo: { ...sessionInfo, auto_apply_changes: enabled } });
  },

  sendMessage: async (text) => {
    const { workspaceId, sending, viewingHistoryId, attachments } = get();
    if (!workspaceId || (!text.trim() && attachments.length === 0) || viewingHistoryId) return;
    const outgoing = attachments.map(toChatAttachment);
    const timelineAttachments: TimelineAttachment[] | undefined = attachments.length
      ? attachments.map((a) => ({ kind: a.kind, name: a.name, previewUrl: a.previewUrl }))
      : undefined;

    if (sending) {
      set((s) => ({
        timeline: [...s.timeline, { kind: "user", id: nextId(), text, attachments: timelineAttachments }],
        attachments: [],
      }));
      try {
        await codingService.injectMessage(workspaceId, text, outgoing);
      } catch (e) {
        set({ error: formatError(e) });
      }
      return;
    }

    set((s) => ({
      timeline: [...s.timeline, { kind: "user", id: nextId(), text, attachments: timelineAttachments }],
      sending: true,
      error: null,
      attachments: [],
    }));
    try {
      const reply = await codingService.sendMessage(workspaceId, text, outgoing);
      set((s) => ({ timeline: [...s.timeline, { kind: "assistant", id: nextId(), text: reply }], sending: false }));
      await get().saveCurrentHistory();
    } catch (e) {
      const message = formatError(e);
      if (message.includes("已停止：用户取消了当前对话轮次")) {
        set((s) => ({
          sending: false,
          timeline: [
            ...s.timeline.map((t) => (t.kind === "tool" && t.running ? { ...t, running: false } : t)),
            { kind: "note", id: nextId(), text: "已停止" },
          ],
        }));
      } else {
        set({ sending: false, error: message });
      }
    }
  },

  cancelTurn: async () => {
    const { workspaceId, sending } = get();
    if (!workspaceId || !sending) return;
    await codingService.cancelTurn(workspaceId);
  },

  addAttachments: async (files) => {
    const read = await Promise.all(files.map(readAttachment));
    const valid = read.filter((a): a is PendingAttachment => a !== null);
    if (valid.length === 0) return;
    set((s) => ({ attachments: [...s.attachments, ...valid].slice(0, MAX_ATTACHMENTS) }));
  },

  addAttachmentsFromPaths: async (paths) => {
    const read = await Promise.all(paths.map(readAttachmentFromPath));
    const valid = read.filter((a): a is PendingAttachment => a !== null);
    if (valid.length === 0) return;
    set((s) => ({ attachments: [...s.attachments, ...valid].slice(0, MAX_ATTACHMENTS) }));
  },

  removeAttachment: (id) => {
    set((s) => ({ attachments: s.attachments.filter((a) => a.id !== id) }));
  },

  clearAttachments: () => set({ attachments: [] }),

  optimizePrompt: async (text) => {
    const { workspaceId, optimizing, viewingHistoryId } = get();
    if (!workspaceId || !text.trim() || optimizing || viewingHistoryId) return text;
    set({ optimizing: true, error: null });
    try {
      const optimized = await codingService.optimizePrompt(workspaceId, text);
      set({ optimizing: false });
      return optimized;
    } catch (e) {
      set({ optimizing: false, error: formatError(e) });
      return text;
    }
  },

  toggleToolOutput: (id) => {
    set((s) => ({
      timeline: s.timeline.map((entry) =>
        entry.kind === "tool" && entry.id === id ? { ...entry, expanded: !entry.expanded } : entry,
      ),
    }));
  },

  acceptChange: async (changeId) => {
    const { workspaceId, viewingHistoryId } = get();
    if (!workspaceId || viewingHistoryId) return;
    const sync = await codingService.acceptChange(workspaceId, changeId);
    set((s) => ({
      changesById: { ...s.changesById, [changeId]: { ...s.changesById[changeId], status: "applied" } },
    }));
    useEditorStore.getState().syncExternalWrite(sync.path, sync.content, sync.mtime);
    await get().saveCurrentHistory();
  },

  rejectChange: async (changeId) => {
    const { workspaceId, viewingHistoryId } = get();
    if (!workspaceId || viewingHistoryId) return;
    await codingService.rejectChange(workspaceId, changeId);
    set((s) => ({
      changesById: { ...s.changesById, [changeId]: { ...s.changesById[changeId], status: "rejected" } },
    }));
    await get().saveCurrentHistory();
  },

  undoChange: async (changeId) => {
    const { workspaceId, viewingHistoryId } = get();
    if (!workspaceId || viewingHistoryId) return;
    const sync = await codingService.undoChange(workspaceId, changeId);
    set((s) => ({
      changesById: { ...s.changesById, [changeId]: { ...s.changesById[changeId], status: "undone" } },
    }));
    useEditorStore.getState().syncExternalWrite(sync.path, sync.content, sync.mtime);
    await get().saveCurrentHistory();
  },

  redoChange: async () => {
    const { workspaceId, viewingHistoryId } = get();
    if (!workspaceId || viewingHistoryId) return;
    const sync = await codingService.redoChange(workspaceId);
    if (!sync) return;
    set((s) => ({
      changesById: { ...s.changesById, [sync.change_id]: { ...s.changesById[sync.change_id], status: "applied" } },
    }));
    useEditorStore.getState().syncExternalWrite(sync.path, sync.content, sync.mtime);
    await get().saveCurrentHistory();
  },

  revertTurn: async (turnId) => {
    const { workspaceId, viewingHistoryId } = get();
    if (!workspaceId || viewingHistoryId) return;
    try {
      const reverted = await codingService.revertTurn(workspaceId, turnId);
      if (reverted.length === 0) {
        set({ error: "当前轮次没有可撤销的已应用改动" });
        return;
      }
      set((s) => {
        const changesById = { ...s.changesById };
        for (const sync of reverted) {
          if (changesById[sync.change_id]) changesById[sync.change_id] = { ...changesById[sync.change_id], status: "undone" };
        }
        return { changesById, error: null };
      });
      for (const sync of reverted) {
        useEditorStore.getState().syncExternalWrite(sync.path, sync.content, sync.mtime);
      }
      await get().saveCurrentHistory();
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  resolveConfirm: async (allow) => {
    const { confirmRequest } = get();
    if (!confirmRequest) return;
    set((s) => {
      const [next, ...rest] = s.confirmQueue;
      return { confirmRequest: next ?? null, confirmQueue: rest };
    });
    await codingService.confirmCommand(confirmRequest.requestId, allow);
  },

  resolveConfirmAndRemember: async (pattern) => {
    const { confirmRequest } = get();
    if (!confirmRequest) return;
    set((s) => {
      const [next, ...rest] = s.confirmQueue;
      return { confirmRequest: next ?? null, confirmQueue: rest };
    });
    try {
      await get().createPermissionRule({
        tool: confirmRequest.kind === "mcp" ? "mcp" : "run_command",
        pattern,
        decision: "allow",
      });
    } finally {
      await codingService.confirmCommand(confirmRequest.requestId, true);
    }
  },

  answerQuestion: async (answer) => {
    const { questionRequest } = get();
    if (!questionRequest) return;
    set({ questionRequest: null });
    await codingService.answerQuestion(questionRequest.requestId, answer);
  },

  loadPermissionRules: async () => {
    try { set({ permissionRules: await permissionRuleService.list() }); } catch { /* best effort */ }
  },

  createPermissionRule: async (input) => {
    const rule = await permissionRuleService.create(input);
    set((s) => ({ permissionRules: [...s.permissionRules, rule] }));
  },

  deletePermissionRule: async (id) => {
    await permissionRuleService.delete(id);
    set((s) => ({ permissionRules: s.permissionRules.filter((r) => r.id !== id) }));
  },

  loadMcpServers: async () => {
    try { set({ mcpServers: await mcpServerService.list() }); } catch { /* best effort */ }
  },

  createMcpServer: async (input) => {
    const server = await mcpServerService.create(input);
    set((s) => ({ mcpServers: [...s.mcpServers, server] }));
  },

  updateMcpServer: async (id, input) => {
    const server = await mcpServerService.update(id, input);
    set((s) => ({ mcpServers: s.mcpServers.map((m) => (m.id === id ? server : m)) }));
  },

  deleteMcpServer: async (id) => {
    await mcpServerService.delete(id);
    set((s) => ({ mcpServers: s.mcpServers.filter((m) => m.id !== id) }));
  },

  loadSkills: async (workspaceId) => {
    try { set({ skills: await skillService.list(workspaceId) }); } catch { /* best effort */ }
  },

  importSkill: async (workspaceId, localPath) => {
    const skill = await skillService.import(workspaceId, localPath);
    set((s) => ({ skills: [...s.skills.filter((sk) => sk.name !== skill.name), skill] }));
    return skill;
  },

  deleteSkill: async (workspaceId, name) => {
    await skillService.delete(workspaceId, name);
    set((s) => ({ skills: s.skills.filter((sk) => sk.name !== name) }));
  },

  loadHistories: async (workspaceId) => {
    const id = workspaceId ?? get().workspaceId;
    if (!id) return;
    try { set({ histories: await codingService.historyList(id) }); } catch { /* history must not block chat */ }
  },

  saveCurrentHistory: async () => {
    const { workspaceId, sessionInfo, timeline, changesById, viewingHistoryId } = get();
    if (!workspaceId || !sessionInfo || timeline.length === 0 || viewingHistoryId) return;
    const titleEntry = timeline.find((entry) => entry.kind === "user");
    const title = titleEntry && titleEntry.kind === "user" ? titleEntry.text.slice(0, 80) : "编程会话";
    const provider = useAiProviderStore.getState().providers.find((item) => item.id === sessionInfo.provider_id);
    try {
      await codingService.historySave({ id: sessionInfo.id, workspace_id: workspaceId, title, provider_id: sessionInfo.provider_id, provider_label: provider?.name ?? "AI Provider", model: provider?.model ?? "", mode: sessionInfo.mode, timeline, changes: Object.values(changesById) });
      await get().loadHistories(workspaceId);
    } catch { /* history is best effort */ }
  },

  openHistory: async (id) => {
    await get().saveCurrentHistory();
    const workspaceId = get().workspaceId;
    if (!workspaceId) return false;
    set({ openingHistoryId: id });
    try {
      const detail = await codingService.historyGet(id);
      if (!detail) {
        set({ error: "这条历史记录已经不存在了" });
        return false;
      }
      const info = await codingService.historyResume(workspaceId, id);
      const changes = detail.changes as FileChange[];
      set({
        workspaceId,
        viewingHistoryId: null,
        sessionInfo: info,
        timeline: (detail.timeline as TimelineEntry[]) ?? [],
        changesById: Object.fromEntries((changes ?? []).map((c) => [c.id, c])),
        error: null,
      });
      return true;
    } catch (e) {
      set({ error: formatError(e) });
      return false;
    } finally {
      set({ openingHistoryId: null });
    }
  },

  deleteHistory: async (id) => {
    const { viewingHistoryId, sessionInfo } = get();
    await codingService.historyDelete(id);
    set((s) => ({ histories: s.histories.filter((item) => item.id !== id), viewingHistoryId: s.viewingHistoryId === id ? null : s.viewingHistoryId }));
    if (viewingHistoryId === id && sessionInfo) await get().newSession(sessionInfo.provider_id);
  },

  renameHistory: async (id, title) => {
    const trimmed = title.trim();
    if (!trimmed) return;
    await codingService.historyRename(id, trimmed);
    await get().loadHistories();
  },

  newSession: async (providerId) => {
    await get().saveCurrentHistory();
    const workspaceId = get().workspaceId;
    if (!workspaceId) return;
    try {
      const info = await codingService.newSession(workspaceId, providerId);
      set({ sessionInfo: info, timeline: [], changesById: {}, viewingHistoryId: null, error: null });
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  restoreOrStart: async (workspaceId, providerId) => {
    const prev = get();
    if (prev.workspaceId === workspaceId) return;

    const nextByWorkspace = prev.workspaceId
      ? {
          ...prev.byWorkspace,
          [prev.workspaceId]: {
            sessionInfo: prev.sessionInfo,
            timeline: prev.timeline,
            changesById: prev.changesById,
            viewingHistoryId: prev.viewingHistoryId,
          },
        }
      : prev.byWorkspace;

    const order = [workspaceId, ...prev.residentOrder.filter((id) => id !== workspaceId)];
    const toEvict: string[] = [];
    while (order.length > MAX_RESIDENT_WORKSPACES) {
      const victim = order.pop();
      if (victim) toEvict.push(victim);
    }
    set({ residentOrder: order, byWorkspace: nextByWorkspace });
    for (const id of toEvict) {
      await get().evictWorkspace(id);
    }

    const cached = get().byWorkspace[workspaceId];
    if (cached) {
      const { [workspaceId]: _discard, ...rest } = get().byWorkspace;
      set({
        workspaceId,
        sessionInfo: cached.sessionInfo,
        timeline: cached.timeline,
        changesById: cached.changesById,
        viewingHistoryId: cached.viewingHistoryId,
        byWorkspace: rest,
        error: null,
      });
      return;
    }

    set({ workspaceId, sessionInfo: null, timeline: [], changesById: {}, viewingHistoryId: null, error: null, histories: [] });
    try {
      const historiesPromise = codingService.historyList(workspaceId);
      const info = await codingService.newSession(workspaceId, providerId);
      if (get().workspaceId !== workspaceId) return;
      set({ workspaceId, sessionInfo: info, timeline: [], changesById: {}, viewingHistoryId: null, histories: [], error: null });

      void historiesPromise.then(async (histories) => {
        if (get().workspaceId !== workspaceId || get().sessionInfo?.id !== info.id) return;
        set({ histories });
        const latest = histories[0];
        const latestTime = latest ? Date.parse(latest.updated_at) : NaN;
        const recent = latest && Number.isFinite(latestTime) && (Date.now() - latestTime) <= 12 * 60 * 60 * 1000;
        if (!recent) return;
        if (get().timeline.length > 0) return;
        try {
          const resumedInfo = await codingService.historyResume(workspaceId, latest.id);
          if (get().workspaceId !== workspaceId || get().sessionInfo?.id !== info.id) return;
          const detail = await codingService.historyGet(latest.id);
          if (!detail || get().workspaceId !== workspaceId || get().sessionInfo?.id !== info.id) return;
          const changes = detail.changes as FileChange[];
          set({
            sessionInfo: resumedInfo,
            timeline: detail.timeline as TimelineEntry[],
            changesById: Object.fromEntries(changes.map((change) => [change.id, change])),
            error: null,
          });
        } catch {
          // Resume failure (e.g. this history's provider was deleted) doesn't
          // affect the already-created new session; the user can keep
          // working in it.
        }
      }).catch(() => undefined);
    } catch (e) {
      if (get().workspaceId === workspaceId) set({ error: formatError(e) });
    }
  },

  evictWorkspace: async (workspaceId) => {
    set((s) => {
      const { [workspaceId]: _discard, ...rest } = s.byWorkspace;
      return { byWorkspace: rest };
    });
    try {
      await codingService.closeSession(workspaceId);
    } catch {
      /* best effort -- failing to release shouldn't block an in-progress switch */
    }
  },

  reset: () =>
    set({
      workspaceId: null,
      sessionInfo: null,
      timeline: [],
      changesById: {},
      sending: false,
      error: null,
      attachments: [],
      optimizing: false,
      confirmRequest: null,
      confirmQueue: [],
      questionRequest: null,
      openingHistoryId: null,
      residentOrder: [],
      byWorkspace: {},
    }),
}));

const MAX_ATTACHMENTS = 6;
const MAX_TEXT_ATTACHMENT_CHARS = 200_000;
const MAX_IMAGE_BYTES = 512 * 1024;
const MAX_PDF_BYTES = 20 * 1024 * 1024;

function readAttachment(file: File): Promise<PendingAttachment | null> {
  return new Promise((resolve) => {
    const isImage = file.type.startsWith("image/");
    const isPdf = file.type === "application/pdf" || file.name.toLowerCase().endsWith(".pdf");
    if (isImage && file.size > MAX_IMAGE_BYTES) {
      resolve(null);
      return;
    }
    if (isPdf && file.size > MAX_PDF_BYTES) {
      resolve(null);
      return;
    }
    const reader = new FileReader();
    reader.onerror = () => resolve(null);
    if (isImage) {
      reader.onload = () => {
        const dataUrl = typeof reader.result === "string" ? reader.result : "";
        const base64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
        if (!base64) {
          resolve(null);
          return;
        }
        resolve({
          id: nextId(),
          kind: "image",
          name: file.name,
          size: file.size,
          mime: file.type || "image/png",
          previewUrl: dataUrl,
          base64,
        });
      };
      reader.readAsDataURL(file);
    } else if (isPdf) {
      reader.onload = () => {
        const dataUrl = typeof reader.result === "string" ? reader.result : "";
        const base64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
        if (!base64) {
          resolve(null);
          return;
        }
        resolve({
          id: nextId(),
          kind: "pdf",
          name: file.name,
          size: file.size,
          mime: file.type || "application/pdf",
          base64,
        });
      };
      reader.readAsDataURL(file);
    } else {
      reader.onload = () => {
        const text = typeof reader.result === "string" ? reader.result : "";
        const truncated = text.length > MAX_TEXT_ATTACHMENT_CHARS;
        resolve({
          id: nextId(),
          kind: "file",
          name: file.name,
          size: file.size,
          content: truncated ? `${text.slice(0, MAX_TEXT_ATTACHMENT_CHARS)}\n...[内容过长，已截断]` : text,
        });
      };
      reader.readAsText(file);
    }
  });
}

function pathBasename(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  const idx = normalized.lastIndexOf("/");
  return idx >= 0 ? normalized.slice(idx + 1) : normalized;
}

function imageMimeForPath(path: string): string {
  const lower = path.toLowerCase();
  if (lower.endsWith(".jpg") || lower.endsWith(".jpeg")) return "image/jpeg";
  if (lower.endsWith(".gif")) return "image/gif";
  if (lower.endsWith(".webp")) return "image/webp";
  if (lower.endsWith(".bmp")) return "image/bmp";
  if (lower.endsWith(".svg")) return "image/svg+xml";
  return "image/png";
}

async function readAttachmentFromPath(path: string): Promise<PendingAttachment | null> {
  const name = pathBasename(path);
  const lower = path.toLowerCase();
  const isImage = /\.(png|jpe?g|gif|webp|bmp|svg)$/.test(lower);
  const isPdf = lower.endsWith(".pdf");
  try {
    if (isImage) {
      const base64 = await localFsService.readBinaryPreview(path);
      const mime = imageMimeForPath(path);
      return { id: nextId(), kind: "image", name, size: 0, mime, previewUrl: `data:${mime};base64,${base64}`, base64 };
    }
    if (isPdf) {
      const base64 = await localFsService.readBinaryPreview(path);
      return { id: nextId(), kind: "pdf", name, size: 0, mime: "application/pdf", base64 };
    }
    const file = await localFsService.readFile(path);
    const text = file.text;
    const truncated = text.length > MAX_TEXT_ATTACHMENT_CHARS;
    return {
      id: nextId(),
      kind: "file",
      name,
      size: 0,
      content: truncated ? `${text.slice(0, MAX_TEXT_ATTACHMENT_CHARS)}\n...[内容过长，已截断]` : text,
    };
  } catch (e) {
    console.error("读取拖拽文件失败", path, e);
    return null;
  }
}

function toChatAttachment(attachment: PendingAttachment): ChatAttachment {
  if (attachment.kind === "image") {
    return { kind: "image", name: attachment.name, mime: attachment.mime ?? "image/png", data_base64: attachment.base64 ?? "" };
  }
  if (attachment.kind === "pdf") {
    return { kind: "pdf", name: attachment.name, data_base64: attachment.base64 ?? "" };
  }
  return { kind: "file", name: attachment.name, content: attachment.content ?? "" };
}

let listenersRegistered = false;

const HISTORY_CHECKPOINT_THROTTLE_MS = 5000;
let lastHistoryCheckpointAt = 0;
function checkpointHistory() {
  const now = Date.now();
  if (now - lastHistoryCheckpointAt < HISTORY_CHECKPOINT_THROTTLE_MS) return;
  lastHistoryCheckpointAt = now;
  void useCodingStore.getState().saveCurrentHistory();
}

/** Registers the coding-agent event listeners once globally (call from
 * `App.tsx` on mount). `listenersRegistered` only guards against double
 * registration within one mount, not "forever" -- see the host's original
 * comment on this same pattern for the React StrictMode double-invoke
 * caveat this guards against. */
export function registerCodingListeners(): Promise<() => void> {
  if (listenersRegistered) return Promise.resolve(() => {});
  listenersRegistered = true;

  const currentSessionId = () => useCodingStore.getState().sessionInfo?.id;

  const unlistenPromises = [
    listen<CodingToolCallEvent>("coding:tool-call-start", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      useCodingStore.setState((s) => ({
        timeline: [...s.timeline, { kind: "tool", id: nextId(), tool: event.payload.tool, running: true, detail: event.payload.detail, startedAt: Date.now() }],
      }));
    }),
    listen<CodingToolCallEvent>("coding:tool-call-end", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      useCodingStore.setState((s) => {
        const idx = [...s.timeline].reverse().findIndex((t) => t.kind === "tool" && t.tool === event.payload.tool && t.running);
        if (idx === -1) return s;
        const realIdx = s.timeline.length - 1 - idx;
        const timeline = [...s.timeline];
        const entry = timeline[realIdx];
        if (entry.kind === "tool") timeline[realIdx] = { ...entry, running: false, output: event.payload.output };
        return { timeline };
      });
      checkpointHistory();
    }),
    listen<CodingAssistantNoteEvent>("coding:assistant-note", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      useCodingStore.setState((s) => ({
        timeline: [...s.timeline, event.payload.kind === "status"
          ? { kind: "progress", id: nextId(), text: event.payload.text }
          : { kind: "note", id: nextId(), text: event.payload.text }],
      }));
    }),
    listen<CodingTokenUsageEvent>("coding:token-usage", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      useCodingStore.setState({
        liveTokenUsage: {
          promptTokens: event.payload.promptTokens,
          completionTokens: event.payload.completionTokens,
          totalTokens: event.payload.totalTokens,
        },
      });
    }),
    listen<CodingTokenUsageEvent>("coding:token-usage-summary", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      useCodingStore.setState((s) => ({
        liveTokenUsage: null,
        timeline: [
          ...s.timeline,
          {
            kind: "usage",
            id: nextId(),
            promptTokens: event.payload.promptTokens,
            completionTokens: event.payload.completionTokens,
            totalTokens: event.payload.totalTokens,
            isTurnTotal: true,
          },
        ],
      }));
      checkpointHistory();
    }),
    listen<CodingFileChangeEvent>("coding:file-change", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      const change = event.payload.change;
      useCodingStore.setState((s) => ({
        changesById: { ...s.changesById, [change.id]: change },
        timeline: [...s.timeline, { kind: "change", id: nextId(), changeId: change.id }],
      }));
      if (event.payload.sync) {
        const sync = event.payload.sync;
        useEditorStore.getState().syncExternalWrite(sync.path, sync.content, sync.mtime);
      }
      checkpointHistory();
    }),
    listen<CodingCommandBlockedEvent>("coding:command-blocked", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      useCodingStore.setState((s) => ({
        timeline: [...s.timeline, { kind: "blocked", id: nextId(), command: event.payload.command }],
      }));
    }),
    listen<CodingCommandConfirmRequestEvent>("coding:command-confirm-request", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      const incoming: CommandConfirmRequest = {
        requestId: event.payload.requestId,
        command: event.payload.command,
        host: event.payload.host,
        kind: event.payload.kind ?? "command",
        matchKey: event.payload.matchKey,
      };
      useCodingStore.setState((s) =>
        s.confirmRequest
          ? { confirmQueue: [...s.confirmQueue, incoming] }
          : { confirmRequest: incoming }
      );
    }),
    listen<CodingTodoUpdateEvent>("coding:todo-update", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      useCodingStore.setState((s) => (s.sessionInfo ? { sessionInfo: { ...s.sessionInfo, todos: event.payload.todos } } : s));
    }),
    listen<CodingQuestionRequestEvent>("coding:question-request", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      useCodingStore.setState({
        questionRequest: { requestId: event.payload.requestId, question: event.payload.question, options: event.payload.options },
      });
    }),
    listen<CodingGitCommitResultEvent>("coding:git-commit-result", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      useCodingStore.setState((s) => ({
        timeline: [...s.timeline, { kind: "git", id: nextId(), path: event.payload.path, output: event.payload.output }],
      }));
    }),
    listen<CodingAutoContinueStartEvent>("coding:auto-continue-start", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      useCodingStore.setState((s) => ({
        sending: true,
        timeline: [...s.timeline, { kind: "note", id: nextId(), text: event.payload.note }],
      }));
    }),
    listen<CodingAutoContinueDoneEvent>("coding:auto-continue-done", (event) => {
      if (event.payload.sessionId !== currentSessionId()) return;
      if (event.payload.error?.includes("已停止：用户取消了当前对话轮次")) {
        useCodingStore.setState((s) => ({
          sending: false,
          timeline: [
            ...s.timeline.map((t) => (t.kind === "tool" && t.running ? { ...t, running: false } : t)),
            { kind: "note", id: nextId(), text: "已停止" },
          ],
        }));
        return;
      }
      useCodingStore.setState((s) => ({
        sending: false,
        timeline: event.payload.reply
          ? [...s.timeline, { kind: "assistant", id: nextId(), text: event.payload.reply }]
          : event.payload.error
            ? [...s.timeline, { kind: "note", id: nextId(), text: `自动继续失败：${event.payload.error}` }]
            : s.timeline,
      }));
      void useCodingStore.getState().saveCurrentHistory();
    }),
  ];

  return Promise.all(unlistenPromises).then((unlistens) => () => {
    listenersRegistered = false;
    unlistens.forEach((u) => u());
  });
}
