import React, { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { FolderOpen, X, TerminalSquare, GitBranch, Bot, Server } from "lucide-react";
import { workspaceService, type WorkspaceProfile } from "./services";
import { EditorPane, useEditorStore } from "@roc_desk/tool-editor";
import { ExplorerTree } from "./components/Workspace/ExplorerTree";
import { RemoteWorkspaceDialog } from "./components/Workspace/RemoteWorkspaceDialog";
import {
  HostKeyPromptHost,
  AgentCertPromptHost,
  registerHostKeyPromptListener,
  registerAgentCertPromptListener,
} from "@roc_desk/tool-ssh";
import { TerminalPanel } from "./components/TerminalPanel";
import { GitPanel } from "./components/GitPanel";
import { CodingAgentPanel } from "./components/CodingAgent/CodingAgentPanel";
import { registerCodingListeners } from "./stores/codingStore";
import { ThemeToggle } from "./components/shared/ThemeToggle";
import { ToastStack } from "./components/shared/Toast";

type BottomTab = "terminal" | "git" | "ai" | null;

const WelcomeScreen: React.FC<{
  recent: WorkspaceProfile[];
  onOpen: () => void;
  onOpenRemote: () => void;
  onOpenRecent: (p: WorkspaceProfile) => void;
  onRemoveRecent: (id: string) => void;
}> = ({ recent, onOpen, onOpenRemote, onOpenRecent, onRemoveRecent }) => (
  <div className="welcome">
    <h2 style={{ fontWeight: 500 }}>编程工作区</h2>
    <div style={{ display: "flex", gap: 8 }}>
      <button className="btn primary" onClick={onOpen}>
        <FolderOpen style={{ width: 14, height: 14, marginRight: 6, verticalAlign: -2 }} />
        打开文件夹
      </button>
      <button className="btn ghost" onClick={onOpenRemote}>
        <Server style={{ width: 14, height: 14, marginRight: 6, verticalAlign: -2 }} />
        连接远程主机
      </button>
    </div>
    {recent.length > 0 && (
      <>
        <div style={{ color: "var(--text-secondary)", fontSize: 12, marginTop: 16 }}>最近打开</div>
        <ul className="recent-list">
          {recent.map((w) => (
            <li key={w.id} className="recent-item" onClick={() => onOpenRecent(w)}>
              <div>
                <div>{w.display_name}</div>
                <div className="recent-item-path">{w.root_path}</div>
              </div>
              <button
                className="btn ghost"
                onClick={(e) => {
                  e.stopPropagation();
                  onRemoveRecent(w.id);
                }}
                title="移除"
              >
                <X style={{ width: 12, height: 12 }} />
              </button>
            </li>
          ))}
        </ul>
      </>
    )}
  </div>
);

/**
 * 编程工作区的整体布局：左侧真正的本地文件树（`LocalFileTree`，多选/剪切复制/
 * 新建/删除/重命名齐全），右侧真正的 Monaco 编辑面板（`<EditorPane/>`，多标签
 * 编辑 + 图片/PDF/Word/Excel 预览 + OCR + 符号索引跳转）——两者都来自
 * `roc_desk-editor` 通过 `file:` 本地路径依赖（`package.json` 里的
 * `@roc_desk/tool-editor`），不是这个仓库自己拷贝维护的一份，见仓库根
 * `docs`/最终报告里对这次替换的说明。
 *
 * 早期版本这里是自己写的一个极简 `<textarea>`（`SimpleEditor`）+ 只有点击展开/
 * 打开的文件树（`FileTree`），用户反馈"每个工具都应该和原始的 roc_desk 功能
 * 保持一致"后换成这样接。
 *
 * `<EditorPane workspaceId={null} rootPath={...}/>` 是"本地模式"——文件读写走
 * `local_*` 命令（`roc_desk-explorer` 提供，本仓库 `standalone/src/main.rs`
 * 已经注册），不需要这个工具自己实现工作区边界校验的 `fs_*` 命令。
 */
export const App: React.FC = () => {
  const [workspace, setWorkspace] = useState<WorkspaceProfile | null>(null);
  const [recent, setRecent] = useState<WorkspaceProfile[]>([]);
  const [bottomTab, setBottomTab] = useState<BottomTab>(null);
  const [error, setError] = useState<string | null>(null);
  const [showRemoteDialog, setShowRemoteDialog] = useState(false);

  const refreshRecent = () => {
    workspaceService
      .listRecent()
      .then(setRecent)
      .catch((e) => setError(String(e)));
  };

  useEffect(refreshRecent, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    registerCodingListeners().then((fn) => { unlisten = fn; });
    return () => unlisten?.();
  }, []);

  // 第一次连上一台新的 SSH/Agent 主机会触发指纹 TOFU 确认，不监听这两个事件会
  // 让 ConnectionForm/sftpService/agentService 的连接请求永远挂起。
  useEffect(() => {
    let stopHostKey: (() => void) | undefined;
    let stopAgentCert: (() => void) | undefined;
    registerHostKeyPromptListener().then((fn) => { stopHostKey = fn; });
    registerAgentCertPromptListener().then((fn) => { stopAgentCert = fn; });
    return () => {
      stopHostKey?.();
      stopAgentCert?.();
    };
  }, []);

  const openFolder = async () => {
    const selected = await open({ directory: true, multiple: false });
    if (!selected || Array.isArray(selected)) return;
    try {
      const profile = await workspaceService.openLocal(selected);
      setWorkspace(profile);
      refreshRecent();
    } catch (e) {
      setError(String(e));
    }
  };

  const openRecent = async (profile: WorkspaceProfile) => {
    try {
      const opened =
        profile.kind === "remote" && profile.connection_id
          ? await workspaceService.openRemote(profile.connection_id, profile.root_path)
          : await workspaceService.openLocal(profile.root_path);
      setWorkspace(opened);
      refreshRecent();
    } catch (e) {
      setError(String(e));
    }
  };

  const removeRecent = async (id: string) => {
    try {
      await workspaceService.removeRecent(id);
      refreshRecent();
    } catch (e) {
      setError(String(e));
    }
  };

  const closeWorkspace = () => {
    if (workspace) void workspaceService.close(workspace.id);
    setWorkspace(null);
    useEditorStore.getState().closeAll();
  };

  if (!workspace) {
    return (
      <div className="app-shell">
        <div className="top-bar">
          <span className="top-bar-title">编程工作区</span>
          <div className="top-bar-spacer" />
          <ThemeToggle />
        </div>
        {error && <div className="error-banner">{error}</div>}
        <WelcomeScreen
          recent={recent}
          onOpen={() => void openFolder()}
          onOpenRemote={() => setShowRemoteDialog(true)}
          onOpenRecent={(p) => void openRecent(p)}
          onRemoveRecent={(id) => void removeRecent(id)}
        />
        <ToastStack />
        <HostKeyPromptHost />
        <AgentCertPromptHost />
        {showRemoteDialog && (
          <RemoteWorkspaceDialog
            onClose={() => setShowRemoteDialog(false)}
            onOpened={(profile) => {
              setWorkspace(profile);
              refreshRecent();
            }}
          />
        )}
      </div>
    );
  }

  const isRemote = workspace.kind === "remote";

  return (
    <div className="app-shell">
      <div className="titlebar">
        <span className="titlebar-title">{workspace.display_name}</span>
        <span className="titlebar-path">{workspace.root_path}</span>
        <ThemeToggle />
        <button className="btn ghost" onClick={closeWorkspace} title="关闭工作区">
          <X style={{ width: 14, height: 14 }} />
        </button>
      </div>
      {error && <div className="error-banner">{error}</div>}
      <div className="body-row">
        <div className="sidebar">
          <ExplorerTree
            workspaceId={workspace.id}
            rootPath={workspace.root_path}
            onOpenFile={(path, opts) => void useEditorStore.getState().openPreview(workspace.id, path).then(() => {
              if (opts?.pin) useEditorStore.getState().pin(path);
            })}
            onCompare={(l, r) => void useEditorStore.getState().openDiff(workspace.id, l, r)}
          />
        </div>
        <div className="main-col">
          <div className="main-content">
            <EditorPane workspaceId={workspace.id} rootPath={workspace.root_path} />
          </div>
          <div className="bottom-panel" style={{ height: bottomTab === "ai" ? 520 : bottomTab ? 280 : "auto" }}>
            <div className="bottom-panel-header">
              {!isRemote && (
                <div
                  className="tab"
                  style={{ borderRight: "none", color: bottomTab === "terminal" ? "var(--text-primary)" : "var(--text-secondary)" }}
                  onClick={() => setBottomTab(bottomTab === "terminal" ? null : "terminal")}
                >
                  <TerminalSquare style={{ width: 13, height: 13, marginRight: 4, verticalAlign: -2 }} />
                  终端
                </div>
              )}
              {!isRemote && (
                <div
                  className="tab"
                  style={{ borderRight: "none", color: bottomTab === "git" ? "var(--text-primary)" : "var(--text-secondary)" }}
                  onClick={() => setBottomTab(bottomTab === "git" ? null : "git")}
                >
                  <GitBranch style={{ width: 13, height: 13, marginRight: 4, verticalAlign: -2 }} />
                  Git
                </div>
              )}
              <div
                className="tab"
                style={{ borderRight: "none", color: bottomTab === "ai" ? "var(--text-primary)" : "var(--text-secondary)" }}
                onClick={() => setBottomTab(bottomTab === "ai" ? null : "ai")}
              >
                <Bot style={{ width: 13, height: 13, marginRight: 4, verticalAlign: -2 }} />
                AI 编程助手
              </div>
            </div>
            {bottomTab && (
              <div className="bottom-panel-body" style={{ display: "flex", flexDirection: "column" }}>
                {bottomTab === "terminal" && <TerminalPanel cwd={workspace.root_path} key={workspace.id} />}
                {bottomTab === "git" && <GitPanel cwd={workspace.root_path} key={workspace.id} />}
                {bottomTab === "ai" && (
                  <CodingAgentPanel
                    workspaceId={workspace.id}
                    active={bottomTab === "ai"}
                    onOpenFile={(path) => void useEditorStore.getState().openPreview(workspace.id, path)}
                  />
                )}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
};
