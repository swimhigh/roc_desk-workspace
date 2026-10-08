import React, { useEffect, useState } from "react";
import {
  connectionService,
  connectionGroupService,
  sftpService,
  agentService,
  ConnectionForm,
  type ConnectionFormValue,
  type ConnectionGroup,
  type ConnectionProfile,
  type FileEntry,
  type Protocol,
  isAppError,
} from "@roc_desk/tool-ssh";
import { workspaceService, type WorkspaceProfile } from "../../services";
import { formatError } from "../../utils/error";
import { useToastStore } from "../shared/Toast";
import { PasswordPromptDialog } from "./PasswordPromptDialog";
import { AGENT_ROOT, agentParentPath } from "../../utils/windowsPath";

function isAgentRoot(protocol: Protocol, path: string) {
  return protocol === "agent" && path === AGENT_ROOT;
}

function sshParentPath(path: string): string {
  return path.split("/").slice(0, -1).join("/") || "/";
}

type Step = "pick" | "new" | "browse";

interface RemoteWorkspaceDialogProps {
  onClose: () => void;
  /** 传入时表示"编辑已存在的工作区目录"，而不是新建工作区。 */
  editWorkspace?: WorkspaceProfile & { connection_id: string };
  onOpened: (profile: WorkspaceProfile) => void;
}

/**
 * "连接远程主机并选择目录"的三步流程，原样搬自宿主
 * `src-web/src/components/Workspace/RemoteWorkspaceDialog.tsx`，服务层换成
 * `@roc_desk/tool-ssh` 导出的那几个（这个独立版自己没有连接管理 UI/状态，直接
 * 用工具库里现成的 `ConnectionForm`/`connectionService` 等）。
 */
export const RemoteWorkspaceDialog: React.FC<RemoteWorkspaceDialogProps> = ({ onClose, editWorkspace, onOpened }) => {
  const [step, setStep] = useState<Step>(editWorkspace ? "browse" : "pick");
  const [profiles, setProfiles] = useState<ConnectionProfile[]>([]);
  const [groups, setGroups] = useState<ConnectionGroup[]>([]);
  const [activeConnectionId, setActiveConnectionId] = useState<string | null>(editWorkspace?.connection_id ?? null);
  const [activeProtocol, setActiveProtocol] = useState<Protocol>("ssh");
  const [cwd, setCwd] = useState(editWorkspace?.root_path ?? "/");
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [browsing, setBrowsing] = useState(false);
  const [passwordPromptFor, setPasswordPromptFor] = useState<ConnectionProfile | null>(null);
  const [savingPassword, setSavingPassword] = useState(false);
  const push = useToastStore((s) => s.push);

  const startBrowsing = async (connectionId: string, path: string, protocol: Protocol) => {
    setActiveConnectionId(connectionId);
    setActiveProtocol(protocol);
    setStep("browse");
    setBrowsing(true);
    try {
      if (protocol === "agent") {
        if (isAgentRoot(protocol, path)) {
          const roots = await agentService.listRoots(connectionId);
          setEntries(roots.map((r) => ({ name: r, path: r, is_dir: true, size: null, modified: null })));
        } else {
          const dirs = await agentService.listDir(connectionId, path);
          setEntries(dirs.filter((e) => e.is_dir));
        }
      } else {
        const dirs = await sftpService.listDir(connectionId, path);
        setEntries(dirs.filter((e) => e.is_dir));
      }
      setCwd(path);
    } catch (e) {
      if (isAppError(e) && e.kind === "Auth") {
        const profile = profiles.find((p) => p.id === connectionId);
        if (profile) {
          setPasswordPromptFor(profile);
          setStep("pick");
          return;
        }
      }
      push("error", `浏览失败：${formatError(e)}`);
      setStep("pick");
    } finally {
      setBrowsing(false);
    }
  };

  useEffect(() => {
    connectionService
      .list()
      .then((loaded) => {
        setProfiles(loaded);
        if (editWorkspace) {
          const profile = loaded.find((p) => p.id === editWorkspace.connection_id);
          void startBrowsing(editWorkspace.connection_id, editWorkspace.root_path, profile?.protocol ?? "ssh");
        }
      })
      .catch((e) => push("error", formatError(e)));
    connectionGroupService.list().then(setGroups).catch((e) => push("error", formatError(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !passwordPromptFor && !savingPassword) onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose, passwordPromptFor, savingPassword]);

  const handleSavePassword = async (password: string) => {
    if (!passwordPromptFor) return;
    setSavingPassword(true);
    try {
      await connectionService.update(passwordPromptFor.id, {
        name: passwordPromptFor.name,
        host: passwordPromptFor.host,
        port: passwordPromptFor.port,
        username: passwordPromptFor.username,
        auth_method: passwordPromptFor.auth_method,
        secret: password,
        group_id: passwordPromptFor.group_id,
        tags: passwordPromptFor.tags,
        jump_host_id: passwordPromptFor.jump_host_id,
        protocol: passwordPromptFor.protocol,
        options: passwordPromptFor.options,
      });
      const connectionId = passwordPromptFor.id;
      const protocol = passwordPromptFor.protocol;
      setPasswordPromptFor(null);
      await startBrowsing(connectionId, protocol === "agent" ? AGENT_ROOT : "/", protocol);
    } catch (e) {
      push("error", `保存密码失败：${formatError(e)}`);
    } finally {
      setSavingPassword(false);
    }
  };

  const handleCreateConnection = async (value: ConnectionFormValue) => {
    try {
      const created = await connectionService.create({
        name: value.name,
        host: value.host,
        port: value.port,
        username: value.username,
        auth_method: value.protocol === "agent" ? "key" : value.authMethod,
        secret: value.secret || null,
        group_id: value.groupId ?? null,
        tags: [],
        jump_host_id: value.jumpHostId ?? null,
        protocol: value.protocol,
        options: null,
      });
      setProfiles((p) => [...p, created]);
      const initialPath =
        value.protocol === "agent" ? AGENT_ROOT : created.username.startsWith("root") ? "/root" : `/home/${created.username}`;
      await startBrowsing(created.id, initialPath, value.protocol);
    } catch (e) {
      push("error", `创建连接失败：${formatError(e)}`);
    }
  };

  const handleConfirm = async () => {
    if (!activeConnectionId) return;
    try {
      if (editWorkspace) {
        const profile = await workspaceService.updatePath(editWorkspace.id, cwd);
        push("success", "工作区目录已更新");
        onOpened(profile);
      } else {
        const profile = await workspaceService.openRemote(activeConnectionId, cwd);
        onOpened(profile);
      }
      onClose();
    } catch (e) {
      push("error", `${editWorkspace ? "更新工作区目录" : "打开工作区"}失败：${formatError(e)}`);
    }
  };

  return (
    <div className="dialog-overlay" role="dialog" aria-modal="true">
      <div className="dialog" style={{ minWidth: 480, maxWidth: 560 }}>
        <div className="dialog-title-bar info">
          <span>🖥</span>
          <span>{editWorkspace ? "修改工作区目录" : "连接远程主机并选择目录"}</span>
        </div>
        <div className="dialog-body">
          {step === "pick" && (
            <div>
              {profiles.length === 0 ? (
                <p style={{ fontSize: 13, color: "var(--text-secondary)", marginBottom: 12 }}>还没有已保存的连接。</p>
              ) : (
                <div style={{ display: "flex", flexDirection: "column", gap: 4, marginBottom: 12 }}>
                  {profiles
                    .filter((p) => p.protocol !== "rdp")
                    .map((p) => (
                      <div
                        key={p.id}
                        className="file-row"
                        style={{ gridTemplateColumns: "1fr auto", cursor: "pointer" }}
                        onClick={() => startBrowsing(p.id, p.protocol === "agent" ? AGENT_ROOT : "/", p.protocol)}
                      >
                        <span>
                          {p.name} — {p.protocol === "agent" ? `${p.host}:${p.port}（Agent）` : `${p.username}@${p.host}:${p.port}`}
                        </span>
                        <span style={{ color: "var(--text-secondary)", fontSize: 12 }}>连接 →</span>
                      </div>
                    ))}
                </div>
              )}
              <button className="btn ghost sm" onClick={() => setStep("new")}>
                + 新建连接
              </button>
            </div>
          )}

          {step === "new" && (
            <ConnectionForm
              groups={groups.map((g) => ({ id: g.id, name: g.name }))}
              jumpHostOptions={profiles.filter((p) => p.protocol === "ssh").map((p) => ({ id: p.id, name: p.name }))}
              allowedProtocols={["ssh", "agent"]}
              onCancel={() => setStep("pick")}
              onSave={handleCreateConnection}
            />
          )}

          {step === "browse" && (
            <div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)", marginBottom: 8 }}>
                当前路径：
                <span style={{ fontFamily: "var(--font-mono)" }}>{isAgentRoot(activeProtocol, cwd) ? "此电脑" : cwd}</span>
              </div>
              {browsing ? (
                <div style={{ padding: 16, fontSize: 12, color: "var(--text-secondary)" }}>加载中…</div>
              ) : (
                <div style={{ maxHeight: 260, overflowY: "auto", border: "1px solid var(--border-default)", borderRadius: 4 }}>
                  {!isAgentRoot(activeProtocol, cwd) && (
                    <div
                      className="file-row"
                      style={{ gridTemplateColumns: "1fr" }}
                      onClick={() =>
                        activeConnectionId &&
                        startBrowsing(
                          activeConnectionId,
                          activeProtocol === "agent" ? agentParentPath(cwd) : sshParentPath(cwd),
                          activeProtocol,
                        )
                      }
                    >
                      <span>📁 ..</span>
                    </div>
                  )}
                  {entries.map((e) => (
                    <div
                      key={e.path}
                      className="file-row"
                      style={{ gridTemplateColumns: "1fr" }}
                      onClick={() => activeConnectionId && startBrowsing(activeConnectionId, e.path, activeProtocol)}
                    >
                      <span>📁 {e.name}</span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
        <div className="dialog-actions">
          <button className="btn ghost sm" onClick={onClose}>
            取消
          </button>
          {step === "browse" && (
            <button className="btn primary sm" onClick={handleConfirm}>
              {editWorkspace ? "保存新目录" : "选定此目录作为工作区"}
            </button>
          )}
        </div>
      </div>
      {passwordPromptFor && (
        <PasswordPromptDialog
          open
          connectionName={passwordPromptFor.name}
          secretLabel={passwordPromptFor.protocol === "agent" ? "配对令牌" : "密码"}
          submitting={savingPassword}
          onCancel={() => setPasswordPromptFor(null)}
          onSubmit={handleSavePassword}
        />
      )}
    </div>
  );
};
