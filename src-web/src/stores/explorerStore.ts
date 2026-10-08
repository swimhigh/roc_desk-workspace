import { create } from "zustand";
import { fsService, type FileEntry } from "@roc_desk/tool-editor";
import { formatError } from "../utils/error";

interface ExplorerState {
  children: Record<string, FileEntry[]>;
  expanded: Set<string>;
  loading: Set<string>;
  selectedPath: string | null;
  compareSource: string | null;
  rootError: string | null;

  toggleDir: (workspaceId: string, path: string) => Promise<void>;
  loadRoot: (workspaceId: string, rootPath: string) => Promise<void>;
  reloadDir: (workspaceId: string, path: string) => Promise<void>;
  refreshAll: (workspaceId: string, rootPath: string) => Promise<void>;
  select: (path: string) => void;
  setCompareSource: (path: string | null) => void;
  reset: () => void;
}

/** 工作区文件树状态，原样搬自宿主 `src-web/src/stores/explorerStore.ts`，只是
 * `fsService` 换成 `@roc_desk/tool-editor` 已经导出的那一份（和 `EditorPane` 内部
 * `editorStore` 用的是同一个实现，workspaceId 语义一致）。 */
export const useExplorerStore = create<ExplorerState>((set, get) => ({
  children: {},
  expanded: new Set(),
  loading: new Set(),
  selectedPath: null,
  compareSource: null,
  rootError: null,

  loadRoot: async (workspaceId, rootPath) => {
    set({ rootError: null });
    try {
      const entries = await fsService.listDir(workspaceId, rootPath);
      set((s) => ({
        children: { ...s.children, [rootPath]: entries },
        expanded: new Set([rootPath]),
      }));
    } catch (e) {
      set({ rootError: formatError(e) });
    }
  },

  toggleDir: async (workspaceId, path) => {
    const isExpanded = get().expanded.has(path);
    if (isExpanded) {
      set((s) => {
        const next = new Set(s.expanded);
        next.delete(path);
        return { expanded: next };
      });
      return;
    }

    set((s) => ({ expanded: new Set(s.expanded).add(path) }));

    if (!get().children[path]) {
      set((s) => ({ loading: new Set(s.loading).add(path) }));
      try {
        const entries = await fsService.listDir(workspaceId, path);
        set((s) => ({ children: { ...s.children, [path]: entries } }));
      } finally {
        set((s) => {
          const next = new Set(s.loading);
          next.delete(path);
          return { loading: next };
        });
      }
    }
  },

  reloadDir: async (workspaceId, path) => {
    try {
      const entries = await fsService.listDir(workspaceId, path);
      set((s) => ({ children: { ...s.children, [path]: entries } }));
    } catch (e) {
      set({ rootError: formatError(e) });
    }
  },

  refreshAll: async (workspaceId, rootPath) => {
    const targets = new Set(get().expanded);
    targets.add(rootPath);
    const results = await Promise.all(
      Array.from(targets).map((p) =>
        fsService.listDir(workspaceId, p).then(
          (entries) => ({ p, entries, ok: true as const }),
          (e) => ({ p, error: formatError(e), ok: false as const }),
        ),
      ),
    );
    set((s) => {
      const children = { ...s.children };
      const expanded = new Set(s.expanded);
      let rootError: string | null = null;
      for (const r of results) {
        if (r.ok) {
          children[r.p] = r.entries;
        } else if (r.p === rootPath) {
          rootError = r.error;
        } else {
          delete children[r.p];
          expanded.delete(r.p);
        }
      }
      return { children, expanded, rootError };
    });
  },

  select: (path) => set({ selectedPath: path }),
  setCompareSource: (path) => set({ compareSource: path }),

  reset: () =>
    set({ children: {}, expanded: new Set(), loading: new Set(), selectedPath: null, compareSource: null, rootError: null }),
}));
