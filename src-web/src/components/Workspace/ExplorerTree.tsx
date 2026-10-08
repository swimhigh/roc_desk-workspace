import React, { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Folder, FolderOpen, File as FileIcon } from "lucide-react";
import {
  fsService,
  useFileTreeOperations,
  parentOf,
  baseName,
  flattenVisible,
  type FileTreeBackend,
  type FileEntry,
} from "@roc_desk/tool-editor";
import { useExplorerStore } from "../../stores/explorerStore";
import { useEditorStore } from "@roc_desk/tool-editor";
import { useToastStore } from "../shared/Toast";
import { ContextMenu, type ContextMenuItem } from "../shared/ContextMenu";
import { ConfirmDialog } from "../shared/ConfirmDialog";
import { formatError } from "../../utils/error";

/** 文本类文件的粗略判断——只用于"选择进行比较"菜单项要不要出现，不是完整的
 * 预览分类（图片/PDF/Word/Excel 预览本身是 `EditorPane`/`CodeEditor` 自己的事）。 */
function looksLikeTextFile(path: string): boolean {
  const ext = path.split(/[\\/]/).pop()?.split(".").pop()?.toLowerCase() ?? "";
  const binary = new Set([
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg",
    "pdf", "docx", "xlsx", "doc", "xls", "ppt", "pptx",
    "exe", "dll", "so", "dylib", "jar",
    "zip", "tar", "gz", "7z", "rar", "woff", "woff2", "ttf", "eot",
    "db", "sqlite", "class", "wasm", "mp4", "mp3", "wav", "avi", "mov",
  ]);
  return !binary.has(ext);
}

interface ExplorerTreeProps {
  workspaceId: string;
  rootPath: string;
  onOpenFile: (path: string, opts?: { pin?: boolean }) => void;
  onCompare: (leftPath: string, rightPath: string) => void;
}

/**
 * 工作区文件树，本地/远程工作区通用（都走 `fsService`/`fs_*`，由后端
 * `WorkspaceHandle.file_ops` 决定具体怎么读写），原样搬自宿主
 * `src-web/src/components/Explorer/ExplorerTree.tsx` 的核心逻辑——懒加载/
 * 多选/剪切复制粘贴/重命名/新建/删除/拖拽移动/右键菜单都保留，去掉了两个
 * 这个独立版没有对应基础设施的功能："运行脚本"（需要宿主那套多标签终端
 * session store）和"导入到本地搜索引擎"（host 独有的日志搜索模块，这个工具
 * 完全没有）——这两个菜单项在这个版本里直接不出现，不是残留的死按钮。
 */
export const ExplorerTree: React.FC<ExplorerTreeProps> = ({ workspaceId, rootPath, onOpenFile, onCompare }) => {
  const { children, expanded, loadRoot, toggleDir, reloadDir, refreshAll, selectedPath, select, compareSource, setCompareSource, rootError } =
    useExplorerStore();
  const push = useToastStore((s) => s.push);
  const [menu, setMenu] = useState<{ x: number; y: number; entry: FileEntry | null; depth: number } | null>(null);
  const createRowRef = useRef<HTMLDivElement>(null);
  const treeContainerRef = useRef<HTMLDivElement>(null);
  const [dragPath, setDragPath] = useState<string | null>(null);
  const [dropPath, setDropPath] = useState<string | null>(null);

  const rootEntries = children[rootPath] ?? [];

  const backend: FileTreeBackend = useMemo(
    () => ({
      deleteFile: (path, isDir) => fsService.deleteFile(workspaceId, path, isDir),
      rename: (from, to) => fsService.rename(workspaceId, from, to),
      copy: (from, to, isDir) => fsService.copy(workspaceId, from, to, isDir),
      createDir: (path) => fsService.createDir(workspaceId, path),
      writeFile: (path, content) => fsService.writeFile(workspaceId, path, content, null).then(() => undefined),
    }),
    [workspaceId],
  );
  const ops = useFileTreeOperations({
    backend,
    reloadDir: (path) => {
      const normalized = path.replace(/\\/g, "/").replace(/\/$/, "");
      const normalizedRoot = rootPath.replace(/\\/g, "/").replace(/\/$/, "");
      const target = path === "" || normalized.toLowerCase() === normalizedRoot.toLowerCase() ? rootPath : path;
      return reloadDir(workspaceId, target);
    },
    select,
    getFlattenedVisible: () => flattenVisible(rootEntries, children, expanded),
    childrenOf: (parentPath) => children[parentPath],
    onOpenFile,
    onFileDeleted: (path) => {
      if (useEditorStore.getState().buffers[path]) useEditorStore.getState().close(path);
    },
  });
  const {
    renamingPath,
    renameValue,
    setRenameValue,
    startRename,
    cancelRename,
    commitRename,
    creating,
    createValue,
    setCreateValue,
    cancelCreate,
    commitCreate,
    deleteTargets,
    requestDelete,
    cancelDelete,
    confirmDelete,
    clipboard,
    setClipboard,
    pasteInto,
    multiSelected,
    handleItemClick,
    handleContextMenuSelect,
    clearSelection,
    batchMenuItems,
  } = ops;

  const startCreate = async (parentPath: string, depth: number, isDir: boolean) => {
    if (!expanded.has(parentPath)) {
      await toggleDir(workspaceId, parentPath);
    }
    ops.startCreate(parentPath, depth, isDir);
  };

  useEffect(() => {
    loadRoot(workspaceId, rootPath);
  }, [workspaceId, rootPath, loadRoot]);

  useEffect(() => {
    let disposed = false;
    let refreshTimer: ReturnType<typeof setTimeout> | undefined;
    let unlisten: (() => void) | undefined;

    void listen<{ workspaceId: string }>("fs:changed", (event) => {
      if (event.payload.workspaceId !== workspaceId) return;
      clearTimeout(refreshTimer);
      refreshTimer = setTimeout(() => {
        void refreshAll(workspaceId, rootPath);
      }, 100);
    }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });

    return () => {
      disposed = true;
      clearTimeout(refreshTimer);
      unlisten?.();
    };
  }, [workspaceId, rootPath, refreshAll]);

  useEffect(() => {
    if (creating) createRowRef.current?.scrollIntoView({ block: "nearest" });
  }, [creating]);

  useEffect(() => {
    if (!selectedPath) return;
    const el = treeContainerRef.current?.querySelector<HTMLElement>(`[data-path="${CSS.escape(selectedPath)}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [selectedPath]);

  const moveDraggedInto = async (targetDir: string) => {
    if (!dragPath) return;
    const source = dragPath;
    const sourceParent = parentOf(source);
    const normalizedSource = source.replace(/\\/g, "/").replace(/\/$/, "").toLowerCase();
    const normalizedTarget = targetDir.replace(/\\/g, "/").replace(/\/$/, "").toLowerCase();
    if (normalizedSource === normalizedTarget || normalizedTarget.startsWith(`${normalizedSource}/`)) {
      push("error", "不能把文件夹移动到自身或其子目录中");
      return;
    }
    const name = baseName(source);
    const destination = `${targetDir}/${name}`;
    try {
      await fsService.rename(workspaceId, source, destination);
      setDragPath(null);
      setDropPath(null);
      await reloadDir(workspaceId, targetDir);
      if (sourceParent !== targetDir) await reloadDir(workspaceId, sourceParent);
      select(destination);
    } catch (e) {
      push("error", `移动失败：${formatError(e)}`);
    }
  };

  const menuItems = (entry: FileEntry, depth: number): ContextMenuItem[] => {
    const normalizedRoot = rootPath.replace(/\\/g, "/").replace(/\/$/, "");
    const normalizedEntryPath = entry.path.replace(/\\/g, "/");
    const relativePath = normalizedEntryPath.toLowerCase().startsWith(normalizedRoot.toLowerCase())
      ? normalizedEntryPath.slice(normalizedRoot.length).replace(/^\//, "")
      : entry.path;
    const items: ContextMenuItem[] = [];
    if (!entry.is_dir) {
      items.push({ label: "打开", onClick: () => onOpenFile(entry.path) });
    } else {
      items.push({ label: "刷新", onClick: () => reloadDir(workspaceId, entry.path) });
    }
    const createTargetDir = entry.is_dir ? entry.path : parentOf(entry.path);
    const createTargetDepth = entry.is_dir ? depth + 1 : depth;
    items.push(
      { label: "新建文件", onClick: () => startCreate(createTargetDir, createTargetDepth, false) },
      { label: "新建文件夹", onClick: () => startCreate(createTargetDir, createTargetDepth, true) },
    );
    items.push(
      { label: "重命名", onClick: () => startRename(entry), separatorBefore: !entry.is_dir },
      { label: "删除", onClick: () => requestDelete([entry]), danger: true },
      {
        label: "剪切",
        onClick: () => setClipboard({ items: [{ path: entry.path, name: entry.name, isDir: entry.is_dir }], mode: "cut" }),
        separatorBefore: true,
      },
    );
    items.push({
      label: "复制",
      onClick: () => setClipboard({ items: [{ path: entry.path, name: entry.name, isDir: entry.is_dir }], mode: "copy" }),
    });
    if (clipboard) {
      items.push({ label: "粘贴", onClick: () => pasteInto(entry.is_dir ? entry.path : parentOf(entry.path)) });
    }
    if (!entry.is_dir && looksLikeTextFile(entry.path)) {
      items.push({ label: "选择进行比较", onClick: () => setCompareSource(entry.path), separatorBefore: true });
      if (compareSource && compareSource !== entry.path && looksLikeTextFile(compareSource)) {
        items.push({ label: `与"${baseName(compareSource)}"比较`, onClick: () => onCompare(compareSource, entry.path) });
      }
    }
    items.push(
      { label: "复制路径", onClick: () => navigator.clipboard.writeText(entry.path), separatorBefore: true },
      { label: "复制相对路径", onClick: () => navigator.clipboard.writeText(relativePath) },
    );
    return items;
  };

  const renderNode = (entry: FileEntry, depth: number) => {
    const isExpanded = expanded.has(entry.path);
    const isRenaming = renamingPath === entry.path;
    return (
      <React.Fragment key={entry.path}>
        <div
          className={`tree-item ${selectedPath === entry.path ? "active" : ""} ${multiSelected.has(entry.path) ? "multi-selected" : ""} ${dropPath === entry.path ? "drop-target" : ""}`}
          draggable
          style={{ paddingLeft: 8 + depth * 16 }}
          data-path={entry.path}
          onClick={(e) => {
            if (isRenaming) return;
            handleItemClick(e, entry, (target) => {
              if (target.is_dir) {
                toggleDir(workspaceId, target.path);
              } else {
                onOpenFile(target.path);
              }
            });
          }}
          onDoubleClick={() => {
            if (!entry.is_dir && !isRenaming) {
              onOpenFile(entry.path, { pin: true });
            }
          }}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            handleContextMenuSelect(entry);
            setMenu({ x: e.clientX, y: e.clientY, entry, depth });
          }}
          onDragStart={(e) => {
            setDragPath(entry.path);
            e.dataTransfer.effectAllowed = "move";
            e.dataTransfer.setData("text/plain", entry.path);
          }}
          onDragEnd={() => {
            setDragPath(null);
            setDropPath(null);
          }}
          onDragOver={(e) => {
            if (!entry.is_dir || !dragPath || dragPath === entry.path) return;
            e.preventDefault();
            e.dataTransfer.dropEffect = "move";
            setDropPath(entry.path);
          }}
          onDragLeave={() => {
            if (dropPath === entry.path) setDropPath(null);
          }}
          onDrop={(e) => {
            e.preventDefault();
            if (entry.is_dir) void moveDraggedInto(entry.path);
            else setDropPath(null);
          }}
        >
          {entry.is_dir ? (
            isExpanded ? <FolderOpen className="tree-icon is-dir" /> : <Folder className="tree-icon is-dir" />
          ) : (
            <FileIcon className="tree-icon" />
          )}
          {isRenaming ? (
            <input
              className="tree-rename-input"
              autoFocus
              value={renameValue}
              onClick={(e) => e.stopPropagation()}
              onChange={(e) => setRenameValue(e.target.value)}
              onBlur={() => commitRename(entry)}
              onKeyDown={(e) => {
                if (e.key === "Enter") commitRename(entry);
                if (e.key === "Escape") cancelRename();
              }}
            />
          ) : (
            <span className="tree-name">{entry.name}</span>
          )}
        </div>
        {entry.is_dir && isExpanded && children[entry.path]?.map((child) => renderNode(child, depth + 1))}
        {entry.is_dir && isExpanded && creating?.parentPath === entry.path && renderCreateRow()}
      </React.Fragment>
    );
  };

  const renderCreateRow = () => {
    if (!creating) return null;
    return (
      <div ref={createRowRef} className="tree-item" style={{ paddingLeft: 8 + creating.depth * 16 }}>
        {creating.isDir ? <Folder className="tree-icon is-dir" /> : <FileIcon className="tree-icon" />}
        <input
          className="tree-rename-input"
          autoFocus
          value={createValue}
          onClick={(e) => e.stopPropagation()}
          onChange={(e) => setCreateValue(e.target.value)}
          onBlur={commitCreate}
          onKeyDown={(e) => {
            if (e.key === "Enter") commitCreate();
            if (e.key === "Escape") cancelCreate();
          }}
        />
      </div>
    );
  };

  return (
    <div
      ref={treeContainerRef}
      className="project-tree"
      onDragOver={(e) => {
        if (!dragPath) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        setDropPath(rootPath);
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) setDropPath(null);
      }}
      onDrop={(e) => {
        e.preventDefault();
        if (e.target === e.currentTarget) void moveDraggedInto(rootPath);
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) clearSelection();
      }}
      onContextMenu={(e) => {
        if (e.target !== e.currentTarget) return;
        e.preventDefault();
        setMenu({ x: e.clientX, y: e.clientY, entry: null, depth: 0 });
      }}
    >
      {rootError ? (
        <div style={{ padding: 16, fontSize: 12, color: "var(--text-secondary)" }}>
          <div style={{ color: "var(--danger, #e5484d)", marginBottom: 8 }}>加载失败：{rootError}</div>
          <button className="btn ghost sm" onClick={() => loadRoot(workspaceId, rootPath)}>
            重试
          </button>
        </div>
      ) : rootEntries.length === 0 && !creating ? (
        <div style={{ padding: 16, fontSize: 12, color: "var(--text-secondary)" }}>此文件夹是空的</div>
      ) : (
        rootEntries.map((entry) => renderNode(entry, 0))
      )}
      {creating?.parentPath === rootPath && renderCreateRow()}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={
            menu.entry
              ? multiSelected.size > 1 && multiSelected.has(menu.entry.path)
                ? batchMenuItems(flattenVisible(rootEntries, children, expanded).filter((e) => multiSelected.has(e.path)))
                : menuItems(menu.entry, menu.depth)
              : [
                  { label: "新建文件", onClick: () => startCreate(rootPath, 0, false) },
                  { label: "新建文件夹", onClick: () => startCreate(rootPath, 0, true) },
                  { label: "刷新", onClick: () => refreshAll(workspaceId, rootPath), separatorBefore: true },
                  ...(clipboard ? [{ label: "粘贴", onClick: () => pasteInto(rootPath), separatorBefore: true }] : []),
                ]
          }
          onClose={() => setMenu(null)}
        />
      )}

      {deleteTargets.length > 0 && (
        <ConfirmDialog
          open
          severity="danger"
          icon="🗑"
          title="确认删除"
          onDismiss={cancelDelete}
          actions={
            <>
              <button className="btn ghost sm" onClick={cancelDelete}>
                取消
              </button>
              <button className="btn danger-strong sm" onClick={confirmDelete}>
                删除
              </button>
            </>
          }
        >
          {deleteTargets.length === 1 ? (
            <p>
              确定要删除{deleteTargets[0].is_dir ? "目录" : "文件"} <strong>{deleteTargets[0].name}</strong> 吗？此操作不可撤销。
            </p>
          ) : (
            <p>
              确定要删除选中的 <strong>{deleteTargets.length}</strong> 项吗？此操作不可撤销。
            </p>
          )}
        </ConfirmDialog>
      )}
    </div>
  );
};
