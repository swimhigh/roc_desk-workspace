import React, { useState } from "react";
import { ConfirmDialog } from "../shared/ConfirmDialog";

interface PasswordPromptDialogProps {
  open: boolean;
  connectionName: string;
  onCancel: () => void;
  onSubmit: (password: string) => void;
  submitting?: boolean;
  secretLabel?: string;
}

/** 连接缺少已保存密码时的补救弹窗，原样搬自宿主
 * `src-web/src/components/ConnectionManager/PasswordPromptDialog.tsx`。 */
export const PasswordPromptDialog: React.FC<PasswordPromptDialogProps> = ({
  open,
  connectionName,
  onCancel,
  onSubmit,
  submitting,
  secretLabel = "密码",
}) => {
  const [password, setPassword] = useState("");
  const [visible, setVisible] = useState(false);

  return (
    <ConfirmDialog
      open={open}
      severity="warning"
      icon="🔑"
      title={`需要重新输入${secretLabel}`}
      dismissible={!submitting}
      onDismiss={onCancel}
      actions={
        <>
          <button className="btn ghost sm" onClick={onCancel} disabled={submitting}>
            取消
          </button>
          <button
            className="btn primary sm"
            disabled={!password || submitting}
            onClick={() => onSubmit(password)}
          >
            {submitting ? "连接中…" : "保存并连接"}
          </button>
        </>
      }
    >
      <p style={{ marginBottom: 8 }}>
        连接 <strong>{connectionName}</strong> 没有已保存的{secretLabel}（或已失效），请重新输入。
      </p>
      <div className="form-input-group">
        <input
          className="form-input"
          type={visible ? "text" : "password"}
          value={password}
          autoFocus
          onChange={(e) => setPassword(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && password && !submitting) onSubmit(password);
          }}
        />
        <button className="btn ghost sm" onClick={() => setVisible((v) => !v)}>
          {visible ? "隐藏" : "显示"}
        </button>
      </div>
    </ConfirmDialog>
  );
};
