import React, { useEffect, useState } from "react";
import { RotateCw } from "lucide-react";
import { gitService } from "../services";

interface GitPanelProps {
  cwd: string;
}

type Section = "status" | "diff" | "log";

/**
 * Local-only Git panel backed by `lib/src/git.rs` (status/diff/log/commit,
 * talking to the `git` binary directly). Not a staged-hunk UI like the
 * host's -- that level of polish belongs to a future pass; this is "can you
 * see repo state and commit from the workspace tool" for a real smoke test.
 */
export const GitPanel: React.FC<GitPanelProps> = ({ cwd }) => {
  const [isRepo, setIsRepo] = useState<boolean | null>(null);
  const [branch, setBranch] = useState("");
  const [section, setSection] = useState<Section>("status");
  const [output, setOutput] = useState("");
  const [commitMessage, setCommitMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    setBusy(true);
    setError(null);
    try {
      const repo = await gitService.isRepo(cwd);
      setIsRepo(repo);
      if (!repo) return;
      setBranch(await gitService.currentBranch(cwd));
      const text =
        section === "status"
          ? await gitService.status(cwd)
          : section === "diff"
            ? await gitService.diff(cwd)
            : await gitService.log(cwd);
      setOutput(text);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cwd, section]);

  const commitAll = async () => {
    if (!commitMessage.trim()) return;
    setBusy(true);
    setError(null);
    try {
      // `git commit -a`-equivalent isn't in the backend's argv surface
      // (deliberately, to avoid accidentally committing files the user
      // didn't intend); this uses `git add -A` first via `.` as the path.
      const result = await gitService.commitPaths(cwd, ["."], commitMessage);
      setOutput(result);
      setCommitMessage("");
      setSection("log");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  if (isRepo === false) {
    return <div className="git-panel">当前工作区不是一个 Git 仓库。</div>;
  }

  return (
    <div className="git-panel">
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
        <span style={{ fontSize: 12, color: "var(--text-secondary)" }}>分支：{branch || "—"}</span>
        <button className="btn ghost" onClick={() => void refresh()} title="刷新" style={{ marginLeft: "auto" }}>
          <RotateCw style={{ width: 12, height: 12 }} />
        </button>
      </div>
      <div className="tabs" style={{ marginBottom: 8 }}>
        {(["status", "diff", "log"] as Section[]).map((s) => (
          <div key={s} className={`tab ${section === s ? "active" : ""}`} onClick={() => setSection(s)}>
            {s === "status" ? "状态" : s === "diff" ? "未暂存改动" : "历史"}
          </div>
        ))}
      </div>
      {error && <div className="error-banner">{error}</div>}
      <pre className="git-output">{busy ? "加载中…" : output || "（空）"}</pre>
      <div className="git-actions">
        <input
          type="text"
          placeholder="提交信息"
          value={commitMessage}
          onChange={(e) => setCommitMessage(e.target.value)}
          style={{ flex: 1, minWidth: 200, background: "var(--bg-surface-raised)", color: "var(--text-primary)", border: "1px solid var(--border)", borderRadius: 4, padding: "5px 8px" }}
        />
        <button className="btn primary" disabled={busy || !commitMessage.trim()} onClick={() => void commitAll()}>
          全部提交（git add -A && commit）
        </button>
      </div>
    </div>
  );
};
