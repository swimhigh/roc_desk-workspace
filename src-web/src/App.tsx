import React, { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { FolderOpen, X, TerminalSquare, GitBranch } from "lucide-react";
import { workspaceService, type WorkspaceProfile } from "./services";
import { FileTree } from "./components/FileTree";
import { SimpleEditor } from "./components/SimpleEditor";
import { TerminalPanel } from "./components/TerminalPanel";
import { GitPanel } from "./components/GitPanel";

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

export const App: React.FC = () => {
  const [workspace, setWorkspace] = useState<WorkspaceProfile | null>(null);
  const [recent, setRecent] = useState<WorkspaceProfile[]>([]);
  const [openFile, setOpenFile] = useState<string | null>(null);
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
      setOpenFile(null);
      refreshRecent();
    } catch (e) {
      setError(String(e));
    }
  };

  const openRecent = async (profile: WorkspaceProfile) => {
    try {
      const opened = await workspaceService.openLocal(profile.root_path);
      setWorkspace(opened);
      setOpenFile(null);
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

  if (!workspace) {
    return (
      <div className="app-shell">
        {error && <div className="error-banner">{error}</div>}
        <WelcomeScreen recent={recent} onOpen={() => void openFolder()} onOpenRecent={(p) => void openRecent(p)} onRemoveRecent={(id) => void removeRecent(id)} />
      </div>
    );
  }

  return (
    <div className="app-shell">
      <div className="titlebar">
        <span className="titlebar-title">{workspace.display_name}</span>
        <span className="titlebar-path">{workspace.root_path}</span>
        <button className="btn ghost" onClick={() => setWorkspace(null)} title="关闭工作区">
          <X style={{ width: 14, height: 14 }} />
        </button>
      </div>
      {error && <div className="error-banner">{error}</div>}
      <div className="body-row">
        <div className="sidebar">
          <FileTree root={workspace.root_path} onOpenFile={setOpenFile} />
        </div>
        <div className="main-col">
          <div className="main-content">
            {openFile ? (
              <SimpleEditor path={openFile} key={openFile} />
            ) : (
              <div style={{ padding: 16, color: "var(--text-secondary)" }}>从左侧文件树选择一个文件开始编辑。</div>
            )}
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
