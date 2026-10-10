import React, { useState } from "react";
import { Loader2, Pencil, Trash2, X } from "lucide-react";

interface HistorySummaryLike {
  id: string;
  title: string;
  provider_label: string;
  model: string;
  mode?: string;
  updated_at: string;
}

interface Props {
  title: string;
  histories: HistorySummaryLike[];
  emptyText: string;
  onOpen: (id: string) => void | Promise<unknown>;
  onDelete: (id: string) => void;
  onRename: (id: string, title: string) => void;
  onClose: () => void;
  /** 正在打开中的历史记录 id——`timeline`/`changes`/`messages` 只存在工作区
   * 目录里，点开一条历史时每次都要去现读（本地工作区走本地磁盘，远程工作区
   * 走 SSH），用它在对应行上显示"正在打开"，而不是让弹窗看起来卡住没反应。 */
  openingId?: string | null;
}

export const CodingHistoryDialog: React.FC<Props> = ({ title, histories, emptyText, onOpen, onDelete, onRename, onClose, openingId }) => {
  const [editingId, setEditingId] = useState<string | null>(null);
  const [renameText, setRenameText] = useState("");
  const submit = (id: string) => {
    if (renameText.trim()) onRename(id, renameText.trim());
    setEditingId(null);
  };
  const opening = Boolean(openingId);
  return <div className="coding-history-overlay" onClick={onClose}>
    <div className="coding-history-dialog" onClick={(event) => event.stopPropagation()}>
      <div className="coding-history-title"><span>{title}</span><button className="btn ghost sm" onClick={onClose}><X /></button></div>
      {histories.length === 0 ? <div className="coding-history-empty">{emptyText}</div> : <div className="coding-history-list">
        {histories.map((item) => <div className="coding-history-row" key={item.id}>
          {editingId === item.id ? <input className="form-input coding-history-rename" autoFocus value={renameText} onChange={(event) => setRenameText(event.target.value)} onBlur={() => submit(item.id)} onKeyDown={(event) => { if (event.key === "Enter") submit(item.id); if (event.key === "Escape") setEditingId(null); }} /> : <button className="coding-history-open" disabled={opening} onClick={() => onOpen(item.id)}><strong>{item.title}</strong><span>{item.provider_label} · {item.model || "未知模型"}{item.mode ? ` · ${item.mode}` : ""}</span>{openingId === item.id ? <span className="coding-history-opening"><Loader2 className="spin" /> 正在打开…</span> : <time>{new Date(item.updated_at).toLocaleString()}</time>}</button>}
          <button className="btn ghost sm" title="重命名" disabled={opening} onMouseDown={(event) => event.preventDefault()} onClick={() => { setEditingId(item.id); setRenameText(item.title); }}><Pencil /></button>
          <button className="btn ghost sm" title="删除历史" disabled={opening} onClick={() => onDelete(item.id)}><Trash2 /></button>
        </div>)}
      </div>}
    </div>
  </div>;
};
