import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Send, Bot, User, GitCommitHorizontal, Brain, ChevronRight, Sparkles, Settings, History, Plus, ShieldCheck, Plug, Blocks, BookOpen, CircleDot, CircleCheck, Circle, Paperclip, Wand2, X, Square } from "lucide-react";
import { useCodingStore, type TimelineEntry } from "../../stores/codingStore";
import { useExternalFileDrop } from "../../hooks/useExternalFileDrop";
import { formatTokenCount } from "../../utils/formatTokens";
import { useAiProviderStore } from "../../stores/aiProviderStore";
import { useEditorStore } from "@roc_desk/tool-editor";
import { detectLanguage } from "../../utils/language";
import { SegmentedControl } from "../shared/SegmentedControl";
import { ToggleSwitch } from "../shared/ToggleSwitch";
import { TargetBadge } from "./TargetBadge";
import { RemoteCapabilityBadge } from "./RemoteCapabilityBadge";
import { ToolCallProgress, toolLabel } from "./ToolCallProgress";
import { FileChangeCard, type DiffLine as CardDiffLine } from "./FileChangeCard";
import { CommandConfirmDialog, BlockedCommandMessage } from "./CommandConfirmDialog";
import { QuestionDialog } from "./QuestionDialog";
import { PermissionRulesDialog } from "./PermissionRulesDialog";
import { McpServerManagerDialog } from "./McpServerManagerDialog";
import { SkillManagerDialog } from "./SkillManagerDialog";
import { AgentMarkdown } from "./AgentMarkdown";
import { ProviderManagerDialog, hasProviderDraft } from "./ProviderManagerDialog";
import { CodingHistoryDialog } from "./CodingHistoryDialog";
import { useToastStore } from "../shared/Toast";
import { formatError } from "../../utils/error";
import type { CodingTarget, FileChange, TodoStatus } from "../../services";

/** Truncates preview text -- a collapsed row/table view only needs the gist, full content lives in the detail dialog. */
function truncatePreview(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
}

/** Summary of one round (from a "user" entry up to, but not including, the
 * next one) -- the collapsed row/table view uses this to render "what the
 * user asked, what the AI last said, how many tool calls/file changes" so
 * the whole round's detail doesn't have to be expanded just to get the
 * gist. */
function summarizeRound(items: { entry: TimelineEntry }[]): {
  userPreview: string;
  answerPreview: string;
  toolCount: number;
  changeCount: number;
  totalTokens: number | null;
} {
  let userText = "";
  let lastAssistantText = "";
  let lastNoteText = "";
  let toolCount = 0;
  let changeCount = 0;
  let blockedCount = 0;
  let totalTokens: number | null = null;
  for (const { entry } of items) {
    if (entry.kind === "user") userText = entry.text;
    else if (entry.kind === "assistant") lastAssistantText = entry.text;
    else if (entry.kind === "note") lastNoteText = entry.text;
    else if (entry.kind === "tool") toolCount += 1;
    else if (entry.kind === "change") changeCount += 1;
    else if (entry.kind === "blocked") blockedCount += 1;
    else if (entry.kind === "usage" && entry.isTurnTotal) totalTokens = entry.totalTokens;
  }
  const answerSource = lastAssistantText || lastNoteText || (blockedCount > 0 ? "命令被拦截" : "");
  return {
    userPreview: userText ? truncatePreview(userText, 60) : "（无文字，仅附件）",
    answerPreview: answerSource ? truncatePreview(answerSource, 80) : "（进行中或没有文字回复）",
    toolCount,
    changeCount,
    totalTokens,
  };
}

/** Data for a collapsed row's batch-action bar -- grouped by the backend's
 * real `turn_id` (not the frontend's "round", see `roundGroups`'s doc: an
 * injected message can make one round carry more than one `turn_id`).
 * Normally one `turn_id` corresponds to the round in this row, but in that
 * edge case there can be more than one group -- render each, don't assume
 * exactly one. */
function computeTurnBatches(
  items: { entry: TimelineEntry }[],
  changesById: Record<string, FileChange>,
  turnStats: Map<string, { changeIds: string[]; lastEntryId: string }>,
): Array<{ turnId: string; pendingInTurn: FileChange[]; appliedInTurn: FileChange[] }> {
  const seenTurnIds = new Set<string>();
  const batches: Array<{ turnId: string; pendingInTurn: FileChange[]; appliedInTurn: FileChange[] }> = [];
  for (const { entry } of items) {
    if (entry.kind !== "change") continue;
    const change = changesById[entry.changeId];
    if (!change || seenTurnIds.has(change.turn_id)) continue;
    seenTurnIds.add(change.turn_id);
    const stat = turnStats.get(change.turn_id);
    const turnChanges = (stat?.changeIds ?? []).map((id) => changesById[id]).filter((c): c is FileChange => Boolean(c));
    batches.push({
      turnId: change.turn_id,
      pendingInTurn: turnChanges.filter((c) => c.status === "pending"),
      appliedInTurn: turnChanges.filter((c) => c.status === "applied"),
    });
  }
  return batches;
}

function todoIcon(status: TodoStatus) {
  if (status === "completed") return <CircleCheck style={{ width: 13, height: 13, color: "var(--accent)" }} />;
  if (status === "in_progress") return <CircleDot style={{ width: 13, height: 13, color: "var(--warning)" }} />;
  return <Circle style={{ width: 13, height: 13, color: "var(--text-secondary)" }} />;
}

/** Default "remember this pattern" suggestion for `run_command`: first word
 * + " *" (e.g. `npm install` -> `npm *`) -- usually more useful than an
 * exact-match pattern, since the common intent is "allow this whole class
 * of command", not "only this exact one". Still freely editable. */
function suggestCommandPattern(command: string): string {
  const first = command.trim().split(/\s+/)[0];
  return first ? `${first} *` : command;
}

interface CodingAgentPanelProps {
  workspaceId: string;
  active: boolean;
  onOpenFile?: (path: string, line?: number) => void;
}

function targetLabel(target: CodingTarget): string {
  return target.kind === "Local" ? "本地" : target.host_label;
}

export const ThinkingBlock: React.FC<{ text: string; active: boolean; onOpenFile?: (path: string, line?: number) => void }> = ({ text, active, onOpenFile }) => {
  const detailsRef = useRef<HTMLDetailsElement>(null);
  useEffect(() => {
    if (detailsRef.current) detailsRef.current.open = active;
  }, [active]);
  return (
    <details ref={detailsRef} className="agent-thinking">
      <summary><ChevronRight className="agent-thinking-chevron" /><Brain /> <span>{active ? "正在思考" : "思考过程"}</span></summary>
      <div className="agent-thinking-body"><AgentMarkdown content={text} onOpenFile={onOpenFile} /></div>
    </details>
  );
};

export const CodingAgentPanel: React.FC<CodingAgentPanelProps> = ({ workspaceId, active, onOpenFile }) => {
  const {
    sessionInfo,
    timeline,
    changesById,
    sending,
    liveTokenUsage,
    error,
    confirmRequest,
    questionRequest,
    start,
    setMode,
    setProvider,
    setAutoAllowReadonly,
    setAutoGitCommit,
    setFullAuto,
    setAutoApplyChanges,
    sendMessage,
    cancelTurn,
    attachments,
    addAttachments,
    addAttachmentsFromPaths,
    removeAttachment,
    optimizing,
    optimizePrompt,
    acceptChange,
    rejectChange,
    undoChange,
    toggleToolOutput,
    revertTurn,
    resolveConfirm,
    resolveConfirmAndRemember,
    answerQuestion,
    histories,
    viewingHistoryId,
    loadHistories,
    openHistory,
    deleteHistory,
    renameHistory,
    newSession,
  } = useCodingStore();
  const providers = useAiProviderStore((s) => s.providers);
  const loadProviders = useAiProviderStore((s) => s.loadProviders);
  const modelsByProvider = useAiProviderStore((s) => s.modelsByProvider);
  const fetchModels = useAiProviderStore((s) => s.fetchModels);
  const updateProvider = useAiProviderStore((s) => s.updateProvider);
  const push = useToastStore((s) => s.push);
  // Keep keystrokes out of the large timeline render tree.  The composer is a
  // native uncontrolled textarea; React only receives a debounced snapshot for
  // button state/optimization, while send always reads the latest ref value.
  const [input, setInput] = useState("");
  const inputValueRef = useRef("");
  const inputElementRef = useRef<HTMLTextAreaElement>(null);
  const inputSyncTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [selectedProviderId, setSelectedProviderId] = useState<string>("");
  const [showProviders, setShowProviders] = useState(false);
  const [providerDraftPending, setProviderDraftPending] = useState(() => hasProviderDraft());
  const [showHistory, setShowHistory] = useState(false);
  const [showPermissionRules, setShowPermissionRules] = useState(false);
  const [showMcpServers, setShowMcpServers] = useState(false);
  const [showSkills, setShowSkills] = useState(false);
  // Only the current/latest round renders fully expanded (user question +
  // process + AI's final reply); earlier rounds collapse into a single
  // summary row (round number + user-question preview + AI-answer preview +
  // tool/change counts). Click that row to see the full detail in a dialog.
  const [detailRound, setDetailRound] = useState<number | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const shouldFollowBottomRef = useRef(true);
  const stableScrollTopRef = useRef(0);
  const suppressScrollEventRef = useRef(false);
  const scrollingRef = useRef(false);
  const scrollEndTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [showJumpBottom, setShowJumpBottom] = useState(false);
  const showJumpBottomRef = useRef(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const composerRef = useRef<HTMLDivElement>(null);
  const { isDragOver: draggingAttachments } = useExternalFileDrop(composerRef, (paths) => {
    void addAttachmentsFromPaths(paths);
  });

  // A 500ms re-render heartbeat while `sending` is true, purely so the
  // fixed "is the AI still running" status bar keeps updating between
  // backend events (no new timeline entry arrives while the model is still
  // thinking between tool calls, which otherwise looks identical to
  // "finished").
  const [liveTick, setLiveTick] = useState(0);
  useEffect(() => {
    if (!sending) return;
    const timer = setInterval(() => setLiveTick((t) => t + 1), 500);
    return () => clearInterval(timer);
  }, [sending]);

  const restoredWorkspaceRef = useRef<string | null>(null);

  useEffect(() => {
    loadProviders();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!active) {
      restoredWorkspaceRef.current = null;
      return;
    }
    if (providers.length === 0) {
      restoredWorkspaceRef.current = null;
      return;
    }
    if (restoredWorkspaceRef.current === workspaceId) return;
    restoredWorkspaceRef.current = workspaceId;
    const providerId = selectedProviderId || providers[0].id;
    useCodingStore.getState().restoreOrStart(workspaceId, providerId);
  }, [active, workspaceId, providers, selectedProviderId]);

  useEffect(() => {
    if (!active || !sessionInfo || providers.length === 0) return;
    if (providers.some((provider) => provider.id === sessionInfo.provider_id)) return;
    const fallback = providers.find((provider) => provider.id === selectedProviderId) ?? providers[0];
    void useCodingStore.getState().setProvider(fallback.id);
  }, [active, providers, selectedProviderId, sessionInfo]);

  useEffect(() => {
    if (!active || !sessionInfo?.provider_id) return;
    void fetchModels(sessionInfo.provider_id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, sessionInfo?.provider_id]);

  useEffect(() => {
    if (providers.length > 0 && !providers.some((provider) => provider.id === selectedProviderId)) {
      setSelectedProviderId(providers[0].id);
    }
  }, [providers, selectedProviderId]);

  useLayoutEffect(() => {
    const list = listRef.current;
    if (!list) return;
    const maxScrollTop = Math.max(0, list.scrollHeight - list.clientHeight);
    if (shouldFollowBottomRef.current) {
      suppressScrollEventRef.current = true;
      list.scrollTop = maxScrollTop;
      stableScrollTopRef.current = list.scrollTop;
      requestAnimationFrame(() => { suppressScrollEventRef.current = false; });
    } else if (list.scrollTop > maxScrollTop) {
      list.scrollTop = maxScrollTop;
      stableScrollTopRef.current = maxScrollTop;
    }
  }, [timeline]);

  useEffect(() => {
    const list = listRef.current;
    if (!list) return;
    let frame = 0;
    const target = list.querySelector<HTMLElement>(".agent-timeline-content") ?? list;
    const observer = new ResizeObserver(() => {
      if (!shouldFollowBottomRef.current || scrollingRef.current) return;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        list.scrollTop = Math.max(0, list.scrollHeight - list.clientHeight);
      });
    });
    observer.observe(target);
    return () => { cancelAnimationFrame(frame); observer.disconnect(); };
  }, []);

  const handleTimelineScroll = () => {
    const list = listRef.current;
    if (!list) return;
    if (suppressScrollEventRef.current) return;
    scrollingRef.current = true;
    if (scrollEndTimerRef.current) clearTimeout(scrollEndTimerRef.current);
    scrollEndTimerRef.current = setTimeout(() => {
      scrollingRef.current = false;
      if (shouldFollowBottomRef.current && listRef.current) {
        listRef.current.scrollTop = Math.max(0, listRef.current.scrollHeight - listRef.current.clientHeight);
      }
    }, 120);
    const distance = list.scrollHeight - list.scrollTop - list.clientHeight;
    const follow = distance <= 48;
    shouldFollowBottomRef.current = follow;
    stableScrollTopRef.current = list.scrollTop;
    const nextShowJump = !follow;
    if (showJumpBottomRef.current !== nextShowJump) {
      showJumpBottomRef.current = nextShowJump;
      setShowJumpBottom(nextShowJump);
    }
  };

  const scrollTimelineToBottom = () => {
    const list = listRef.current;
    if (!list) return;
    shouldFollowBottomRef.current = true;
    showJumpBottomRef.current = false;
    setShowJumpBottom(false);
    // A smooth-scroll animation fires a stream of intermediate scroll
    // events; without suppressing them, `handleTimelineScroll` would judge
    // "more than 48px from the bottom" partway through the animation and
    // flip `shouldFollowBottomRef`/`showJumpBottom` straight back --
    // exactly the bug where clicking the button didn't actually reach the
    // bottom. Suppress scroll events for the whole animation via
    // `suppressScrollEventRef`, releasing on `scrollend` (or a timer
    // covering the animation duration where that event isn't supported).
    suppressScrollEventRef.current = true;
    list.scrollTo({ top: Math.max(0, list.scrollHeight - list.clientHeight), behavior: "smooth" });
    const clearSuppress = () => { suppressScrollEventRef.current = false; };
    if ("onscrollend" in list) {
      list.addEventListener("scrollend", clearSuppress, { once: true });
    } else {
      setTimeout(clearSuppress, 500);
    }
  };

  // Groups timeline entries by round (boundary = a "user" entry) -- same
  // concept as the backend's `current_turn_id`/`turn_id`, just counted by
  // ordinal position of "user" entries here, not actually tied to turn_id.
  const roundOfIndex = React.useMemo(() => {
    const rounds: number[] = [];
    let round = -1;
    for (const entry of timeline) {
      if (entry.kind === "user") round += 1;
      rounds.push(round);
    }
    return rounds;
  }, [timeline]);
  const currentRound = roundOfIndex.length > 0 ? roundOfIndex[roundOfIndex.length - 1] : -1;

  // Slices the timeline by `roundOfIndex` -- only decides "should this
  // round render expanded by default", not the same thing as grouping by
  // the backend's real `turn_id` (that's `turnStats`'s job): an injected
  // message mid-turn can add a new "user" bubble (and therefore a new
  // round here) while the backend still counts it as the same `turn_id`,
  // so batch-action buttons keep computing off `turnStats`.
  const roundGroups = React.useMemo(() => {
    const groups: { round: number; items: { entry: TimelineEntry; index: number }[] }[] = [];
    timeline.forEach((entry, index) => {
      const round = roundOfIndex[index];
      const last = groups[groups.length - 1];
      if (last && last.round === round) {
        last.items.push({ entry, index });
      } else {
        groups.push({ round, items: [{ entry, index }] });
      }
    });
    return groups;
  }, [timeline, roundOfIndex]);

  const turnStats = React.useMemo(() => {
    const stats = new Map<string, { changeIds: string[]; lastEntryId: string }>();
    for (const entry of timeline) {
      if (entry.kind !== "change") continue;
      const change = changesById[entry.changeId];
      if (!change) continue;
      const existing = stats.get(change.turn_id);
      if (existing) {
        existing.changeIds.push(change.id);
        existing.lastEntryId = entry.id;
      } else {
        stats.set(change.turn_id, { changeIds: [change.id], lastEntryId: entry.id });
      }
    }
    return stats;
  }, [timeline, changesById]);

  useEffect(() => {
    setDetailRound(null);
  }, [workspaceId, viewingHistoryId]);

  useEffect(() => {
    if (detailRound === null) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setDetailRound(null);
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [detailRound]);

  const handleStart = () => {
    if (!selectedProviderId) return;
    start(workspaceId, selectedProviderId);
  };

  const handleSend = () => {
    const value = inputValueRef.current;
    if (!value.trim() && attachments.length === 0) return;
    sendMessage(value);
    inputValueRef.current = "";
    if (inputElementRef.current) inputElementRef.current.value = "";
    setInput("");
  };

  const handleOptimize = async () => {
    const value = inputValueRef.current;
    if (!value.trim() || optimizing) return;
    const optimized = await optimizePrompt(value);
    inputValueRef.current = optimized;
    if (inputElementRef.current) inputElementRef.current.value = optimized;
    setInput(optimized);
  };

  const handleFilesSelected = (files: FileList | null) => {
    if (!files || files.length === 0) return;
    addAttachments(Array.from(files));
    if (fileInputRef.current) fileInputRef.current.value = "";
  };

  const handlePaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const items = e.clipboardData?.items;
    if (!items || items.length === 0) return;
    const files = Array.from(items)
      .filter((item) => item.kind === "file")
      .map((item) => item.getAsFile())
      .filter((file): file is File => file !== null);
    if (files.length === 0) return;
    e.preventDefault();
    addAttachments(files);
  };

  if (!sessionInfo) {
    return (
      <div style={{ display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", height: "100%", gap: 12 }}>
        {providers.length === 0 ? (
          <>
            <div style={{ fontSize: 13, color: "var(--text-secondary)" }}>需要先配置一个 AI Provider</div>
            <button className="btn primary sm" onClick={() => setShowProviders(true)}>
              <Settings style={{ width: 14, height: 14 }} /> {providerDraftPending ? "继续配置" : "配置模型"}
            </button>
            {showProviders && <ProviderManagerDialog onClose={(hasDraft) => { setShowProviders(false); setProviderDraftPending(hasDraft); }} />}
          </>
        ) : (
          <>
            <select className="form-select" value={selectedProviderId} onChange={(e) => setSelectedProviderId(e.target.value)}>
              {providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
            <button className="btn primary sm" onClick={handleStart}>
              开始 AI 会话
            </button>
            {error && <div style={{ padding: "0 12px", fontSize: 12, color: "var(--danger)", textAlign: "center", maxWidth: 320 }}>{error}</div>}
          </>
        )}
      </div>
    );
  }

  const runBatch = async (items: FileChange[], action: (id: string) => Promise<void>, verb: string) => {
    let failed = 0;
    let lastError: unknown = null;
    for (const item of items) {
      try {
        await action(item.id);
      } catch (e) {
        failed += 1;
        lastError = e;
      }
    }
    if (failed > 0) {
      push("error", `${items.length} 个改动中有 ${failed} 个${verb}失败：${formatError(lastError)}`);
    }
  };

  const isRemote = sessionInfo.target.kind === "Remote";
  const activeProvider = providers.find((provider) => provider.id === sessionInfo.provider_id);
  const activeThinkingId = [...timeline].reverse().find((entry) => entry.kind === "note")?.id;
  const hasDraft = providerDraftPending || hasProviderDraft();

  const lastEntry = timeline[timeline.length - 1];
  const liveStatusText = !sending
    ? null
    : lastEntry?.kind === "tool" && lastEntry.running
    ? `正在执行 ${toolLabel(lastEntry.tool)}${lastEntry.detail ? ` · ${lastEntry.detail}` : ""}`
    : lastEntry?.kind === "note"
    ? "AI 正在思考…"
    : "AI 正在处理…";
  const liveTokenText = sending && liveTokenUsage ? `已用 ${formatTokenCount(liveTokenUsage.totalTokens)} tokens` : null;

  const renderEntry = (entry: TimelineEntry): React.ReactNode => {
    if (entry.kind === "user" || entry.kind === "assistant") {
      return (
        <div key={entry.id} style={{ display: "flex", gap: 8, alignItems: "flex-start" }}>
          <div
            style={{
              width: 22, height: 22, borderRadius: "50%", flexShrink: 0,
              display: "flex", alignItems: "center", justifyContent: "center",
              background: entry.kind === "user" ? "var(--bg-hover)" : "var(--accent-dim)",
              color: entry.kind === "user" ? "var(--text-secondary)" : "var(--accent)",
            }}
          >
            {entry.kind === "user" ? <User style={{ width: 13, height: 13 }} /> : <Bot style={{ width: 13, height: 13 }} />}
          </div>
          <div className={`agent-message ${entry.kind}`}>
            {entry.kind === "assistant" ? (
              <AgentMarkdown content={entry.text} onOpenFile={onOpenFile} />
            ) : (
              <>
                {entry.text && <div className="agent-user-text">{entry.text}</div>}
                {entry.attachments && entry.attachments.length > 0 && (
                  <div className="agent-attachment-list">
                    {entry.attachments.map((att, idx) =>
                      att.kind === "image" && att.previewUrl ? (
                        <img key={idx} src={att.previewUrl} alt={att.name} className="agent-attachment-thumb" title={att.name} />
                      ) : (
                        <span key={idx} className="agent-attachment-chip" title={att.name}>
                          <Paperclip style={{ width: 11, height: 11 }} /> {att.name}
                        </span>
                      )
                    )}
                  </div>
                )}
              </>
            )}
          </div>
        </div>
      );
    }
    if (entry.kind === "tool") {
      const hasFileTarget = ["read_file", "write_file", "edit_file", "list_directory", "multi_edit"].includes(entry.tool);
      return (
        <ToolCallProgress
          key={entry.id}
          tool={entry.tool}
          elapsedMs={entry.running && entry.startedAt ? Date.now() - entry.startedAt : 0}
          done={!entry.running}
          detail={entry.detail}
          onOpenFile={hasFileTarget && entry.detail && onOpenFile ? () => onOpenFile(entry.detail!) : undefined}
          output={entry.output}
          expanded={entry.expanded}
          onToggleOutput={() => toggleToolOutput(entry.id)}
        />
      );
    }
    if (entry.kind === "note") {
      return <ThinkingBlock key={entry.id} text={entry.text} active={sending && entry.id === activeThinkingId} onOpenFile={onOpenFile} />;
    }
    if (entry.kind === "progress") {
      return <div key={entry.id} className="agent-progress-note"><Sparkles /><span>{entry.text}</span></div>;
    }
    if (entry.kind === "blocked") {
      return <BlockedCommandMessage key={entry.id} command={entry.command} />;
    }
    if (entry.kind === "git") {
      return (
        <div key={entry.id} style={{ fontSize: 12, color: "var(--text-secondary)", display: "flex", gap: 6, alignItems: "flex-start" }}>
          <GitCommitHorizontal style={{ width: 14, height: 14, flexShrink: 0, marginTop: 2 }} />
          <div>
            <div>Git 提交 {entry.path}</div>
            <pre style={{ margin: 0, fontFamily: "var(--font-mono)", whiteSpace: "pre-wrap", fontSize: 11 }}>{entry.output}</pre>
          </div>
        </div>
      );
    }
    if (entry.kind === "usage") {
      return entry.isTurnTotal ? (
        <div key={entry.id} style={{ fontSize: 12, color: "var(--text-secondary)", fontWeight: 600, padding: "2px 0" }}>
          本轮对话共消耗 tokens：输入 {formatTokenCount(entry.promptTokens)} · 输出 {formatTokenCount(entry.completionTokens)} · 合计{" "}
          {formatTokenCount(entry.totalTokens)}
        </div>
      ) : (
        <div key={entry.id} style={{ fontSize: 11, color: "var(--text-secondary)", opacity: 0.65 }}>
          本次请求消耗 tokens：输入 {formatTokenCount(entry.promptTokens)} · 输出 {formatTokenCount(entry.completionTokens)} · 合计{" "}
          {formatTokenCount(entry.totalTokens)}
        </div>
      );
    }
    const change = changesById[entry.changeId];
    if (!change) return null;
    const diff: CardDiffLine[] = change.diff.map((l) => ({ sign: l.sign, content: l.content }));
    const status = change.status === "undone" ? "rejected" : change.status;
    const stat = turnStats.get(change.turn_id);
    const turnChanges = (stat?.changeIds ?? []).map((id) => changesById[id]).filter((c): c is FileChange => Boolean(c));
    const pendingInTurn = turnChanges.filter((c) => c.status === "pending");
    const appliedInTurn = turnChanges.filter((c) => c.status === "applied");
    const showBatchActions = !viewingHistoryId && stat?.lastEntryId === entry.id
      && (pendingInTurn.length > 0 || appliedInTurn.length > 0);
    return (
      <React.Fragment key={entry.id}>
        <FileChangeCard
          path={change.path}
          status={status}
          diff={diff}
          onViewDiff={() =>
            useEditorStore.getState().openDiffContent(
              `${change.path}（改动前）`,
              change.old_content,
              `${change.path}（改动后）`,
              change.new_content,
              detectLanguage(change.path),
            )
          }
          onAccept={viewingHistoryId ? undefined : () => acceptChange(change.id)}
          onReject={viewingHistoryId ? undefined : () => rejectChange(change.id)}
          onUndo={viewingHistoryId ? undefined : () => undoChange(change.id)}
        />
        {showBatchActions && (
          <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
            {pendingInTurn.length > 0 && (
              <>
                <button
                  className="btn ghost sm"
                  onClick={() => void runBatch(pendingInTurn, acceptChange, "应用")}
                >
                  全部应用（{pendingInTurn.length}）
                </button>
                <button
                  className="btn ghost sm"
                  onClick={() => void runBatch(pendingInTurn, rejectChange, "拒绝")}
                >
                  全部拒绝（{pendingInTurn.length}）
                </button>
              </>
            )}
            {appliedInTurn.length > 0 && (
              <button className="btn ghost sm" onClick={() => void revertTurn(change.turn_id)}>
                撤销本轮全部改动（{appliedInTurn.length}）
              </button>
            )}
          </div>
        )}
      </React.Fragment>
    );
  };

  const detailGroup = detailRound !== null ? roundGroups.find((g) => g.round === detailRound) ?? null : null;

  return (
    <div style={{ display: "flex", flexDirection: "column", flex: 1, minHeight: 0 }}>
      <div
        ref={listRef}
        onScroll={handleTimelineScroll}
        className="agent-timeline-scroll"
        style={{ flex: 1, minHeight: 0, overflowY: "auto", display: "flex", flexDirection: "column", position: "relative" }}
      >
      <div className="agent-toolbar" style={{ gap: 12, flexWrap: "wrap", height: "auto", minHeight: 32 }}>
        <SegmentedControl
          value={sessionInfo.mode}
          onChange={(m) => { if (!viewingHistoryId) setMode(m); }}
          options={[
            { value: "plan", label: "Plan" },
            { value: "build", label: "Build" },
          ]}
        />
        <select
          className="form-select agent-model-select"
          value={sessionInfo.provider_id}
          onChange={(event) => setProvider(event.target.value)}
          disabled={sending || Boolean(viewingHistoryId)}
          title="切换后续消息使用的 Provider"
        >
          {providers.map((provider) => (
            <option key={provider.id} value={provider.id}>
              {provider.name}
            </option>
          ))}
        </select>
        <TargetBadge targetLabel={targetLabel(sessionInfo.target)} isRemote={isRemote} />
        {isRemote && <RemoteCapabilityBadge />}
        {sessionInfo.project_memory_loaded.length > 0 && (
          <span
            style={{ display: "inline-flex", alignItems: "center", gap: 4, fontSize: 11, color: "var(--text-secondary)" }}
            title={`系统提示词已注入：${sessionInfo.project_memory_loaded.join(", ")}`}
          >
            <BookOpen style={{ width: 12, height: 12 }} /> {sessionInfo.project_memory_loaded.join(" / ")}
          </span>
        )}
        <button className="btn ghost sm" onClick={() => { setShowHistory(true); loadHistories(workspaceId); }} title="历史会话">
          <History style={{ width: 13, height: 13 }} /> 历史{histories.length ? ` (${histories.length})` : ""}
        </button>
        <button className="btn ghost sm" onClick={() => newSession(sessionInfo.provider_id)} disabled={sending} title="新建会话">
          <Plus style={{ width: 13, height: 13 }} /> 新会话
        </button>
        <button className="btn ghost sm" onClick={() => setShowPermissionRules(true)} title="权限规则管理">
          <ShieldCheck style={{ width: 13, height: 13 }} /> 权限规则
        </button>
        <button className="btn ghost sm" onClick={() => setShowMcpServers(true)} title="MCP 服务器管理">
          <Plug style={{ width: 13, height: 13 }} /> MCP
        </button>
        <button className="btn ghost sm" onClick={() => setShowSkills(true)} title="项目 Skills 查看/导入">
          <Blocks style={{ width: 13, height: 13 }} /> Skills
        </button>
        <button className={`btn ghost sm ${hasDraft ? "active" : ""}`} onClick={() => setShowProviders(true)}>
          <Settings style={{ width: 13, height: 13 }} /> {hasDraft ? "继续配置" : "模型管理"}
        </button>
        {sessionInfo.mode === "build" && !viewingHistoryId && (
          <>
            <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, color: "var(--text-secondary)" }}>
              <ToggleSwitch checked={sessionInfo.auto_allow_readonly} onChange={setAutoAllowReadonly} label="自动放行只读命令" />
              自动放行只读命令
            </label>
            <label
              style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, color: "var(--text-secondary)" }}
              title={"默认开启：AI 的文件改动直接写入磁盘，界面上只需要点\"撤销\"；关闭后退回每条改动手动点\"应用\"。不影响命令确认。"}
            >
              <ToggleSwitch checked={sessionInfo.auto_apply_changes} onChange={setAutoApplyChanges} label="自动应用文件改动" />
              自动应用文件改动
            </label>
            <label
              style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, color: "var(--text-secondary)" }}
              title={sessionInfo.git_repo ? "每次点击\"应用\"就自动 git add + commit 这个文件" : "工作区根目录不是 Git 仓库，无法使用"}
            >
              <ToggleSwitch
                checked={sessionInfo.auto_git_commit}
                onChange={setAutoGitCommit}
                disabled={!sessionInfo.git_repo}
                label="自动 Git 提交"
              />
              自动 Git 提交
            </label>
            <label
              style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, color: "var(--text-secondary)" }}
              title="开启后 AI 的文件改动直接写入磁盘，命令与 MCP 工具不再逐项确认；已弹出的当前会话命令确认也会立即放行。高危命令和显式拒绝的权限规则仍会生效。"
            >
              <ToggleSwitch checked={sessionInfo.full_auto} onChange={setFullAuto} label="完全授权模式" />
              完全授权模式
            </label>
          </>
        )}
      </div>

      {showJumpBottom && (
        <button className="agent-jump-bottom" onClick={scrollTimelineToBottom} title="跳到最新消息">
          ↓ 最新消息
        </button>
      )}

      {sessionInfo.todos.length > 0 && (
        <div style={{ padding: "6px 12px", borderBottom: "1px solid var(--border-subtle)", display: "flex", flexDirection: "column", gap: 3 }}>
          {sessionInfo.todos.map((todo) => (
            <div
              key={todo.id}
              style={{
                display: "flex", alignItems: "center", gap: 6, fontSize: 12,
                color: todo.status === "completed" ? "var(--text-secondary)" : "var(--text-primary)",
                textDecoration: todo.status === "completed" ? "line-through" : "none",
              }}
            >
              {todoIcon(todo.status)}
              <span>{todo.content}</span>
            </div>
          ))}
        </div>
      )}

      {viewingHistoryId && <div className="coding-history-banner">
        <span>正在查看历史会话（只读）</span>
        <button className="btn primary sm" onClick={() => newSession(activeProvider?.id ?? selectedProviderId ?? sessionInfo.provider_id)}>返回新会话</button>
      </div>}

      <div className="agent-timeline-content" style={{ padding: "8px 12px", display: "flex", flexDirection: "column", gap: 10 }}>
        {timeline.length === 0 ? (
          <div style={{ textAlign: "center", color: "var(--text-secondary)", fontSize: 13, marginTop: 24 }}>
            {sessionInfo.mode === "plan" ? "直接提问或描述任务；Plan 模式不会修改文件" : "提问，或描述你想做的改动"}
          </div>
        ) : (
          roundGroups.map((group) => {
            if (group.round === currentRound) {
              return (
                <div key={`round-${group.round}`} className="agent-round-current">
                  <div className="agent-round-current-label">
                    第 {group.round + 1} 轮{sending ? " · 进行中" : ""}
                  </div>
                  {group.items.map(({ entry }) => renderEntry(entry))}
                </div>
              );
            }
            const summary = summarizeRound(group.items);
            const batches = computeTurnBatches(group.items, changesById, turnStats);
            const hasBatchActions = !viewingHistoryId
              && batches.some((b) => b.pendingInTurn.length > 0 || b.appliedInTurn.length > 0);
            return (
              <div
                key={`round-${group.round}`}
                className="agent-round-row"
                role="button"
                tabIndex={0}
                onClick={() => setDetailRound(group.round)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    setDetailRound(group.round);
                  }
                }}
                title="点击查看这一轮的完整过程"
              >
                <span className="agent-round-row-index">#{group.round + 1}</span>
                <div className="agent-round-row-texts">
                  <div className="agent-round-row-user"><User style={{ width: 11, height: 11 }} /> {summary.userPreview}</div>
                  <div className="agent-round-row-answer"><Bot style={{ width: 11, height: 11 }} /> {summary.answerPreview}</div>
                </div>
                <div className="agent-round-row-meta">
                  {summary.toolCount > 0 && <span className="agent-round-row-badge">{summary.toolCount} 个工具</span>}
                  {summary.changeCount > 0 && <span className="agent-round-row-badge">{summary.changeCount} 处改动</span>}
                  {summary.totalTokens !== null && (
                    <span className="agent-round-row-badge">{formatTokenCount(summary.totalTokens)} tokens</span>
                  )}
                </div>
                {hasBatchActions && (
                  <div className="agent-round-row-actions" onClick={(e) => e.stopPropagation()}>
                    {batches.map((b) => (
                      <React.Fragment key={b.turnId}>
                        {b.pendingInTurn.length > 0 && (
                          <>
                            <button className="btn ghost sm" onClick={() => void runBatch(b.pendingInTurn, acceptChange, "应用")}>
                              应用（{b.pendingInTurn.length}）
                            </button>
                            <button className="btn ghost sm" onClick={() => void runBatch(b.pendingInTurn, rejectChange, "拒绝")}>
                              拒绝（{b.pendingInTurn.length}）
                            </button>
                          </>
                        )}
                        {b.appliedInTurn.length > 0 && (
                          <button className="btn ghost sm" onClick={() => void revertTurn(b.turnId)}>
                            撤销（{b.appliedInTurn.length}）
                          </button>
                        )}
                      </React.Fragment>
                    ))}
                  </div>
                )}
                <ChevronRight style={{ width: 14, height: 14, color: "var(--text-secondary)", flexShrink: 0 }} />
              </div>
            );
          })
        )}
      </div>
      </div>

      {detailGroup && (
        <div className="agent-round-detail-overlay" onClick={() => setDetailRound(null)}>
          <div className="agent-round-detail-dialog" onClick={(e) => e.stopPropagation()} role="dialog" aria-modal="true">
            <div className="agent-round-detail-header">
              <span>第 {detailGroup.round + 1} 轮详情</span>
              <button className="agent-round-detail-close" onClick={() => setDetailRound(null)} title="关闭（Esc）">
                <X style={{ width: 14, height: 14 }} />
              </button>
            </div>
            <div className="agent-round-detail-body">
              {detailGroup.items.map(({ entry }) => renderEntry(entry))}
            </div>
          </div>
        </div>
      )}

      {liveStatusText && (
        <div className="agent-live-status" key={liveTick}>
          <span className="agent-live-dot" />
          {liveStatusText}
          {liveTokenText && <span className="agent-live-tokens">· {liveTokenText}</span>}
        </div>
      )}

      {error && <div style={{ padding: "4px 12px", fontSize: 12, color: "var(--danger)" }}>{error}</div>}

      <div
        ref={composerRef}
        className={`agent-composer ${draggingAttachments ? "agent-composer-dragging" : ""}`}
      >
        {attachments.length > 0 && (
          <div className="agent-pending-attachments">
            {attachments.map((att) => (
              <div key={att.id} className="agent-pending-attachment">
                {att.kind === "image" && att.previewUrl ? (
                  <img src={att.previewUrl} alt={att.name} className="agent-attachment-thumb" title={att.name} />
                ) : (
                  <span className="agent-attachment-chip" title={att.name}>
                    <Paperclip style={{ width: 11, height: 11 }} /> {att.name}
                  </span>
                )}
                <button className="agent-pending-attachment-remove" onClick={() => removeAttachment(att.id)} title="移除附件">
                  <X style={{ width: 10, height: 10 }} />
                </button>
              </div>
            ))}
          </div>
        )}
        <textarea
          className="agent-composer-input"
          ref={inputElementRef}
          rows={3}
          disabled={Boolean(viewingHistoryId)}
          placeholder={
            sending
              ? "AI 正在处理，这时候按 Enter 会直接插进当前对话，不用等它说完"
              : "提问或描述任务，Enter 发送，Shift+Enter 换行，可直接粘贴图片"
          }
          defaultValue=""
          onChange={(e) => {
            inputValueRef.current = e.target.value;
            if (inputSyncTimerRef.current) clearTimeout(inputSyncTimerRef.current);
            inputSyncTimerRef.current = setTimeout(() => {
              setInput(inputValueRef.current);
              inputSyncTimerRef.current = null;
            }, 120);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              handleSend();
            }
          }}
          onPaste={handlePaste}
        />
        <div className="agent-composer-footer">
          <div className="agent-model-meta" title={activeProvider?.api_base}>
            <Sparkles />
            <select
              className="agent-model-select-inline"
              value={activeProvider?.model ?? ""}
              onChange={(event) => {
                if (!activeProvider) return;
                void updateProvider(activeProvider.id, {
                  name: activeProvider.name,
                  api_base: activeProvider.api_base,
                  api_key: null,
                  model: event.target.value,
                  is_local: activeProvider.is_local,
                  wire_api: activeProvider.wire_api,
                  reasoning_effort: activeProvider.reasoning_effort,
                  context_window_tokens: activeProvider.context_window_tokens,
                });
              }}
              disabled={sending || Boolean(viewingHistoryId) || !activeProvider}
              title="切换这个 Provider 使用的模型"
            >
              {(modelsByProvider[sessionInfo.provider_id]?.length
                ? modelsByProvider[sessionInfo.provider_id]
                : activeProvider
                ? [activeProvider.model]
                : []
              ).map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
            {activeProvider && <span>{activeProvider.name} · {activeProvider.is_local ? "本地" : "云端"}</span>}
            <span>· {sessionInfo.mode === "plan" ? "Plan" : "Build"}</span>
          </div>
          <span className="agent-input-hint">{sending ? "Enter 插话 · Shift+Enter 换行" : "Enter 发送 · Shift+Enter 换行"}</span>
          <input
            ref={fileInputRef}
            type="file"
            multiple
            accept="image/*,.pdf,application/pdf,.txt,.md,.json,.ts,.tsx,.js,.jsx,.py,.rs,.go,.java,.c,.cpp,.h,.hpp,.css,.html,.yaml,.yml,.toml,.csv,.log,.sh"
            style={{ display: "none" }}
            onChange={(e) => handleFilesSelected(e.target.files)}
          />
          <button
            className="agent-composer-icon-btn"
            onClick={() => fileInputRef.current?.click()}
            disabled={Boolean(viewingHistoryId)}
            title="添加图片/文件附件"
          >
            <Paperclip />
          </button>
          <button
            className="agent-composer-icon-btn"
            onClick={handleOptimize}
            disabled={optimizing || sending || Boolean(viewingHistoryId) || !input.trim()}
            title="优化输入：用当前模型把草稿改写成更清晰的提示词"
          >
            <Wand2 className={optimizing ? "agent-icon-spin" : undefined} />
          </button>
          {sending ? (
            <button
              className="agent-send-btn agent-stop-btn"
              onClick={cancelTurn}
              title="停止：中断当前正在进行的对话轮次"
            >
              <Square fill="currentColor" />
            </button>
          ) : (
            <button
              className="agent-send-btn"
              onClick={handleSend}
              disabled={Boolean(viewingHistoryId) || (!input.trim() && attachments.length === 0)}
              title="发送"
            >
              <Send />
            </button>
          )}
        </div>
      </div>

      {confirmRequest && (
        <CommandConfirmDialog
          open
          host={confirmRequest.host ?? undefined}
          command={confirmRequest.command}
          kind={confirmRequest.kind}
          suggestedPattern={confirmRequest.kind === "mcp" ? (confirmRequest.matchKey ?? confirmRequest.command) : suggestCommandPattern(confirmRequest.command)}
          onReject={() => resolveConfirm(false)}
          onAllowOnce={() => resolveConfirm(true)}
          onAllowAndRemember={(pattern) => resolveConfirmAndRemember(pattern)}
        />
      )}
      {questionRequest && (
        <QuestionDialog
          open
          question={questionRequest.question}
          options={questionRequest.options}
          onAnswer={(answer) => answerQuestion(answer)}
        />
      )}
      {showProviders && <ProviderManagerDialog onClose={(hasDraft) => { setShowProviders(false); setProviderDraftPending(hasDraft); }} />}
      {showHistory && <CodingHistoryDialog title="编程会话历史" emptyText="还没有已保存的编程会话" histories={histories} onOpen={(id) => { openHistory(id); setShowHistory(false); }} onDelete={deleteHistory} onRename={renameHistory} onClose={() => setShowHistory(false)} />}
      {showPermissionRules && <PermissionRulesDialog onClose={() => setShowPermissionRules(false)} />}
      {showMcpServers && <McpServerManagerDialog onClose={() => setShowMcpServers(false)} />}
      {showSkills && <SkillManagerDialog workspaceId={workspaceId} onClose={() => setShowSkills(false)} />}
    </div>
  );
};
