import React, { useEffect, useState } from "react";
import { Folder, FolderOpen, File as FileIcon, RotateCw } from "lucide-react";
import { localFsService, type FileEntry } from "../services";

interface FileTreeProps {
  root: string;
  onOpenFile: (path: string) => void;
}

function sortEntries(entries: FileEntry[]): FileEntry[] {
  return [...entries].sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    return a.name.toLowerCase().localeCompare(b.name.toLowerCase());
  });
}

/**
 * Minimal single-root file tree scoped to the open workspace. Deliberately
 * not the host's full-featured tree (multi-select, cut/copy/paste, context
 * menu) -- see the task's final report for why: this crate's frontend keeps
 * things self-contained rather than pulling in `roc_desk-editor`'s richer
 * `LocalFileTree` (which itself depends on shared components that haven't
 * been extracted into a reusable npm package yet).
 */
export const FileTree: React.FC<FileTreeProps> = ({ root, onOpenFile }) => {
  const [children, setChildren] = useState<Record<string, FileEntry[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<string | null>(null);

  const load = async (path: string) => {
    try {
      const entries = await localFsService.listDir(path);
      setChildren((c) => ({ ...c, [path]: sortEntries(entries) }));
    } catch {
      // Directory disappeared/unreadable -- leave the tree as-is.
    }
  };

  useEffect(() => {
    setChildren({});
    setExpanded(new Set([root]));
    void load(root);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root]);

  const toggle = (entry: FileEntry) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(entry.path)) next.delete(entry.path);
      else {
        next.add(entry.path);
        if (!children[entry.path]) void load(entry.path);
      }
      return next;
    });
  };

  const renderNode = (entry: FileEntry, depth: number): React.ReactNode => {
    const isExpanded = expanded.has(entry.path);
    return (
      <React.Fragment key={entry.path}>
        <div
          className={`tree-item ${selected === entry.path ? "active" : ""}`}
          style={{ paddingLeft: 8 + depth * 14 }}
          onClick={() => {
            setSelected(entry.path);
            if (entry.is_dir) toggle(entry);
            else onOpenFile(entry.path);
          }}
        >
          {entry.is_dir ? (
            isExpanded ? <FolderOpen className="tree-icon is-dir" /> : <Folder className="tree-icon is-dir" />
          ) : (
            <FileIcon className="tree-icon" />
          )}
          <span>{entry.name}</span>
        </div>
        {entry.is_dir && isExpanded && children[entry.path]?.map((child) => renderNode(child, depth + 1))}
      </React.Fragment>
    );
  };

  const rootName = root.split(/[\\/]/).filter(Boolean).pop() ?? root;

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 4, padding: "4px 8px", borderBottom: "1px solid var(--border)" }}>
        <span style={{ flex: 1, fontSize: 11, color: "var(--text-secondary)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={root}>
          {rootName}
        </span>
        <button
          className="btn ghost"
          title="刷新"
          onClick={() => {
            setChildren({});
            void load(root);
          }}
          style={{ padding: 2 }}
        >
          <RotateCw style={{ width: 12, height: 12 }} />
        </button>
      </div>
      <div style={{ flex: 1, overflow: "auto" }}>{children[root]?.map((child) => renderNode(child, 0))}</div>
    </div>
  );
};
