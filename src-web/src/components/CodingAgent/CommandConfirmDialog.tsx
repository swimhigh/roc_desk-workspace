import React, { useMemo, useState } from "react";
import { ConfirmDialog } from "../shared/ConfirmDialog";
import { highlightShellCommand } from "../../utils/shellHighlight";

interface CommandConfirmDialogProps {
  open: boolean;
  host?: string;
  command: string;
  kind: "command" | "mcp";
  suggestedPattern: string;
  onReject: () => void;
  onAllowOnce: () => void;
  onAllowAndRemember: (pattern: string) => void;
}

/** Second confirmation for a risky command / MCP tool call. A blacklist
 * hit doesn't use this dialog -- that renders a non-actionable
 * `<BlockedCommandMessage>` instead, with no bypass button. */
export const CommandConfirmDialog: React.FC<CommandConfirmDialogProps> = ({
  open,
  host,
  command,
  kind,
  suggestedPattern,
  onReject,
  onAllowOnce,
  onAllowAndRemember,
}) => {
  const [pattern, setPattern] = useState(suggestedPattern);

  React.useEffect(() => {
    setPattern(suggestedPattern);
  }, [suggestedPattern, open]);

  const title = kind === "mcp" ? "确认 MCP 工具调用" : "确认执行命令";
  const label = kind === "mcp" ? "调用：" : "命令：";
  const highlightedCommand = useMemo(() => (kind === "mcp" ? null : highlightShellCommand(command)), [kind, command]);

  return (
    <ConfirmDialog
      open={open}
      severity="warning"
      icon="⚠"
      title={title}
      dismissible
      onDismiss={onReject}
      actions={
        <>
          <button className="btn ghost sm" onClick={onReject}>拒绝</button>
          <button className="btn ghost sm" onClick={onAllowOnce}>仅本次允许</button>
          <button className="btn primary sm" onClick={() => onAllowAndRemember(pattern)} disabled={!pattern.trim()}>
            允许并记住
          </button>
        </>
      }
    >
      {host && <div className="cmd-confirm-host">目标主机: {host} · 远程</div>}
      <div style={{ fontSize: 12, color: "var(--text-secondary)", marginBottom: 4 }}>{label}</div>
      <div className="cmd-confirm-code">
        {kind === "mcp" ? (
          command
        ) : (
          <>
            <span className="cmd-confirm-prompt">$</span>
            <span dangerouslySetInnerHTML={{ __html: highlightedCommand ?? "" }} />
          </>
        )}
      </div>
      <p style={{ fontSize: 12, color: "var(--text-secondary)" }}>
        此{kind === "mcp" ? "工具调用" : "命令"}将{host ? "在远程主机上" : "本地"}执行，请确认你了解其影响。
      </p>
      <div className="form-row" style={{ marginTop: 8 }}>
        <label className="form-label">"允许并记住"会保存这条规则，以后自动放行匹配此模式的{kind === "mcp" ? "调用" : "命令"}（支持 * / ?）</label>
        <input className="form-input" value={pattern} onChange={(e) => setPattern(e.target.value)} />
      </div>
    </ConfirmDialog>
  );
};

interface BlockedCommandMessageProps {
  command: string;
}

export const BlockedCommandMessage: React.FC<BlockedCommandMessageProps> = ({ command }) => (
  <div className="blocked-cmd-msg">
    🛑 已拦截高危命令：{command}，如需执行请前往终端手动操作
  </div>
);
