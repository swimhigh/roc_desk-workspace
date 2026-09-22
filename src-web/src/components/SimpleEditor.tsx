import React, { useEffect, useState } from "react";
import { Save } from "lucide-react";
import { localFsService } from "../services";

interface SimpleEditorProps {
  path: string;
}

/**
 * Plain-text file editing (no syntax highlighting) -- see the task's final
 * report for why this doesn't embed `roc_desk-editor`'s Monaco-based
 * `<EditorPane/>`. Good enough for "open a folder, view/edit a file, save
 * it", which is the priority-1 bar for this tool's frontend.
 */
export const SimpleEditor: React.FC<SimpleEditorProps> = ({ path }) => {
  const [text, setText] = useState("");
  const [mtime, setMtime] = useState<number | null>(null);
  const [dirty, setDirty] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    setLoading(true);
    setError(null);
    setDirty(false);
    localFsService
      .readFile(path)
      .then((content) => {
        setText(content.text);
        setMtime(content.mtime);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [path]);

  const save = async () => {
    try {
      const outcome = await localFsService.writeFile(path, text, mtime);
      if (outcome.type === "Written") {
        setMtime(outcome.mtime);
        setDirty(false);
        setStatus("已保存");
        setTimeout(() => setStatus(null), 1500);
      } else {
        setError(`文件已被外部修改（当前内容预览：${outcome.current_preview}），未保存`);
      }
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%" }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "4px 10px", borderBottom: "1px solid var(--border)" }}>
        <span style={{ flex: 1, fontFamily: "var(--font-mono)", fontSize: 12 }}>
          {path}
          {dirty ? " *" : ""}
        </span>
        {status && <span style={{ color: "var(--success)", fontSize: 12 }}>{status}</span>}
        <button className="btn" onClick={() => void save()} disabled={loading}>
          <Save style={{ width: 12, height: 12, marginRight: 4, verticalAlign: -1 }} />
          保存
        </button>
      </div>
      {error && <div className="error-banner">{error}</div>}
      {loading ? (
        <div style={{ padding: 10, color: "var(--text-secondary)" }}>加载中…</div>
      ) : (
        <textarea
          className="editor-textarea"
          value={text}
          spellCheck={false}
          onChange={(e) => {
            setText(e.target.value);
            setDirty(true);
          }}
          onKeyDown={(e) => {
            if ((e.ctrlKey || e.metaKey) && e.key === "s") {
              e.preventDefault();
              void save();
            }
          }}
        />
      )}
    </div>
  );
};
