import React, { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { FolderOpen, X, TerminalSquare, GitBranch } from "lucide-react";
import { workspaceService, type WorkspaceProfile } from "./services";
import { LocalFileTree, EditorPane, useEditorStore } from "@roc_desk/tool-editor";
import { TerminalPanel } from "./components/TerminalPanel";
import { GitPanel } from "./components/GitPanel";
import { ThemeToggle } from "./components/shared/ThemeToggle";
import { ToastStack } from "./components/shared/Toast";

type BottomTab = "terminal" | "git" | null;

const WelcomeScreen: React.FC<{
  recent: WorkspaceProfile[];
  onOpen: () => void;
  onOpenRecent: (p: WorkspaceProfile) => void;
  onRemoveRecent: (id: string) => void;
}> = ({ recent, onOpen, onOpenRecent, onRemoveRecent }) => (
  <div className="welcome">
    <h2 style={{ fontWeight: 500 }}>编程工作区</h2>
    <button className="btn primary" onClick={onOpen}>
      <FolderOpen style={{ width: 14, height: 14, marginRight: 6, verticalAlign: -2 }} />
      打开文件夹
    </button>
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

  const refreshRecent = () => {
    workspaceService
      .listRecent()
      .then(setRecent)
      .catch((e) => setError(String(e)));
  };

  useEffect(refreshRecent, []);

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
      const opened = await workspaceService.openLocal(profile.root_path);
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
        <WelcomeScreen recent={recent} onOpen={() => void openFolder()} onOpenRecent={(p) => void openRecent(p)} onRemoveRecent={(id) => void removeRecent(id)} />
        <ToastStack />
      </div>
    );
  }

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
          <LocalFileTree
            root={workspace.root_path}
            onRootChange={() => {
              /* 工作区场景的根目录由"打开文件夹"/"最近打开"决定，不通过文件树
               * 自己的"钉常用目录"按钮改变——固定传入 workspace.root_path 即可。*/
            }}
            onOpenFile={(path) => void useEditorStore.getState().openStandaloneFile(path)}
          />
        </div>
        <div className="main-col">
          <div className="main-content">
            <EditorPane workspaceId={null} rootPath={workspace.root_path} />
          </div>
          <div className="bottom-panel" style={{ height: bottomTab ? 280 : "auto" }}>
            <div className="bottom-panel-header">
              <div
                className="tab"
                style={{ borderRight: "none", color: bottomTab === "terminal" ? "var(--text-primary)" : "var(--text-secondary)" }}
                onClick={() => setBottomTab(bottomTab === "terminal" ? null : "terminal")}
              >
                <TerminalSquare style={{ width: 13, height: 13, marginRight: 4, verticalAlign: -2 }} />
                终端
              </div>
              <div
                className="tab"
                style={{ borderRight: "none", color: bottomTab === "git" ? "var(--text-primary)" : "var(--text-secondary)" }}
                onClick={() => setBottomTab(bottomTab === "git" ? null : "git")}
              >
                <GitBranch style={{ width: 13, height: 13, marginRight: 4, verticalAlign: -2 }} />
                Git
              </div>
            </div>
            {bottomTab && (
              <div className="bottom-panel-body">
                {bottomTab === "terminal" && <TerminalPanel cwd={workspace.root_path} key={workspace.id} />}
                {bottomTab === "git" && <GitPanel cwd={workspace.root_path} key={workspace.id} />}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
};
