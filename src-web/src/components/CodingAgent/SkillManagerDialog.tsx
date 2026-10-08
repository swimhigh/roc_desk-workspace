import React, { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { Trash2, FolderInput, FileArchive } from "lucide-react";
import { ConfirmDialog } from "../shared/ConfirmDialog";
import { useCodingStore } from "../../stores/codingStore";
import { useToastStore } from "../shared/Toast";
import { formatError } from "../../utils/error";

interface SkillManagerDialogProps {
  workspaceId: string;
  onClose: () => void;
}

/** Project Skills view/import -- a skill lives at the workspace's
 * `.rock_desk/skills/<name>/SKILL.md`. "Import" picks a local directory
 * containing `SKILL.md`, or a `.zip`/`.tar.gz`/`.tgz` archive (auto-
 * extracted into a temp dir then imported; RAR is unsupported). Copied
 * into the current workspace via `copy_between` on the backend, so a
 * remote/Agent workspace works too. Re-importing the same name overwrites
 * the old version. */
export const SkillManagerDialog: React.FC<SkillManagerDialogProps> = ({ workspaceId, onClose }) => {
  const { skills, loadSkills, importSkill, deleteSkill } = useCodingStore();
  const [importing, setImporting] = useState(false);
  const push = useToastStore((s) => s.push);

  useEffect(() => {
    loadSkills(workspaceId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workspaceId]);

  const runImport = async (selected: string | string[] | null) => {
    if (!selected || Array.isArray(selected)) return;
    setImporting(true);
    try {
      const skill = await importSkill(workspaceId, selected);
      push("success", `已导入技能：${skill.name}`);
    } catch (e) {
      push("error", `导入失败：${formatError(e)}`);
    } finally {
      setImporting(false);
    }
  };

  const handleImportFolder = async () => runImport(await open({ directory: true, multiple: false }));

  const handleImportArchive = async () =>
    runImport(
      await open({
        directory: false,
        multiple: false,
        filters: [
          { name: "Skills 压缩包 (.zip)", extensions: ["zip"] },
          { name: "Skills 压缩包 (.tar.gz/.tgz)", extensions: ["gz", "tgz"] },
        ],
      }),
    );

  const handleDelete = async (name: string) => {
    try {
      await deleteSkill(workspaceId, name);
      push("success", `已删除技能：${name}`);
    } catch (e) {
      push("error", `删除失败：${formatError(e)}`);
    }
  };

  return (
    <ConfirmDialog
      open
      severity="info"
      icon="🧩"
      title="项目 Skills 管理"
      onDismiss={onClose}
      actions={<button className="btn ghost sm" onClick={onClose}>关闭</button>}
    >
      <p style={{ fontSize: 12, color: "var(--text-secondary)", marginBottom: 10 }}>
        技能存放在工作区 <code>.rock_desk/skills/&lt;name&gt;/SKILL.md</code>，AI 助手会自动发现并在需要时按名加载详细步骤执行；
        可以选一个已解压的文件夹，也可以直接选 .zip/.tar.gz/.tgz 压缩包（自动解压导入，不支持 RAR）。远程/Agent 工作区会拷贝到远端。同名技能重新导入会覆盖旧版本。
      </p>
      <div style={{ maxHeight: 220, overflowY: "auto", marginBottom: 12 }}>
        {skills.length === 0 ? (
          <p style={{ fontSize: 13, color: "var(--text-secondary)" }}>当前工作区还没有技能，点下方"导入文件夹/压缩包"添加一个。</p>
        ) : (
          skills.map((skill) => (
            <div key={skill.name} className="file-row" style={{ gridTemplateColumns: "1fr auto" }}>
              <span>
                {skill.name}
                {skill.description && (
                  <span style={{ color: "var(--text-secondary)", fontSize: 11, marginLeft: 6 }}>
                    {skill.description}
                  </span>
                )}
              </span>
              <button className="btn ghost sm" onClick={() => handleDelete(skill.name)} title="删除">
                <Trash2 style={{ width: 14, height: 14 }} />
              </button>
            </div>
          ))
        )}
      </div>

      <div className="form-actions">
        <button className="btn primary sm" onClick={handleImportFolder} disabled={importing}>
          <FolderInput style={{ width: 14, height: 14 }} /> {importing ? "导入中…" : "导入文件夹"}
        </button>
        <button className="btn ghost sm" onClick={handleImportArchive} disabled={importing}>
          <FileArchive style={{ width: 14, height: 14 }} /> {importing ? "导入中…" : "导入压缩包"}
        </button>
      </div>
    </ConfirmDialog>
  );
};
