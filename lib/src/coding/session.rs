use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use roc_desk_common::agent_confirm::{CommandConfirmRegistry, QuestionRegistry};
use roc_desk_common::agent_llm;
use roc_desk_common::ai::attachments::{build_user_message_content, ChatAttachment};
use roc_desk_common::ai::{search_web_results, AiProvider, AiProviderManager};
use roc_desk_common::change_store::{ChangeStatus, ChangeStore, CodingTarget};
use roc_desk_common::fsops::{search_stream, FileOps, SearchMode, SearchOptions};
use roc_desk_common::symbols::{build_index, SymbolIndex, SymbolLocation};
use roc_desk_core::error::AppError;
use roc_desk_ssh::agent::AgentConnectionPool;
use roc_desk_ssh::ssh::SshConnectionPool;

use super::audit::AuditLogRepo;
use super::evidence::{AiEvidenceRepo, EvidenceEntry, MAX_EVIDENCE_BYTES};
use super::git_ops;
use super::guard;
use super::mcp::McpServerManager;
use super::permission::{Decision, PermissionEngine, PermissionRulesRepo};
use super::skills::{self, SkillMeta};
use super::tools::{self, TodoItem, ToolCall};
use super::webfetch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingMode {
    Plan,
    Build,
}

/// A message the user sent while the AI was still working on the previous
/// one -- not waited on until this round's tool loop fully finishes,
/// accumulated (one list per workspace, independent of `CodingSession`'s own
/// lock) and drained at the top of each `send_message` tool-loop iteration
/// so the model sees it on its next request. Can't live on `CodingSession`
/// itself -- `send_message` holds the session's outer lock from entry to
/// return, so a field inside the session could never be reached by another
/// command until this round ends.
#[derive(Debug, Clone)]
pub struct PendingInjection {
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
}

/// AI coding agent session: bound to an already-open workspace, at most one
/// active session per workspace in one process.
///
/// File edits follow a "generate Diff immediately, land on disk only after
/// the user Accepts" flow (pending/applied/rejected three-state, matching
/// `FileChangeCard.tsx`) rather than writing straight to disk and relying on
/// Undo to recover -- the core scenario for an AI coding agent is touching a
/// production server, where "undo after the fact" costs far more than "one
/// extra confirmation click". So a later `read_file` within the same turn
/// doesn't see "stale" content, `pending_content_for` prefers the
/// not-yet-written proposed content, keeping the model's reasoning
/// consistent with the Diff it already generated.
pub struct CodingSession {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub workspace_root: String,
    pub target: CodingTarget,
    pub mode: CodingMode,
    pub provider_id: Uuid,
    /// Independent, separately-locked container for file changes (Diff/
    /// Accept/Undo/Redo) -- deliberately not a direct field of
    /// `CodingSession` itself, see `ChangeStore`'s doc comment: Accept/
    /// Reject/Undo must not be stuck waiting behind a possibly
    /// minutes-long conversation turn holding this session's own lock. The
    /// caller is expected to keep another clone of the same `Arc` to expose
    /// accept/reject/undo commands directly, bypassing this struct (and
    /// therefore its lock) entirely.
    pub change_store: Arc<Mutex<ChangeStore>>,
    /// Task list maintained by the `todo_write` tool (OpenCode semantics:
    /// each call replaces the whole list, not an incremental patch); the
    /// frontend renders it as a persistent list above the conversation.
    pub todos: Vec<TodoItem>,
    /// Skills discovered from the current workspace's
    /// `.rock_desk/skills/*/SKILL.md` -- only scanned once at session start
    /// (same timing/rationale as the git-repo probe), doesn't pick up newly
    /// added skill directories during the session's lifetime.
    pub skills: Vec<SkillMeta>,
    /// Project-memory filenames (`AGENTS.md`/`CLAUDE.md`, whichever exist)
    /// actually read and injected into the system prompt for this session --
    /// the frontend uses it to render a "loaded XXX" badge in the toolbar.
    pub project_memory_loaded: Vec<String>,
    pub(crate) evidence_repo: Arc<AiEvidenceRepo>,
    evidence_recall_query: Option<String>,
    messages: Vec<serde_json::Value>,
    pub(crate) file_ops: Arc<dyn FileOps>,
    /// The user-message turn currently being processed -- a fresh one is
    /// generated at the top of `send_message`; every `FileChange` produced
    /// by `stage_change` during this turn is tagged with the same
    /// `turn_id`, letting the frontend do "whole turn" batch operations.
    current_turn_id: Uuid,
    /// When `send_message` wraps up (the model stops calling tools, gives a
    /// final reply) and this turn still has `Pending` file changes the user
    /// hasn't acted on, this records that turn's `turn_id`; the
    /// accept/reject commands check this field once "every change in this
    /// turn has now been handled" and, if it matches, automatically
    /// continue the conversation for the user (feedback: the AI would say
    /// "I'll continue with X once you confirm", the user clicks apply, and
    /// nothing happens -- they had to type another message to continue).
    /// `None` means there's no turn currently stuck waiting on confirmation.
    awaiting_confirmation_turn: Option<Uuid>,
    /// Background processes started by `run_command_background`, keyed by
    /// the job_id handed back to the model -- local target only. Lives on
    /// the session's own field (not a separate `AppState`-level lock like
    /// `change_store`) because only `execute_tool` (which already holds
    /// `&mut self`) ever reads/writes it -- no need for concurrent access
    /// from another command while `send_message` holds the turn's lock.
    /// When the session itself is dropped (workspace closed / new session
    /// started), `Drop` kills every job that wasn't already stopped via
    /// `stop_background_process`, so no dev-server-like child process is
    /// left running in the background holding a port after the user has
    /// left this session.
    background_jobs: HashMap<Uuid, BackgroundJob>,
}

impl Drop for CodingSession {
    fn drop(&mut self) {
        for (_, mut job) in self.background_jobs.drain() {
            let _ = job.child.start_kill();
        }
    }
}

/// A `run_command_background` job, either still running or finished but not
/// yet cleaned up via `stop_background_process`. `output` is the cumulative
/// output two background reader tasks (stdout/stderr) keep appending to,
/// using `Arc<StdMutex<..>>` rather than `tokio::sync::Mutex` -- the append
/// itself never crosses an `.await`, so the std-lib lock is enough, no need
/// for tokio's overhead.
struct BackgroundJob {
    command: String,
    child: tokio::process::Child,
    output: Arc<StdMutex<String>>,
}

/// Cap on one background job's cumulative output character count -- a dev
/// server/watch process may never exit and keep writing to stdout forever;
/// without a cap this `String` would grow unbounded. Trimmed to the most
/// recent slice once it exceeds `2x` the cap (not re-trimmed on every
/// line), `is_char_boundary`-safe so a multi-byte character is never cut in
/// half.
const MAX_BACKGROUND_OUTPUT_CHARS: usize = 20_000;

fn append_background_output(buf: &StdMutex<String>, text: &str) {
    let mut s = buf.lock().unwrap();
    s.push_str(text);
    if s.len() > MAX_BACKGROUND_OUTPUT_CHARS * 2 {
        let keep_from = s.len().saturating_sub(MAX_BACKGROUND_OUTPUT_CHARS);
        let mut idx = keep_from;
        while idx < s.len() && !s.is_char_boundary(idx) {
            idx += 1;
        }
        *s = s[idx..].to_string();
    }
}

/// In total, this session keeps only this many most-recent user turns even
/// when under the token budget -- avoids a very long-running session
/// slowly eating memory.
const MAX_CONTEXT_USER_TURNS: usize = 8;
/// Marks `self.messages[1]` (if present) as the "earlier-conversation
/// summary" message, not real history -- `limit_context` uses this prefix
/// to decide "is there already a summary message to append to, or does a
/// new one need to be created".
const CONTEXT_SUMMARY_PREFIX: &str = "【早前对话摘要】";
/// Character cap on the summary message itself -- in a long session that
/// keeps appending summaries, the summary itself would otherwise grow
/// unbounded; past this cap the earliest slice of the summary is dropped
/// (not re-summarized -- avoids over-engineering this).
const MAX_CONTEXT_SUMMARY_CHARS: usize = 16_000;
/// Timeout for the summary request itself -- summarization is a nice-to-
/// have, must not become a new place to hang; timing out/failing falls
/// straight back to a hard drop.
const CONTEXT_SUMMARY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Compresses a batch of turns about to be dropped from `self.messages` into
/// a 2-4 sentence summary -- sends one lightweight request without a
/// `tools` field, not counted as part of the tool loop itself (this is a
/// separate request triggered by `limit_context` itself, not part of the
/// tool loop). Any failure (network error, timeout, no usable text in the
/// response) returns `None`, and the caller falls back to the original
/// "these turns are just dropped, no trace left" behavior -- summarization
/// is a nice-to-have, must not become a new place to hang.
async fn summarize_dropped_turns(
    dropped: &[serde_json::Value],
    client: &reqwest::Client,
    provider: &AiProvider,
    api_key: &Option<String>,
) -> Option<String> {
    let transcript: String = dropped
        .iter()
        .filter_map(|message| {
            let role = message["role"].as_str()?;
            let content = match &message["content"] {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            let content: String = content.chars().take(800).collect();
            let tool_note = message["tool_calls"]
                .as_array()
                .filter(|calls| !calls.is_empty())
                .map(|calls| format!("（调用了 {} 个工具）", calls.len()))
                .unwrap_or_default();
            Some(format!("[{role}]{tool_note} {content}"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if transcript.trim().is_empty() {
        return None;
    }
    let url = format!(
        "{}/chat/completions",
        provider.api_base.trim_end_matches('/')
    );
    let body = json!({
        "model": provider.model,
        "messages": [
            {
                "role": "system",
                "content": "你是对话摘要助手。这段摘要是给正在处理同一个任务的编程助手接着往下\
                             用的背景资料，不是写给人看的概述，必须具体到文件路径和关键发现，\
                             不能只给模糊结论——后续对话要能直接引用这些路径和发现，不用重新\
                             搜索/读文件。按下面的格式输出中文内容，没有的部分直接省略，不要写\
                             占位词：\n\
                             已读文件：<路径1>：<这个文件里的关键发现，一两句>；<路径2>：...\n\
                             已确认结论：<明确的技术结论/决策，逐条列>\n\
                             下一步：<如果任务还没做完，下一步打算做什么>\n\
                             不要写客套话，不要逐句复述对话过程，只保留后续用得上的具体信息。"
            },
            { "role": "user", "content": transcript }
        ]
    });
    let mut req = client.post(&url).json(&body);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = match tokio::time::timeout(CONTEXT_SUMMARY_TIMEOUT, req.send()).await {
        Ok(Ok(resp)) => resp,
        _ => return None,
    };
    let body: serde_json::Value = match resp.json().await {
        Ok(body) => body,
        Err(_) => return None,
    };
    let text = body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Fallback for when `summarize_dropped_turns` fails (network error/timeout/
/// no usable text from the provider) -- previously the failure case simply
/// dropped those messages silently, with absolutely no trace left. This
/// doesn't call the model at all: it algorithmically pulls "what the user
/// said, which files were touched" out of the dropped messages. Lower
/// quality than the LLM-generated version, but a lossy trail beats total
/// disappearance.
fn deterministic_fallback_summary(dropped: &[serde_json::Value]) -> String {
    let mut user_texts = Vec::new();
    let mut file_paths = std::collections::BTreeSet::new();
    for message in dropped {
        if message["role"].as_str() == Some("user") {
            if let Some(text) = message["content"].as_str() {
                let trimmed: String = text.chars().take(200).collect();
                if !trimmed.trim().is_empty() {
                    user_texts.push(trimmed);
                }
            }
        }
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                if let Some(args) = call["function"]["arguments"].as_str() {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(args) {
                        for key in ["path", "file_path", "directory"] {
                            if let Some(p) = parsed[key].as_str() {
                                file_paths.insert(p.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    let mut summary =
        String::from("（自动摘要生成失败，以下是算法兜底提取的线索，细节不如正常摘要完整）\n");
    if !user_texts.is_empty() {
        summary.push_str("用户消息：");
        summary.push_str(&user_texts.join("；"));
        summary.push('\n');
    }
    if !file_paths.is_empty() {
        summary.push_str("涉及文件：");
        summary.push_str(&file_paths.into_iter().collect::<Vec<_>>().join("、"));
        summary.push('\n');
    }
    summary
}

impl CodingSession {
    fn evidence_version(mtime: i64, bytes: u64) -> String {
        format!("mtime={mtime};size={bytes}")
    }

    fn evidence_hash(text: &str) -> String {
        let mut h = Sha256::new();
        h.update(text.as_bytes());
        format!("{:x}", h.finalize())
    }

    async fn persist_file_evidence(&self, path: &str, text: &str, mtime: i64, bytes: u64) -> Uuid {
        let content: String = text.chars().take(MAX_EVIDENCE_BYTES).collect();
        let version = Self::evidence_version(mtime, bytes);
        let hash = Self::evidence_hash(&content);
        let entry = EvidenceEntry {
            id: Uuid::new_v4(),
            workspace_id: self.workspace_id,
            target_key: self.target_key(),
            kind: "file_snapshot".into(),
            query_hash: hash.clone(),
            path_or_url: path.into(),
            version_token: version,
            content_hash: hash,
            payload_json: serde_json::json!({"path": path}).to_string(),
            summary: format!("文件快照：{path}"),
            content,
            expires_at: None,
        };
        let id = entry.id;
        let _ = self.evidence_repo.upsert(&entry);
        id
    }

    fn target_key(&self) -> String {
        match &self.target {
            CodingTarget::Local => "local".into(),
            CodingTarget::Remote { connection_id, .. } => format!("ssh:{connection_id}"),
            CodingTarget::Agent { connection_id, .. } => format!("agent:{connection_id}"),
        }
    }

    /// Keeps the context within a predictable memory/token budget. When
    /// dropping history, prefers deleting a whole turn at a "user message"
    /// boundary (one assistant tool_calls round and its immediately
    /// following tool results are always kept together, never leaving an
    /// orphaned tool message an OpenAI-compatible API would reject; the
    /// leading system prompt and project conventions are never dropped).
    ///
    /// When no complete turn can be dropped (the current, still-in-progress
    /// turn is itself over budget -- dozens of tool calls each reading a
    /// non-trivial file/search result can blow the budget on their own,
    /// and there's no "next user message" to use as an `end` boundary),
    /// falls back to trimming the oldest "assistant tool call + its tool
    /// result" group from *within* the current turn
    /// (`trim_oldest_exchange_in_current_round`), keeping only the current
    /// turn's newest group untouched -- the model can still see the most
    /// recent tool call's result, older ones get replaced with a summary.
    async fn limit_context(&mut self, client: &reqwest::Client, provider: &AiProvider, api_key: &Option<String>) {
        self.inject_evidence_recall();
        let budget = agent_llm::context_budget(provider);
        let mut user_turns = self
            .messages
            .iter()
            .filter(|message| message["role"].as_str() == Some("user"))
            .count();
        loop {
            let size_tokens = agent_llm::estimate_tokens(
                self.messages
                    .iter()
                    .map(|message| message.to_string().len())
                    .sum::<usize>(),
            );
            if size_tokens <= budget && user_turns <= MAX_CONTEXT_USER_TURNS {
                break;
            }
            let Some(start) = self
                .messages
                .iter()
                .position(|message| message["role"].as_str() == Some("user"))
            else {
                break;
            };
            let end = self
                .messages
                .iter()
                .enumerate()
                .skip(start + 1)
                .find_map(|(index, message)| {
                    (message["role"].as_str() == Some("user")).then_some(index)
                });
            match end {
                Some(end) => {
                    let dropped: Vec<serde_json::Value> = self.messages.drain(start..end).collect();
                    user_turns -= 1;
                    let summary = match summarize_dropped_turns(&dropped, client, provider, api_key).await {
                        Some(summary) => summary,
                        None => deterministic_fallback_summary(&dropped),
                    };
                    self.append_context_summary(&summary);
                }
                None => {
                    if !self
                        .trim_oldest_exchange_in_current_round(start, client, provider, api_key)
                        .await
                    {
                        // No more "older, safe to drop" tool exchanges left
                        // in the current turn (only the newest group
                        // remains, can't touch it) -- nothing more to do,
                        // give up trimming further to avoid an infinite
                        // loop; the request may still be over budget, but
                        // it's already been compressed as much as possible.
                        break;
                    }
                }
            }
        }
    }

    /// Within the current turn (`round_start` is that turn's user message's
    /// index), finds the oldest "assistant tool call + its immediately
    /// following tool result" group and drops it entirely, replacing it
    /// with a summary -- the fallback `limit_context` reaches for when no
    /// whole turn can be dropped (see its doc comment above). Deliberately
    /// leaves the current turn's **newest** exchange untouched: the model
    /// must still be able to see the just-happened tool call's result, a
    /// compaction must not also erase the information currently in use.
    /// When only one exchange remains (the newest one), there's nothing
    /// older to drop, returns `false` to tell the caller "nothing further
    /// can be compressed here".
    async fn trim_oldest_exchange_in_current_round(
        &mut self,
        round_start: usize,
        client: &reqwest::Client,
        provider: &AiProvider,
        api_key: &Option<String>,
    ) -> bool {
        let mut exchanges: Vec<(usize, usize)> = Vec::new();
        let mut i = round_start + 1;
        while i < self.messages.len() {
            if self.messages[i]["role"].as_str() == Some("assistant") {
                let unit_start = i;
                let mut j = i + 1;
                while j < self.messages.len() && self.messages[j]["role"].as_str() == Some("tool")
                {
                    j += 1;
                }
                exchanges.push((unit_start, j));
                i = j;
            } else {
                i += 1;
            }
        }
        if exchanges.len() <= 1 {
            return false;
        }
        let (unit_start, unit_end) = exchanges[0];
        let dropped: Vec<serde_json::Value> = self.messages.drain(unit_start..unit_end).collect();
        let summary = match summarize_dropped_turns(&dropped, client, provider, api_key).await {
            Some(summary) => summary,
            None => deterministic_fallback_summary(&dropped),
        };
        self.append_context_summary(&summary);
        true
    }

    fn inject_evidence_recall(&mut self) {
        let Some(query) = self.messages.iter().rev().find(|m| m["role"].as_str() == Some("user")).and_then(|m| m["content"].as_str()).map(str::trim).filter(|q| !q.is_empty()) else { return; };
        if self.evidence_recall_query.as_deref() == Some(query) { return; }
        self.evidence_recall_query = Some(query.to_string());
        let terms = query.split_whitespace().take(8).collect::<Vec<_>>().join(" OR ");
        let Ok(rows) = self.evidence_repo.search_fts(self.workspace_id, &self.target_key(), &terms, 8) else { return; };
        if rows.is_empty() { return; }
        let body = rows.into_iter().map(|(id, path, summary)| format!("- evidence_id={id} source={path} {summary}")).collect::<Vec<_>>().join("\n");
        self.messages.push(json!({"role":"system","content":format!("[历史证据召回，仅供定位，需用 read_evidence 获取正文]\n{body}")}));
    }

    /// Merges a new piece of summary text into `self.messages`'s dedicated
    /// summary message -- identifies "which message is the summary one" via
    /// the `CONTEXT_SUMMARY_PREFIX` prefix rather than a fixed index:
    /// project-memory messages (`AGENTS.md`/`CLAUDE.md`) are also inserted
    /// at the front as system messages, and their count varies by project,
    /// so the summary message must be inserted "after every leading system
    /// message, before the first non-system message" to not disturb that
    /// order. When the summary itself exceeds the character cap, trims from
    /// the earliest part, keeping the most recent content -- a newer
    /// summary is usually more relevant to the current topic than an older
    /// one.
    fn append_context_summary(&mut self, new_piece: &str) {
        if let Some(existing) = self.messages.iter_mut().find(|message| {
            message["role"].as_str() == Some("system")
                && message["content"]
                    .as_str()
                    .is_some_and(|c| c.starts_with(CONTEXT_SUMMARY_PREFIX))
        }) {
            let mut body = existing["content"]
                .as_str()
                .unwrap_or_default()
                .strip_prefix(CONTEXT_SUMMARY_PREFIX)
                .unwrap_or_default()
                .trim_start_matches('\n')
                .to_string();
            body.push('\n');
            body.push_str(new_piece);
            if body.chars().count() > MAX_CONTEXT_SUMMARY_CHARS {
                let skip = body.chars().count() - MAX_CONTEXT_SUMMARY_CHARS;
                body = format!(
                    "（更早的摘要已省略）\n{}",
                    body.chars().skip(skip).collect::<String>()
                );
            }
            *existing = json!({
                "role": "system",
                "content": format!("{CONTEXT_SUMMARY_PREFIX}\n{body}")
            });
        } else {
            let insert_at = self
                .messages
                .iter()
                .position(|message| message["role"].as_str() != Some("system"))
                .unwrap_or(self.messages.len());
            self.messages.insert(
                insert_at,
                json!({
                    "role": "system",
                    "content": format!("{CONTEXT_SUMMARY_PREFIX}\n{new_piece}")
                }),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Uuid,
        workspace_id: Uuid,
        workspace_root: String,
        target: CodingTarget,
        provider_id: Uuid,
        file_ops: Arc<dyn FileOps>,
        change_store: Arc<Mutex<ChangeStore>>,
        evidence_repo: Arc<AiEvidenceRepo>,
    ) -> Self {
        // Without explicitly telling the model "what system/shell you're
        // on", it falls back on its training-data default assumption
        // (almost always guessing Linux/bash regardless of the real
        // target) -- a wrong guess either fails the command outright or
        // produces a subtler syntax error under a remote/restricted shell,
        // costing the model several more tool-call rounds to notice. The
        // platform/shell mapping here matches the existing escaping
        // assumptions in `run_local_ai_command_output`/
        // `coding::local_exec::shell_quote` exactly, not a newly invented
        // mapping.
        let target_desc = match &target {
            CodingTarget::Local => "本地工作区，Windows，命令行执行环境是 PowerShell".to_string(),
            CodingTarget::Remote { host_label, .. } => {
                format!("远程主机 {host_label}，Linux，命令行执行环境是 bash")
            }
            CodingTarget::Agent { host_label, .. } => {
                format!("远程 Windows 主机 {host_label}，命令行执行环境是 cmd.exe")
            }
        };
        let system_prompt = format!(
            "你是集成在 roc_desk 桌面工具里的 AI 编程助手，当前绑定的工作区根目录是 `{workspace_root}`（{target_desc}）。\
             run_command 执行的命令必须匹配上面说明的 shell 语法，不要凭空假设是另一种系统/shell。\
             你可以用提供的工具读写文件、搜索代码、访问互联网、执行命令。涉及“今天/最新/新闻/外部事实”的问题必须先调用 web_search，\
             不要凭模型记忆回答。write_file/edit_file 产生的改动不会立即生效，\
             而是生成 Diff 交给用户确认，所以你可以放心连续提出多个改动，不需要等待每一步都被确认才能继续推理。\
             run_command 有安全限制：破坏性命令会被直接拦截，其余命令需要用户在弹窗里确认才会真正执行。\
             面对\"分析整个项目\"这类开放式大任务时，优先用 search_files/list_directory 快速定位\
             最相关的一小批文件（不需要每个文件都读一遍），读完这些就给出结论；不要为了追求\"看得更全\"\
             而无休止地继续搜索/读取，觉得信息已经够回答用户的问题时就直接总结，而不是再多看几个文件。\
             \n\n读文件内容优先用 read_file 工具，不要用 run_command 里 sed/cat/head 这类命令去手动\
             分段读——read_file 会自动按合理长度截断并在截断处提示，不需要你自己为了\"怕超长\"而每次\
             只读几十行、切成一大堆零碎调用（这样反而更浪费工具调用次数）；单次工具结果本身有长度保护，\
             可以放心一次性多读一些内容（比如几百行），不用过度保守。\
             \n\n用户的话如果有明显歧义、可能对应两种差别很大的意图（比如一句简短的\"继续\"\"报错了\"\
             \"确认\"，既可能是在接着上一个没答完的问题往下走，也可能是在描述一件跟上文完全无关的新情况），\
             不要凭猜测直接选一种理解就展开长篇回答——调用 question 工具，把你想到的几种理解列成\
             options 让用户选一下，等用户选完再按确定下来的理解继续，比自己猜错了、答非所问、用户还要\
             再纠正一轮更省事。只有在意图已经足够清楚、只是细节需要你自己判断的情况下，才不需要用这个\
             工具反复确认——不要把它用成什么都要问一遍的过度谨慎。\
             \n\n如果歧义的原因是你手头缺上下文——比如用户说\"按刚才的方案\"\"继续处理\"\"你说的那个\
             办法\"，但当前对话历史里根本找不到对应的具体内容（长对话被自动摘要压缩后会发生这种情况）——\
             不要立刻用 question 工具让用户把内容重新讲一遍。先主动用手头已有的工具自己找线索：用\
             git_status/git_diff/run_command 跑 git log -n 10 看最近改了什么，用 read_file/\
             list_directory/search_files 看用户提到的文件、函数现在是什么状态。多数时候当前代码库的\
             实际情况就足够你推断出大致是要做什么、直接继续把任务做完，不需要用户重新说一遍。只有这样\
             查过之后仍然拿不准关键决策（比如有两种同样合理但结果差异很大的做法）时，才用 question 工具\
             问一个具体、带着你已经查到的线索的问题（例如\"我看到 X 文件现在是 Y 状态，接下来是按 A 方式\
             改还是 B 方式改？\"），而不是空泛地让用户把整个方案重新讲一遍。\
             \n\n除了前面提到的这些，还有几个更专用的工具，适用时优先用它们而不是绕远路用 run_command 拼命令行：\
             查看仓库状态/改动用 git_status/git_diff（只读，不需要确认），确定要提交时用 git_commit（需要\
             明确给出要提交的路径列表）；同一个文件要改好几处时用 multi_edit 一次性提交，不要为了改一个文件\
             连续调好几次 edit_file；按函数名/类名找定义位置优先用 find_definition，比用 search_files 猜关键词\
             更精确（找不到再退回 search_files）；需要启动一个不会自己退出的进程（开发服务器、watch 进程）\
             时用 run_command_background，不要用 run_command——那会一直等它退出、把这一轮卡住，配合\
             read_background_output 查看输出、stop_background_process 结束它；遇到\"在一大堆文件里找到某个\
             具体结论\"这种会消耗大量探索性工具调用、但你自己只需要一个结论的子任务，可以用 task 委派给一个\
             独立上下文的子代理去做，避免探索过程占满你自己的上下文——但子任务之间没有共享的探索上下文，\
             委派时要把背景信息写全。",
        );
        Self {
            id,
            workspace_id,
            workspace_root,
            target,
            mode: CodingMode::Plan,
            provider_id,
            change_store,
            todos: Vec::new(),
            skills: Vec::new(),
            project_memory_loaded: Vec::new(),
            evidence_repo,
            evidence_recall_query: None,
            messages: vec![json!({ "role": "system", "content": system_prompt })],
            file_ops,
            current_turn_id: Uuid::new_v4(),
            awaiting_confirmation_turn: None,
            background_jobs: HashMap::new(),
        }
    }

    /// Called by the accept/reject commands once they've confirmed "this
    /// turn has no more Pending changes": a matching `turn_id` to the one
    /// currently stuck actually clears the flag and returns `true` (telling
    /// the caller it can trigger an auto-continue); a mismatch (e.g. this
    /// is a leftover from an earlier turn the user only got around to much
    /// later, by which point the session may already be running a newer
    /// turn) does nothing and returns `false` -- avoids a stale
    /// confirmation accidentally triggering an unrelated auto-continue.
    pub fn resolve_awaiting_confirmation(&mut self, turn_id: Uuid) -> bool {
        if self.awaiting_confirmation_turn == Some(turn_id) {
            self.awaiting_confirmation_turn = None;
            true
        } else {
            false
        }
    }

    /// Feeds `fetch_project_memory`'s result back into the system prompt.
    /// Does no I/O itself, purely synchronous, safe to call right after a
    /// `tokio::join!`. Split from the fetch half (rather than one method
    /// holding `&mut self` start to finish) because the caller's project-
    /// memory/skill-discovery/git-repo probes are all independent remote
    /// probes that can run concurrently via `tokio::join!` instead of
    /// sequentially (each one can take up to the SSH/SFTP handshake's own
    /// minutes-level timeout; running them sequentially pays up to three
    /// times that in the worst case).
    pub(crate) fn apply_project_memory(&mut self, memory: Vec<(String, String)>) {
        for (filename, content) in memory {
            self.messages.push(json!({
                "role": "system",
                "content": format!("以下是项目 {filename} 中记录的约定，请在完成任务时遵守：\n\n{content}")
            }));
            self.project_memory_loaded.push(filename);
        }
    }

    /// Same idea as `apply_project_memory`, feeds `skills::discover_skills`'s
    /// result back into the skill list + system prompt.
    pub(crate) fn apply_skills(&mut self, skills: Vec<SkillMeta>) {
        self.skills = skills;
        if self.skills.is_empty() {
            return;
        }
        let list = self
            .skills
            .iter()
            .map(|s| format!("- {}: {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n");
        self.messages.push(json!({
            "role": "system",
            "content": format!("当前工作区定义了以下技能，需要时用 skill 工具按名称加载正文：\n{list}")
        }));
    }

    /// Snapshot of the real conversation context sent to the AI -- used
    /// when persisting a session so resuming it can genuinely continue on
    /// top of the existing context, not just replay a record of file
    /// changes.
    pub fn messages_snapshot(&self) -> Vec<serde_json::Value> {
        self.messages.clone()
    }

    /// Wholesale-replaces the current (freshly-constructed, system-prompt-
    /// only) `messages` with a persisted conversation context, restoring
    /// the state a historical session was interrupted at.
    pub fn restore_messages(&mut self, messages: Vec<serde_json::Value>) {
        if !messages.is_empty() {
            self.messages = messages;
            // Not trimmed here -- `limit_context` is async (it may fire off
            // a summary request when over budget), and there's no
            // provider/api_key available at this point. The first new
            // message after resuming goes through `send_message`'s loop,
            // which calls `limit_context` on entry anyway, where the
            // trimming happens if needed.
        }
    }

    /// Takes (and clears) this session's pending-injection messages
    /// accumulated in the host's pending-injections map, appending them to
    /// `messages` in order. Keyed by `workspace_id` (same key space as the
    /// cancel-token map), not `self.id` -- the `inject_message` command
    /// also only ever gets a workspace_id, the frontend has no reason to
    /// know this session's internal id.
    async fn drain_pending_injections(
        &mut self,
        pending_injections: &Arc<StdMutex<HashMap<Uuid, Vec<PendingInjection>>>>,
        client: &reqwest::Client,
        provider: &AiProvider,
        api_key: &Option<String>,
        app_handle: &AppHandle,
    ) {
        let injected = {
            let mut map = pending_injections.lock().unwrap();
            map.remove(&self.workspace_id).unwrap_or_default()
        };
        for inj in injected {
            let content =
                build_user_message_content(&inj.text, &inj.attachments, client, provider, api_key, app_handle, self.id, "coding")
                    .await;
            self.messages.push(json!({ "role": "user", "content": content }));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn send_message(
        &mut self,
        user_text: &str,
        attachments: &[ChatAttachment],
        providers: &AiProviderManager,
        ssh_pool: &SshConnectionPool,
        agent_pool: &AgentConnectionPool,
        audit: &AuditLogRepo,
        confirms: &CommandConfirmRegistry,
        permission_rules: &PermissionRulesRepo,
        question_confirms: &QuestionRegistry,
        mcp_manager: &McpServerManager,
        app_handle: &AppHandle,
        cancel_token: &tokio_util::sync::CancellationToken,
        pending_injections: &Arc<StdMutex<HashMap<Uuid, Vec<PendingInjection>>>>,
        symbol_indexes: &Arc<RwLock<HashMap<Uuid, SymbolIndex>>>,
    ) -> Result<String, AppError> {
        let provider = providers.get(self.provider_id)?.ok_or_else(|| {
            AppError::NotFound(format!("ai provider not found: {}", self.provider_id))
        })?;
        let api_key = providers.resolve_api_key(&provider).await?;
        let client = reqwest::Client::new();

        self.drain_pending_injections(pending_injections, &client, &provider, &api_key, app_handle)
            .await;
        self.current_turn_id = Uuid::new_v4();

        let content =
            build_user_message_content(user_text, attachments, &client, &provider, &api_key, app_handle, self.id, "coding")
                .await;
        self.messages.push(json!({ "role": "user", "content": content }));

        // MCP servers aren't connected until Build mode needs them -- a
        // single server failing to connect (child process won't start/HTTP
        // handshake fails) doesn't affect the others or the built-in tools,
        // just silently skipped. `mcp_tool_index` maps the qualified name
        // exposed to the model directly back to `(server_id, original tool
        // name)`, so dispatch doesn't need to reverse-parse the
        // `mcp__<server>__<tool>` naming scheme.
        let mut mcp_tool_defs: Vec<serde_json::Value> = Vec::new();
        let mut mcp_tool_index: HashMap<String, (Uuid, String)> = HashMap::new();
        if self.mode == CodingMode::Build {
            if let Ok(servers) = mcp_manager.list_enabled() {
                for server in servers {
                    let Ok(client) = mcp_manager.get_or_connect(server.id).await else {
                        continue;
                    };
                    for tool in &client.tools {
                        let qualified_name = format!(
                            "mcp__{}__{}",
                            sanitize_tool_name(&server.name),
                            sanitize_tool_name(&tool.name)
                        );
                        mcp_tool_defs.push(json!({
                            "type": "function",
                            "function": {
                                "name": qualified_name,
                                "description": format!("[MCP:{}] {}", server.name, tool.description),
                                "parameters": tool.input_schema,
                            }
                        }));
                        mcp_tool_index.insert(qualified_name, (server.id, tool.name.clone()));
                    }
                }
            }
        }

        let _ = app_handle.emit(
            "coding:assistant-note",
            json!({
                "sessionId": self.id,
                "text": "我先理解任务并定位相关代码，接下来的检查和执行步骤会实时显示在这里。",
                "kind": "status"
            }),
        );

        let mut turn_usage = agent_llm::TurnUsage::default();
        let session_id = self.id;

        // No hard cap on tool-loop iterations -- the loop only ends when
        // the model stops calling tools on its own (the `tool_calls.is_empty()`
        // branch below returns), or the user clicks "stop" (`cancel_token`
        // cancelled, returns `Err` immediately below). A model stuck in a
        // genuine non-converging loop has no automatic backstop; only a
        // manual stop ends it.
        loop {
            if cancel_token.is_cancelled() {
                return Err(AppError::Internal(
                    "已停止：用户取消了当前对话轮次".to_string(),
                ));
            }
            self.drain_pending_injections(pending_injections, &client, &provider, &api_key, app_handle)
                .await;
            self.limit_context(&client, &provider, &api_key).await;

            let mut tools = tools_for_mode(self.mode);
            if let Some(arr) = tools.as_array_mut() {
                arr.extend(mcp_tool_defs.iter().cloned());
            }

            let round = match agent_llm::call_llm_once(
                &client,
                &provider,
                &api_key,
                &self.messages,
                Some(&tools),
                app_handle,
                session_id,
                "coding",
                cancel_token,
            )
            .await
            {
                Ok(round) => round,
                Err(agent_llm::LlmCallError::Cancelled) => {
                    return Err(AppError::Internal(
                        "已停止：用户取消了当前对话轮次".to_string(),
                    ));
                }
                Err(agent_llm::LlmCallError::RequestFailed(detail)) => {
                    self.messages.push(agent_llm::request_failed_message(&detail));
                    return Err(AppError::Connection(detail));
                }
                Err(agent_llm::LlmCallError::ParseFailed(e)) => {
                    self.messages.push(agent_llm::parse_failed_message(&e));
                    return Err(AppError::Internal(format!("解析响应失败: {e}")));
                }
            };
            turn_usage.add(round.usage);
            let message = round.message;
            let tool_calls = round.tool_calls;

            if tool_calls.is_empty() {
                let text = message["content"].as_str().unwrap_or("").to_string();
                self.messages
                    .push(json!({ "role": "assistant", "content": text }));
                turn_usage.emit_summary(app_handle, session_id, "coding");
                let has_pending_this_turn = {
                    let store = self.change_store.lock().await;
                    store.changes().iter().any(|c| {
                        c.turn_id == self.current_turn_id && c.status == ChangeStatus::Pending
                    })
                };
                self.awaiting_confirmation_turn =
                    has_pending_this_turn.then_some(self.current_turn_id);
                return Ok(text);
            }

            if let Some(note) = message["content"].as_str() {
                if !note.trim().is_empty() {
                    let _ = app_handle.emit(
                        "coding:assistant-note",
                        json!({ "sessionId": self.id, "text": note, "kind": "model" }),
                    );
                }
            }

            if message["content"]
                .as_str()
                .is_none_or(|text| text.trim().is_empty())
            {
                if let Some(call) = tool_calls.first() {
                    let fn_name = call["function"]["name"].as_str().unwrap_or_default();
                    let fn_args = call["function"]["arguments"].as_str().unwrap_or("{}");
                    let text = tool_progress_text(fn_name, fn_args, tool_calls.len());
                    let _ = app_handle.emit(
                        "coding:assistant-note",
                        json!({ "sessionId": self.id, "text": text, "kind": "status" }),
                    );
                }
            }

            self.messages.push(message);
            for call in &tool_calls {
                let call_id = call["id"].as_str().unwrap_or_default().to_string();
                let fn_name = call["function"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let fn_args = call["function"]["arguments"].as_str().unwrap_or("{}");
                let detail = tool_call_detail(&fn_name, fn_args);

                let _ = app_handle.emit(
                    "coding:tool-call-start",
                    json!({ "sessionId": self.id, "tool": fn_name, "detail": detail }),
                );

                let call_result = if let Some((server_id, tool_name)) = mcp_tool_index.get(&fn_name)
                {
                    let arguments: serde_json::Value =
                        serde_json::from_str(fn_args).unwrap_or_else(|_| json!({}));
                    Ok(ToolCall::Mcp {
                        server_id: *server_id,
                        tool_name: tool_name.clone(),
                        arguments,
                    })
                } else {
                    tools::parse_tool_call(&fn_name, fn_args)
                };
                let permission_engine = PermissionEngine::load(permission_rules)?;
                let result_text = agent_llm::cap_tool_result(match call_result {
                    Ok(call) => self
                        .execute_tool(
                            call,
                            ssh_pool,
                            agent_pool,
                            audit,
                            confirms,
                            &permission_engine,
                            question_confirms,
                            mcp_manager,
                            &client,
                            &provider,
                            &api_key,
                            symbol_indexes,
                            cancel_token,
                            app_handle,
                        )
                        .await
                        .unwrap_or_else(|e| format!("工具执行出错：{e}")),
                    Err(e) => format!("工具调用参数解析失败：{e}"),
                });

                let _ = app_handle.emit(
                    "coding:tool-call-end",
                    json!({ "sessionId": self.id, "tool": fn_name, "output": result_text }),
                );

                self.messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": result_text,
                }));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_tool(
        &mut self,
        call: ToolCall,
        ssh_pool: &SshConnectionPool,
        agent_pool: &AgentConnectionPool,
        audit: &AuditLogRepo,
        confirms: &CommandConfirmRegistry,
        permission_engine: &PermissionEngine,
        question_confirms: &QuestionRegistry,
        mcp_manager: &McpServerManager,
        // Only used by the `ToolCall::Task` branch (a sub-agent fires off
        // its own LLM requests) -- still part of this one shared signature
        // rather than a separate path because `run_subagent_task` needs to
        // recursively call back into this very `execute_tool` to run its
        // own sub-agent tool calls; sharing one signature is simpler.
        client: &reqwest::Client,
        provider: &AiProvider,
        api_key: &Option<String>,
        symbol_indexes: &Arc<RwLock<HashMap<Uuid, SymbolIndex>>>,
        cancel_token: &tokio_util::sync::CancellationToken,
        app_handle: &AppHandle,
    ) -> Result<String, AppError> {
        match call {
            ToolCall::ReadFile { path } => {
                if let Some(content) = self.change_store.lock().await.pending_content_for(&path) {
                    return Ok(agent_llm::cap_tool_result(content));
                }
                if let Ok(size) = self.file_ops.file_size(&path).await {
                    if let Ok(Some(entry)) = self.evidence_repo.get_latest_path(
                        self.workspace_id, &self.target_key(), &path, size,
                    ) {
                        return Ok(agent_llm::cap_tool_result(entry.content));
                    }
                }
                let content = self.file_ops.read_file_for_editor(&path).await?;
                let evidence_id = self.persist_file_evidence(&path, &content.text, content.mtime, content.total_size).await;
                Ok(format!("{}\n[evidence_id={evidence_id}]", agent_llm::cap_tool_result(content.text)))
            }
            ToolCall::ReadEvidence { id, start_line, end_line } => {
                let entry = self.evidence_repo.get_by_id(id)?.ok_or_else(|| AppError::NotFound("证据不存在或已清理".into()))?;
                let lines: Vec<&str> = entry.content.lines().collect();
                let start = start_line.unwrap_or(1).max(1);
                let end = end_line.unwrap_or(lines.len()).min(lines.len().max(start));
                let body = if lines.is_empty() { String::new() } else { lines[(start - 1).min(lines.len())..end].join("\n") };
                Ok(format!("[evidence_id={id} source={} version={}]\n{}", entry.path_or_url, entry.version_token, agent_llm::cap_tool_result(body)))
            }
            ToolCall::ListDirectory { path } => {
                let entries = self.file_ops.list_dir(&path).await?;
                Ok(agent_llm::cap_tool_result(
                    serde_json::to_string(&entries).unwrap_or_default(),
                ))
            }
            ToolCall::SearchFiles { pattern, path } => {
                self.search_files(&pattern, &path, ssh_pool, agent_pool)
                    .await
            }
            ToolCall::WebSearch { query } => {
                search_web_results(&reqwest::Client::new(), &query).await
            }
            ToolCall::WriteFile { path, content } => {
                self.stage_change(&path, content, app_handle)
                    .await
            }
            ToolCall::EditFile {
                path,
                old_text,
                new_text,
            } => {
                let original = match self.change_store.lock().await.pending_content_for(&path) {
                    Some(c) => c,
                    None => self.file_ops.read_file(&path).await?.text,
                };
                let updated = tools::apply_text_edit(&original, &old_text, &new_text).ok_or_else(|| {
                    AppError::Internal(format!(
                        "edit_file 失败：在 {path} 中没有找到匹配的 old_text，请先用 read_file 确认现有内容"
                    ))
                })?;
                self.stage_change(&path, updated, app_handle)
                    .await
            }
            ToolCall::RunCommand { command } => {
                self.run_command_gated(
                    &command,
                    ssh_pool,
                    agent_pool,
                    audit,
                    confirms,
                    permission_engine,
                    app_handle,
                )
                .await
            }
            ToolCall::Glob { pattern, path } => self.search_glob(&pattern, &path).await,
            ToolCall::WebFetch { url } => {
                self.webfetch_gated(&url, permission_engine, app_handle)
                    .await
            }
            ToolCall::TodoWrite { todos } => {
                self.todos = todos;
                let _ = app_handle.emit(
                    "coding:todo-update",
                    json!({ "sessionId": self.id, "todos": &self.todos }),
                );
                Ok(format!("任务清单已更新，共 {} 项", self.todos.len()))
            }
            ToolCall::Question { question, options } => {
                self.ask_user(&question, options, question_confirms, app_handle)
                    .await
            }
            ToolCall::Skill { name } => {
                skills::load_skill_body(self.file_ops.as_ref(), &self.skills, &name).await
            }
            ToolCall::MultiEdit { path, edits } => {
                let mut content = match self.change_store.lock().await.pending_content_for(&path) {
                    Some(c) => c,
                    None => self.file_ops.read_file(&path).await?.text,
                };
                for (i, edit) in edits.iter().enumerate() {
                    content = match tools::apply_text_edit(&content, &edit.old_text, &edit.new_text) {
                        Some(updated) => updated,
                        None => {
                            return Err(AppError::Internal(format!(
                                "multi_edit 失败：第 {} 处替换的 old_text 在 {path} 中没有找到匹配（前面 {} 处\
                                 已经在内存里预演成功，但这次调用整体不会生效，不会留下部分修改），请先用 \
                                 read_file 确认最新内容后重试",
                                i + 1,
                                i
                            )))
                        }
                    };
                }
                self.stage_change(&path, content, app_handle)
                    .await
            }
            ToolCall::GitStatus { path } => Ok(agent_llm::cap_tool_result(
                git_ops::status(&self.target, &self.workspace_root, path.as_deref(), ssh_pool, agent_pool)
                    .await?,
            )),
            ToolCall::GitDiff { path } => Ok(agent_llm::cap_tool_result(
                git_ops::diff(&self.target, &self.workspace_root, path.as_deref(), ssh_pool, agent_pool)
                    .await?,
            )),
            ToolCall::GitCommit { message, paths } => {
                if paths.is_empty() {
                    return Err(AppError::Internal(
                        "git_commit 失败：paths 不能为空，请明确给出要提交的文件/目录路径（可以先用 \
                         git_status 确认）"
                            .to_string(),
                    ));
                }
                let (auto_allow_readonly, full_auto) = {
                    let store = self.change_store.lock().await;
                    (
                        store.auto_allow_readonly.load(std::sync::atomic::Ordering::Relaxed),
                        store.full_auto.load(std::sync::atomic::Ordering::Relaxed),
                    )
                };
                let synthetic_command = format!("git commit -m {message:?} -- {}", paths.join(" "));
                match gate_command(
                    self.id,
                    &self.target,
                    auto_allow_readonly,
                    full_auto,
                    &synthetic_command,
                    audit,
                    confirms,
                    permission_engine,
                    app_handle,
                )
                .await
                {
                    GateOutcome::Blocked(msg) => Ok(msg),
                    GateOutcome::Allowed => Ok(agent_llm::cap_tool_result(
                        git_ops::commit_paths(
                            &self.target,
                            &self.workspace_root,
                            &paths,
                            &message,
                            ssh_pool,
                            agent_pool,
                        )
                        .await?,
                    )),
                }
            }
            ToolCall::RunCommandBackground { command } => {
                self.run_command_background_gated(&command, audit, confirms, permission_engine, app_handle)
                    .await
            }
            ToolCall::ReadBackgroundOutput { job_id } => self.read_background_output(job_id),
            ToolCall::StopBackgroundProcess { job_id } => self.stop_background_process(job_id),
            ToolCall::FindDefinition { symbol } => self.find_definition(&symbol, symbol_indexes).await,
            ToolCall::Task { description, prompt } => {
                Box::pin(self.run_subagent_task(
                    &description,
                    &prompt,
                    client,
                    provider,
                    api_key,
                    ssh_pool,
                    agent_pool,
                    audit,
                    confirms,
                    permission_engine,
                    question_confirms,
                    mcp_manager,
                    symbol_indexes,
                    cancel_token,
                    app_handle,
                ))
                .await
            }
            ToolCall::Mcp {
                server_id,
                tool_name,
                arguments,
            } => {
                let full_auto = self
                    .change_store
                    .lock()
                    .await
                    .full_auto
                    .load(std::sync::atomic::Ordering::Relaxed);
                self.call_mcp_tool(
                    full_auto,
                    server_id,
                    &tool_name,
                    arguments,
                    permission_engine,
                    confirms,
                    mcp_manager,
                    app_handle,
                )
                .await
            }
        }
    }

    /// `glob` tool: matches filenames/paths only, never reads content --
    /// directly reuses `fsops::search_stream` (`SearchMode::FileName`),
    /// naturally unified across local/remote workspaces.
    async fn search_glob(&self, pattern: &str, path: &str) -> Result<String, AppError> {
        let mut matches = Vec::new();
        let options = SearchOptions {
            case_sensitive: false,
            whole_word: false,
            use_regex: false,
        };
        search_stream(
            self.file_ops.as_ref(),
            path,
            pattern,
            &options,
            SearchMode::FileName,
            |result| matches.push(result.path),
            || false,
        )
        .await?;
        Ok(if matches.is_empty() {
            "没有匹配结果".to_string()
        } else {
            matches.join("\n")
        })
    }

    /// `webfetch` tool: default policy is "allow" (same as the already-
    /// unrestricted `web_search`, SSRF protection lives in
    /// `coding::webfetch::fetch_url`), but the user can add a `deny` rule
    /// for `webfetch` in the permission rules (e.g. a corporate network
    /// policy against the AI reaching external pages), which blocks it
    /// without ever sending the request.
    async fn webfetch_gated(
        &self,
        url: &str,
        permission_engine: &PermissionEngine,
        _app_handle: &AppHandle,
    ) -> Result<String, AppError> {
        if permission_engine.decide("webfetch", url) == Some(Decision::Deny) {
            return Ok(format!("已按权限规则拒绝访问：{url}"));
        }
        webfetch::fetch_url(&reqwest::Client::new(), url).await
    }

    /// `question` tool: the same "block, wait for a frontend response"
    /// pattern as the command-confirm dialog, just waiting on a text answer
    /// instead of a bool.
    async fn ask_user(
        &self,
        question: &str,
        options: Vec<String>,
        question_confirms: &QuestionRegistry,
        app_handle: &AppHandle,
    ) -> Result<String, AppError> {
        let (request_id, rx) = question_confirms.register().await;
        let _ = app_handle.emit(
            "coding:question-request",
            json!({ "sessionId": self.id, "requestId": request_id, "question": question, "options": options }),
        );
        let answer = rx.await.unwrap_or_default();
        Ok(answer)
    }

    /// MCP tool call gate: checks for a configured `mcp:<server>:<tool>`
    /// rule first, falling back to "always confirm" if none -- same default
    /// policy as `run_command` missing a whitelist hit, reusing the same
    /// `CommandConfirmRegistry` (semantically both are "allow this one
    /// side-effecting action to run once"; the frontend's dialog copy
    /// differs by the `kind` field, no need for a second confirmation flow
    /// on the backend).
    #[allow(clippy::too_many_arguments)]
    async fn call_mcp_tool(
        &self,
        full_auto: bool,
        server_id: Uuid,
        tool_name: &str,
        arguments: serde_json::Value,
        permission_engine: &PermissionEngine,
        confirms: &CommandConfirmRegistry,
        mcp_manager: &McpServerManager,
        app_handle: &AppHandle,
    ) -> Result<String, AppError> {
        let servers = mcp_manager.list()?;
        let server = servers
            .iter()
            .find(|s| s.id == server_id)
            .ok_or_else(|| AppError::NotFound("MCP 服务器不存在".into()))?;
        let match_key = format!("{}:{}", server.name, tool_name);

        let allowed = match permission_engine.decide("mcp", &match_key) {
            Some(Decision::Allow) => true,
            Some(Decision::Deny) => false,
            _ if full_auto => true,
            _ => {
                let (request_id, rx) = confirms.register(self.id).await;
                let _ = app_handle.emit(
                    "coding:command-confirm-request",
                    json!({
                        "sessionId": self.id,
                        "requestId": request_id,
                        "command": format!("{}.{}({})", server.name, tool_name, arguments),
                        "host": null,
                        "kind": "mcp",
                        "matchKey": match_key,
                    }),
                );
                rx.await.unwrap_or(false)
            }
        };
        if !allowed {
            return Ok(format!(
                "用户拒绝执行 MCP 工具调用：{}.{}",
                server.name, tool_name
            ));
        }

        let client = mcp_manager.get_or_connect(server_id).await?;
        client.call_tool(tool_name, arguments).await
    }

    async fn search_files(
        &self,
        pattern: &str,
        path: &str,
        ssh_pool: &SshConnectionPool,
        agent_pool: &AgentConnectionPool,
    ) -> Result<String, AppError> {
        let mut h = Sha256::new();
        h.update(pattern.as_bytes()); h.update([0]); h.update(path.as_bytes());
        let query_hash = format!("{:x}", h.finalize());
        if let Some(entry) = self.evidence_repo.get_exact(self.workspace_id, &self.target_key(), "content_search", &query_hash, path, "search-v1")? {
            return Ok(format!("{}\n[evidence_id={} cache_hit=true]", entry.content, entry.id));
        }
        let result = self.search_files_uncached(pattern, path, ssh_pool, agent_pool).await?;
        let content: String = result.chars().take(MAX_EVIDENCE_BYTES).collect();
        let id = Uuid::new_v4();
        let mut ch = Sha256::new(); ch.update(content.as_bytes());
        let hash = format!("{:x}", ch.finalize());
        let entry = EvidenceEntry { id, workspace_id: self.workspace_id, target_key: self.target_key(), kind: "content_search".into(), query_hash, path_or_url: path.into(), version_token: "search-v1".into(), content_hash: hash, payload_json: serde_json::json!({"pattern": pattern, "path": path}).to_string(), summary: format!("搜索 {pattern} in {path}"), content, expires_at: None };
        let _ = self.evidence_repo.upsert(&entry);
        Ok(format!("{}\n[evidence_id={} cache_hit=false]", result, id))
    }

    async fn search_files_uncached(
        &self,
        pattern: &str,
        path: &str,
        ssh_pool: &SshConnectionPool,
        agent_pool: &AgentConnectionPool,
    ) -> Result<String, AppError> {
        match &self.target {
            CodingTarget::Local => {
                let results = tools::search_files_local(std::path::Path::new(path), pattern, 50);
                Ok(if results.is_empty() {
                    "没有匹配结果".to_string()
                } else {
                    results.join("\n")
                })
            }
            CodingTarget::Remote { connection_id, .. } => {
                let session = ssh_pool.get_or_connect(*connection_id).await?;
                let quoted_pattern = super::local_exec::shell_quote(pattern);
                let quoted_path = super::local_exec::shell_quote(path);
                let cmd = format!(
                    "rg -n -F -- {quoted_pattern} {quoted_path} 2>/dev/null | head -n 200 || grep -rn -F -- {quoted_pattern} {quoted_path} 2>/dev/null | head -n 200"
                );
                session.exec(&cmd).await
            }
            // Agent-side search runs on the remote host itself
            // (`roc_desk_agent`'s own filesystem walk, not a per-file
            // network round trip).
            CodingTarget::Agent { connection_id, .. } => {
                let session = agent_pool.get_or_connect(*connection_id).await?;
                let options = roc_desk_protocol::SearchOptions {
                    case_sensitive: false,
                    whole_word: false,
                    use_regex: false,
                };
                let request = roc_desk_protocol::Request::SearchContent {
                    root: path.to_string(),
                    query: pattern.to_string(),
                    options,
                };
                match session.request(request).await? {
                    roc_desk_protocol::Response::Ok(
                        roc_desk_protocol::ResponseBody::SearchResults(results),
                    ) => {
                        if results.is_empty() {
                            return Ok("没有匹配结果".to_string());
                        }
                        let mut lines = Vec::new();
                        'outer: for file in results {
                            for m in file.matches {
                                if lines.len() >= 200 {
                                    break 'outer;
                                }
                                lines.push(format!(
                                    "{}:{}:{}",
                                    file.path,
                                    m.line_number,
                                    m.line_text.trim()
                                ));
                            }
                        }
                        Ok(lines.join("\n"))
                    }
                    roc_desk_protocol::Response::Error { message, .. } => {
                        Err(AppError::Internal(message))
                    }
                    _ => Err(AppError::Internal("Agent 返回了意外的响应类型".into())),
                }
            }
        }
    }

    pub(crate) async fn stage_change(
        &mut self,
        path: &str,
        new_content: String,
        app_handle: &AppHandle,
    ) -> Result<String, AppError> {
        let turn_id = self.current_turn_id;
        let (change, sync, commit) = self
            .change_store
            .lock()
            .await
            .stage(path, new_content, turn_id)
            .await?;
        let id = change.id;
        let applied = sync.is_some();
        let _ = self.evidence_repo.invalidate_path(self.workspace_id, &self.target_key(), path);
        let _ = app_handle.emit(
            "coding:file-change",
            json!({ "sessionId": self.id, "change": &change, "sync": sync }),
        );
        if let Some(commit) = commit {
            let _ = app_handle.emit(
                "coding:git-commit-result",
                json!({ "sessionId": self.id, "path": commit.path, "output": commit.output }),
            );
        }
        Ok(if applied {
            format!("已为 {path} 生成变更（id={id}），已直接写入磁盘，用户可在界面上点\"撤销\"。")
        } else {
            format!("已为 {path} 生成变更（id={id}），已在界面展示 Diff，等待用户 Accept 后才会真正写入磁盘。")
        })
    }

    /// Real logic lives in the free function `run_command_gated_shared`
    /// below; this just forwards the handful of `&self` fields it needs.
    pub(crate) async fn run_command_gated(
        &mut self,
        command: &str,
        ssh_pool: &SshConnectionPool,
        agent_pool: &AgentConnectionPool,
        audit: &AuditLogRepo,
        confirms: &CommandConfirmRegistry,
        permission_engine: &PermissionEngine,
        app_handle: &AppHandle,
    ) -> Result<String, AppError> {
        let (auto_allow_readonly, full_auto) = {
            let store = self.change_store.lock().await;
            (
                store.auto_allow_readonly.load(std::sync::atomic::Ordering::Relaxed),
                store.full_auto.load(std::sync::atomic::Ordering::Relaxed),
            )
        };
        run_command_gated_shared(
            self.id,
            &self.target,
            &self.workspace_root,
            auto_allow_readonly,
            full_auto,
            command,
            ssh_pool,
            agent_pool,
            audit,
            confirms,
            permission_engine,
            app_handle,
        )
        .await
    }

    /// `run_command_background` tool -- `CodingTarget::Local` only: a
    /// "background process" on a Remote/Agent target would mean keeping
    /// some state alive on one SSH/Agent connection across multiple tool
    /// calls, but the existing connection pools are short-lived ("exec,
    /// return the connection") -- supporting this properly would need a
    /// whole separate remote-session state manager, not worth the
    /// complexity for now.
    pub(crate) async fn run_command_background_gated(
        &mut self,
        command: &str,
        audit: &AuditLogRepo,
        confirms: &CommandConfirmRegistry,
        permission_engine: &PermissionEngine,
        app_handle: &AppHandle,
    ) -> Result<String, AppError> {
        if !matches!(self.target, CodingTarget::Local) {
            return Ok(
                "后台执行目前只支持本地工作区。远程/Agent 目标请改用 run_command，在命令本身里用 \
                 shell 自带的后台手段（比如 Linux 下 `nohup ... &`，Windows 下 `Start-Process`）。"
                    .to_string(),
            );
        }
        let (auto_allow_readonly, full_auto) = {
            let store = self.change_store.lock().await;
            (
                store.auto_allow_readonly.load(std::sync::atomic::Ordering::Relaxed),
                store.full_auto.load(std::sync::atomic::Ordering::Relaxed),
            )
        };
        match gate_command(
            self.id,
            &self.target,
            auto_allow_readonly,
            full_auto,
            command,
            audit,
            confirms,
            permission_engine,
            app_handle,
        )
        .await
        {
            GateOutcome::Blocked(message) => return Ok(message),
            GateOutcome::Allowed => {}
        }

        #[cfg(target_os = "windows")]
        let mut cmd = super::local_exec::windows_command_for(command, true);
        #[cfg(not(target_os = "windows"))]
        let mut cmd = super::local_exec::unix_command_for(command);
        cmd.current_dir(&self.workspace_root);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // `kill_on_drop` deliberately not set here -- this `Child` is
        // stashed in `self.background_jobs` long-term; it gets killed when
        // the user calls `stop_background_process` or the whole session
        // ends (`Drop for CodingSession` handles that uniformly, see the
        // struct field doc comment), not when this `execute_tool` call
        // returns.
        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::Internal(format!("启动后台进程失败：{e}")))?;
        let pid = child.id();
        let output = Arc::new(StdMutex::new(String::new()));
        if let Some(stdout) = child.stdout.take() {
            let buf = output.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    append_background_output(&buf, &format!("{line}\n"));
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let buf = output.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    append_background_output(&buf, &format!("[stderr] {line}\n"));
                }
            });
        }
        let job_id = Uuid::new_v4();
        self.background_jobs.insert(
            job_id,
            BackgroundJob {
                command: command.to_string(),
                child,
                output,
            },
        );
        Ok(format!(
            "已在后台启动{}，job_id={job_id}。用 read_background_output 查看目前的输出，用 \
             stop_background_process 结束它。",
            pid.map(|p| format!("（pid={p}）")).unwrap_or_default()
        ))
    }

    /// `read_background_output` tool -- `try_wait` is a non-blocking poll,
    /// never waits for the process to exit.
    pub(crate) fn read_background_output(&mut self, job_id: Uuid) -> Result<String, AppError> {
        let job = self.background_jobs.get_mut(&job_id).ok_or_else(|| {
            AppError::Internal(format!(
                "找不到 job_id={job_id}，可能已经用 stop_background_process 结束并清理过了，或者 id 有误"
            ))
        })?;
        let status = match job.child.try_wait() {
            Ok(Some(status)) => format!("已结束，退出码 {:?}", status.code()),
            Ok(None) => "仍在运行".to_string(),
            Err(e) => format!("查询进程状态失败：{e}"),
        };
        let output = agent_llm::cap_tool_result(job.output.lock().unwrap().clone());
        let output = if output.trim().is_empty() {
            "（还没有任何输出）".to_string()
        } else {
            output
        };
        Ok(format!("命令：{}\n状态：{status}\n累计输出：\n{output}", job.command))
    }

    /// `stop_background_process` tool.
    pub(crate) fn stop_background_process(&mut self, job_id: Uuid) -> Result<String, AppError> {
        let mut job = self.background_jobs.remove(&job_id).ok_or_else(|| {
            AppError::Internal(format!("找不到 job_id={job_id}，可能已经结束并清理过了，或者 id 有误"))
        })?;
        let _ = job.child.start_kill();
        Ok(format!("已终止 job_id={job_id}（命令：{}）", job.command))
    }

    /// `find_definition` tool -- if the index hasn't been built yet for this
    /// session, scan the whole workspace on the spot and store the result
    /// back into `symbol_indexes` for later calls (within this session or
    /// elsewhere, e.g. the editor's "go to definition") to reuse without
    /// rescanning.
    pub(crate) async fn find_definition(
        &self,
        symbol: &str,
        symbol_indexes: &Arc<RwLock<HashMap<Uuid, SymbolIndex>>>,
    ) -> Result<String, AppError> {
        {
            let indexes = symbol_indexes.read().await;
            if let Some(index) = indexes.get(&self.workspace_id) {
                return Ok(format_symbol_lookup(symbol, index.lookup(symbol)));
            }
        }
        let index = build_index(self.file_ops.as_ref(), &self.workspace_root).await?;
        let result = format_symbol_lookup(symbol, index.lookup(symbol));
        symbol_indexes.write().await.insert(self.workspace_id, index);
        Ok(result)
    }

    /// `task` tool -- delegates a sub-task to a temporary sub-agent with its
    /// own independent message history, running a bounded tool loop and
    /// handing only the final summary text back to the main loop (the sub-
    /// agent's exploration never mixes into `self.messages`, which is the
    /// whole point: keep a large batch of exploratory tool calls from
    /// filling up the main conversation's context). Side effects (Diffs
    /// from writing files, commands run) are real and shared with the main
    /// loop via the same `self` -- the sub-agent isn't fully sandboxed, only
    /// its "conversation history" is a separate, disposable `messages`.
    #[allow(clippy::too_many_arguments)]
    async fn run_subagent_task(
        &mut self,
        description: &str,
        prompt: &str,
        client: &reqwest::Client,
        provider: &AiProvider,
        api_key: &Option<String>,
        ssh_pool: &SshConnectionPool,
        agent_pool: &AgentConnectionPool,
        audit: &AuditLogRepo,
        confirms: &CommandConfirmRegistry,
        permission_engine: &PermissionEngine,
        question_confirms: &QuestionRegistry,
        mcp_manager: &McpServerManager,
        symbol_indexes: &Arc<RwLock<HashMap<Uuid, SymbolIndex>>>,
        cancel_token: &tokio_util::sync::CancellationToken,
        app_handle: &AppHandle,
    ) -> Result<String, AppError> {
        const MAX_SUBAGENT_ITERATIONS: usize = 15;
        let mut messages = vec![
            json!({ "role": "system", "content": format!(
                "你是被主任务临时委派的子代理，工作区根目录是 `{}`。只需要完成下面这一个具体子任务，\
                 不需要和用户交互、不需要维护任务清单，完成后用一段简洁但信息完整的文字总结你做了什么、\
                 找到了什么、结论是什么——这段总结会被直接交回给委派你的主任务，它看不到你这边的过程\
                 细节，只看得到这段总结文字，务必把关键信息（具体文件路径、结论、必要的后续建议）写全，\
                 不要只说\"已完成\"。",
                self.workspace_root
            ) }),
            json!({ "role": "user", "content": prompt }),
        ];
        let _ = app_handle.emit(
            "coding:assistant-note",
            json!({ "sessionId": self.id, "text": format!("委派子任务：{description}"), "kind": "status" }),
        );
        let mut tools = tools_for_mode(self.mode);
        if let Some(arr) = tools.as_array_mut() {
            arr.retain(|t| {
                !matches!(
                    t["function"]["name"].as_str().unwrap_or(""),
                    "task" | "question" | "todo_write"
                )
            });
        }
        for _ in 0..MAX_SUBAGENT_ITERATIONS {
            if cancel_token.is_cancelled() {
                return Err(AppError::Internal("已停止：用户取消了当前对话轮次".to_string()));
            }
            let round = match agent_llm::call_llm_once(
                client,
                provider,
                api_key,
                &messages,
                Some(&tools),
                app_handle,
                self.id,
                "coding",
                cancel_token,
            )
            .await
            {
                Ok(r) => r,
                Err(agent_llm::LlmCallError::Cancelled) => {
                    return Err(AppError::Internal("已停止：用户取消了当前对话轮次".to_string()))
                }
                Err(agent_llm::LlmCallError::RequestFailed(detail)) => {
                    return Err(AppError::Connection(detail))
                }
                Err(agent_llm::LlmCallError::ParseFailed(e)) => {
                    return Err(AppError::Internal(format!("子任务解析响应失败: {e}")))
                }
            };
            let message = round.message;
            let tool_calls = round.tool_calls;
            if tool_calls.is_empty() {
                let text = message["content"].as_str().unwrap_or("").to_string();
                return Ok(format!("[子任务「{description}」完成]\n{text}"));
            }
            messages.push(message);
            for call in &tool_calls {
                let call_id = call["id"].as_str().unwrap_or_default().to_string();
                let fn_name = call["function"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let fn_args = call["function"]["arguments"].as_str().unwrap_or("{}");
                let call_result = tools::parse_tool_call(&fn_name, fn_args);
                let result_text = agent_llm::cap_tool_result(match call_result {
                    Ok(c) => Box::pin(self.execute_tool(
                        c,
                        ssh_pool,
                        agent_pool,
                        audit,
                        confirms,
                        permission_engine,
                        question_confirms,
                        mcp_manager,
                        client,
                        provider,
                        api_key,
                        symbol_indexes,
                        cancel_token,
                        app_handle,
                    ))
                    .await
                    .unwrap_or_else(|e| format!("工具执行出错：{e}")),
                    Err(e) => format!("工具调用参数解析失败：{e}"),
                });
                messages.push(json!({ "role": "tool", "tool_call_id": call_id, "content": result_text }));
            }
        }
        Ok(format!(
            "[子任务「{description}」在 {MAX_SUBAGENT_ITERATIONS} 轮内没有给出最终结论，已提前收尾；\
             如果任务确实很大，建议拆成更小的子任务分别委派]"
        ))
    }
}

fn format_symbol_lookup(symbol: &str, locations: Vec<SymbolLocation>) -> String {
    if locations.is_empty() {
        format!(
            "没有找到符号 `{symbol}` 的定义——索引是基于正则的轻量扫描，不理解语言语义，多行签名/\
             宏生成的定义/罕见写法可能漏掉；符号名如果没写错，改用 search_files 按关键词搜索。"
        )
    } else {
        let lines: Vec<String> = locations
            .iter()
            .map(|loc| format!("{}:{} ({})", loc.path, loc.line, loc.kind))
            .collect();
        format!("找到 {} 处候选定义：\n{}", locations.len(), lines.join("\n"))
    }
}

pub(crate) struct CommandExecutionResult {
    pub output: String,
    pub exit_code: Option<i32>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_command_gated_shared(
    session_id: Uuid,
    target: &CodingTarget,
    workspace_root: &str,
    auto_allow_readonly: bool,
    full_auto: bool,
    command: &str,
    ssh_pool: &SshConnectionPool,
    agent_pool: &AgentConnectionPool,
    audit: &AuditLogRepo,
    confirms: &CommandConfirmRegistry,
    permission_engine: &PermissionEngine,
    app_handle: &AppHandle,
) -> Result<String, AppError> {
    Ok(run_command_gated_shared_with_status(
        session_id,
        target,
        workspace_root,
        auto_allow_readonly,
        full_auto,
        command,
        ssh_pool,
        agent_pool,
        audit,
        confirms,
        permission_engine,
        app_handle,
    )
    .await?
    .output)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_command_gated_shared_with_status(
    session_id: Uuid,
    target: &CodingTarget,
    workspace_root: &str,
    auto_allow_readonly: bool,
    full_auto: bool,
    command: &str,
    ssh_pool: &SshConnectionPool,
    agent_pool: &AgentConnectionPool,
    audit: &AuditLogRepo,
    confirms: &CommandConfirmRegistry,
    permission_engine: &PermissionEngine,
    app_handle: &AppHandle,
) -> Result<CommandExecutionResult, AppError> {
    run_command_gated_shared_with_status_in_context(
        session_id,
        target,
        workspace_root,
        auto_allow_readonly,
        full_auto,
        workspace_root,
        &HashMap::new(),
        command,
        ssh_pool,
        agent_pool,
        audit,
        confirms,
        permission_engine,
        app_handle,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
/// `gate_command`'s verdict -- the text carried by `Blocked` is exactly the
/// tool result to hand back to the model (blacklist block/permission-rule
/// deny/user clicked reject each have different copy, the caller doesn't
/// need to know which one, just pass it through as-is).
pub(crate) enum GateOutcome {
    Blocked(String),
    Allowed,
}

/// Splits "can this command run" (blacklist -> permission rules ->
/// whitelist auto-allow/confirm dialog) out from "having decided, actually
/// run it" -- `run_command` (runs and waits for the result) and
/// `run_command_background` (starts it, doesn't wait) need the exact same
/// gating logic but different follow-up actions.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn gate_command(
    session_id: Uuid,
    target: &CodingTarget,
    auto_allow_readonly: bool,
    full_auto: bool,
    command: &str,
    audit: &AuditLogRepo,
    confirms: &CommandConfirmRegistry,
    permission_engine: &PermissionEngine,
    app_handle: &AppHandle,
) -> GateOutcome {
    let target_label = target_label_for(target);
    let is_windows_target = matches!(target, CodingTarget::Agent { .. });

    if guard::is_blacklisted(command, is_windows_target) {
        audit.record(session_id, &target_label, command, "blocked", None);
        let _ = app_handle.emit(
            "coding:command-blocked",
            json!({ "sessionId": session_id, "command": command }),
        );
        return GateOutcome::Blocked(format!(
            "已拦截高危命令：{command}，如需执行请前往终端模块手动操作"
        ));
    }

    match permission_engine.decide("run_command", command) {
        Some(Decision::Deny) => {
            audit.record(session_id, &target_label, command, "rejected", None);
            return GateOutcome::Blocked(format!("已按权限规则拒绝执行：{command}"));
        }
        Some(Decision::Allow) => {
            audit.record(session_id, &target_label, command, "auto-allow-rule", None);
            return GateOutcome::Allowed;
        }
        _ => {}
    }

    let allowed = if full_auto || (auto_allow_readonly && guard::is_whitelisted(command)) {
        true
    } else {
        let (request_id, rx) = confirms.register(session_id).await;
        let is_remote = matches!(
            target,
            CodingTarget::Remote { .. } | CodingTarget::Agent { .. }
        );
        let _ = app_handle.emit(
            "coding:command-confirm-request",
            json!({
                "sessionId": session_id,
                "requestId": request_id,
                "command": command,
                "host": if is_remote { Some(target_label.clone()) } else { None },
                "kind": "command",
            }),
        );
        rx.await.unwrap_or(false)
    };

    if !allowed {
        audit.record(session_id, &target_label, command, "rejected", None);
        return GateOutcome::Blocked(format!("用户拒绝执行命令：{command}"));
    }

    GateOutcome::Allowed
}

pub(crate) async fn run_command_gated_shared_with_status_in_context(
    session_id: Uuid,
    target: &CodingTarget,
    _workspace_root: &str,
    auto_allow_readonly: bool,
    full_auto: bool,
    cwd: &str,
    env: &HashMap<String, String>,
    command: &str,
    ssh_pool: &SshConnectionPool,
    agent_pool: &AgentConnectionPool,
    audit: &AuditLogRepo,
    confirms: &CommandConfirmRegistry,
    permission_engine: &PermissionEngine,
    app_handle: &AppHandle,
) -> Result<CommandExecutionResult, AppError> {
    match gate_command(
        session_id,
        target,
        auto_allow_readonly,
        full_auto,
        command,
        audit,
        confirms,
        permission_engine,
        app_handle,
    )
    .await
    {
        GateOutcome::Blocked(message) => {
            return Ok(CommandExecutionResult {
                output: message,
                exit_code: Some(126),
            });
        }
        GateOutcome::Allowed => {}
    }

    let target_label = target_label_for(target);
    let output = run_target_command(target, cwd, env, command, ssh_pool, agent_pool).await?;
    let summary: String = output.output.chars().take(2000).collect();
    audit.record(
        session_id,
        &target_label,
        command,
        "executed",
        Some(&summary),
    );
    Ok(CommandExecutionResult {
        output: output.output.chars().take(4000).collect(),
        exit_code: output.exit_code,
    })
}

async fn run_target_command(
    target: &CodingTarget,
    cwd: &str,
    env: &HashMap<String, String>,
    command: &str,
    ssh_pool: &SshConnectionPool,
    agent_pool: &AgentConnectionPool,
) -> Result<CommandExecutionResult, AppError> {
    match target {
        CodingTarget::Local => {
            let output = super::local_exec::run_local_ai_command_output(command, cwd, env).await?;
            Ok(CommandExecutionResult {
                output: String::from_utf8_lossy(&[output.stdout, output.stderr].concat())
                    .to_string(),
                exit_code: output.status.code(),
            })
        }
        CodingTarget::Remote { connection_id, .. } => {
            let session = ssh_pool.get_or_connect(*connection_id).await?;
            // Actively evicts this cached connection on `exec()` error (most
            // notably an `EXEC_TIMEOUT` timeout) -- `is_alive()` can't
            // detect a network-level silent disconnect, and without
            // eviction the next `get_or_connect` would hand out the same
            // dead connection again, paying the full ~120s timeout every
            // time.
            let output = match session.exec(command).await {
                Ok(output) => output,
                Err(e) => {
                    ssh_pool.evict(*connection_id).await;
                    return Err(e);
                }
            };
            Ok(CommandExecutionResult {
                output,
                exit_code: Some(0),
            })
        }
        CodingTarget::Agent { connection_id, .. } => {
            let session = agent_pool.get_or_connect(*connection_id).await?;
            Ok(CommandExecutionResult {
                output: session.exec(command, cwd).await?,
                exit_code: Some(0),
            })
        }
    }
}

pub(crate) fn target_label_for(target: &CodingTarget) -> String {
    match target {
        CodingTarget::Local => "本地".to_string(),
        CodingTarget::Remote { host_label, .. } => host_label.clone(),
        CodingTarget::Agent { host_label, .. } => host_label.clone(),
    }
}

/// I/O only, never touches `CodingSession` -- paired with
/// `CodingSession::apply_project_memory` so the caller can run this probe
/// concurrently with the git-repo probe/skill discovery via `tokio::join!`,
/// see `apply_project_memory`'s doc comment.
pub(crate) async fn fetch_project_memory(file_ops: &dyn FileOps) -> Vec<(String, String)> {
    const MAX_BYTES: usize = 32 * 1024;
    let mut result = Vec::new();
    for filename in ["AGENTS.md", "CLAUDE.md"] {
        let Ok(content) = file_ops.read_file(filename).await else {
            continue;
        };
        let truncated: String = content.text.chars().take(MAX_BYTES).collect();
        result.push((filename.to_string(), truncated));
    }
    result
}

/// Picks one field out of a tool call's arguments that best explains "what
/// is this actually operating on", for the frontend timeline to display.
/// Returns `None` on parse failure or no matching field -- not showing a
/// detail is more honest than showing "undefined".
fn tool_call_detail(fn_name: &str, fn_args: &str) -> Option<String> {
    let args: serde_json::Value = serde_json::from_str(fn_args).ok()?;
    match fn_name {
        "read_file" | "write_file" | "edit_file" | "list_directory" => {
            args["path"].as_str().map(str::to_string)
        }
        "web_search" => args["query"].as_str().map(str::to_string),
        "search_files" => {
            let pattern = args["pattern"].as_str().unwrap_or("?");
            let path = args["path"].as_str().unwrap_or("?");
            Some(format!("{pattern} in {path}"))
        }
        "run_command" | "run_command_background" => args["command"]
            .as_str()
            .map(|s| s.chars().take(80).collect()),
        "multi_edit" => args["path"].as_str().map(str::to_string),
        "git_status" | "git_diff" => Some(
            args["path"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| "整个仓库".to_string()),
        ),
        "git_commit" => args["message"].as_str().map(|s| s.chars().take(80).collect()),
        "read_background_output" | "stop_background_process" => {
            args["job_id"].as_str().map(str::to_string)
        }
        "find_definition" => args["symbol"].as_str().map(str::to_string),
        "task" => args["description"].as_str().map(str::to_string),
        _ => None,
    }
}

fn tool_progress_text(fn_name: &str, fn_args: &str, call_count: usize) -> String {
    let detail = tool_call_detail(fn_name, fn_args);
    let target = detail.as_deref().unwrap_or("相关内容");
    let action = match fn_name {
        "read_file" => format!("读取 `{target}`，确认当前实现"),
        "list_directory" => format!("查看 `{target}` 的目录结构"),
        "search_files" => format!("搜索 `{target}`，定位相关代码"),
        "web_search" => format!("访问互联网搜索 `{target}`"),
        "write_file" => format!("为 `{target}` 准备新文件变更"),
        "edit_file" => format!("为 `{target}` 准备代码修改"),
        "run_command" => format!("运行 `{target}`，检查实际结果"),
        "multi_edit" => format!("为 `{target}` 准备多处代码修改"),
        "git_status" => format!("查看 {target} 的 Git 状态"),
        "git_diff" => format!("查看 {target} 未暂存的改动"),
        "git_commit" => format!("提交改动：{target}"),
        "run_command_background" => format!("后台启动 `{target}`"),
        "find_definition" => format!("查找 `{target}` 的定义"),
        "task" => format!("委派子任务：{target}"),
        _ => format!("执行 {fn_name}，继续处理任务"),
    };
    if call_count > 1 {
        format!("我准备并行执行 {call_count} 项检查，先{action}。")
    } else {
        format!("我正在{action}。")
    }
}

fn tools_for_mode(mode: CodingMode) -> serde_json::Value {
    let all = tools::tool_schema();
    match mode {
        CodingMode::Build => all,
        CodingMode::Plan => {
            // glob/todo_write/skill/question are read-only or pure-UI
            // interactions (never touch the filesystem/network/command
            // execution), exposed in Plan mode too, same tier as read_file
            // and other analysis-only tools. webfetch makes a real network
            // request, and MCP tools can't be proven read-only, so both
            // stay Build-mode-only (MCP tools are dynamically spliced into
            // the `tools` array, not in this static whitelist, so they
            // naturally only ever show up when `send_message` checks
            // `mode == Build`).
            const READ_ONLY: &[&str] = &[
                "read_file",
                "list_directory",
                "search_files",
                "web_search",
                "glob",
                "todo_write",
                "skill",
                "question",
                "git_status",
                "git_diff",
                "find_definition",
                "task",
            ];
            serde_json::Value::Array(
                all.as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|t| READ_ONLY.contains(&t["function"]["name"].as_str().unwrap_or("")))
                    .collect(),
            )
        }
    }
}

/// OpenAI function-calling tool names must match `^[a-zA-Z0-9_-]+$`; MCP
/// server/tool names are user-chosen and may contain Chinese characters or
/// spaces -- illegal characters get replaced before splicing into
/// `mcp__<server>__<tool>`, to avoid the whole request being rejected by
/// the provider over an invalid tool name.
fn sanitize_tool_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
