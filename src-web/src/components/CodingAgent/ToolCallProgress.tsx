import React from "react";
import { Check, ChevronDown, ChevronRight } from "lucide-react";

interface ToolCallProgressProps {
  tool: string;
  elapsedMs: number;
  done?: boolean;
  detail?: string | null;
  onOpenFile?: () => void;
  output?: string | null;
  expanded?: boolean;
  onToggleOutput?: () => void;
}

export function toolLabel(tool: string): string {
  const labels: Record<string, string> = {
    read_file: "读取文件", write_file: "创建文件变更", edit_file: "编辑文件",
    list_directory: "查看目录", search_files: "搜索代码", run_command: "执行命令",
    glob: "按文件名查找", webfetch: "抓取网页", todo_write: "更新任务清单",
    question: "向你提问", skill: "加载技能",
    multi_edit: "批量编辑文件", git_status: "查看 Git 状态", git_diff: "查看 Git 改动",
    git_commit: "Git 提交", run_command_background: "后台启动命令",
    read_background_output: "查看后台输出", stop_background_process: "结束后台进程",
    find_definition: "查找定义", task: "委派子任务",
  };
  if (tool.startsWith("mcp__")) {
    const [, server, name] = tool.split("__");
    return `调用 MCP 工具 ${server ?? ""}.${name ?? tool}`;
  }
  return labels[tool] ?? `执行 ${tool}`;
}

/** Tool-call elapsed-time feedback. */
export const ToolCallProgress: React.FC<ToolCallProgressProps> = ({
  tool,
  elapsedMs,
  done,
  detail,
  onOpenFile,
  output,
  expanded,
  onToggleOutput,
}) => {
  const isCommand = tool === "run_command" || tool === "run_query";
  const canExpand = Boolean(done && output && onToggleOutput);
  const visibleOutput = output && output.length > 12000
    ? `${output.slice(0, 12000)}\n…（结果过长，已折叠 ${output.length - 12000} 个字符）`
    : output;
  return (
    <div>
      <div
        className="tool-call-progress"
        style={canExpand ? { cursor: "pointer" } : undefined}
        onClick={canExpand ? onToggleOutput : undefined}
        title={canExpand ? (expanded ? "点击收起执行结果" : "点击查看执行结果") : undefined}
      >
        {done ? <Check style={{ width: 10, height: 10, color: "var(--text-secondary)" }} /> : <span className="spinner" />}
        {done ? `已完成 ${toolLabel(tool)}` : `正在${toolLabel(tool)} · ${(elapsedMs / 1000).toFixed(1)}s`}
        {detail && (
          onOpenFile ? (
            <button className="tool-file-ref" onClick={(e) => { e.stopPropagation(); onOpenFile(); }}>· {detail}</button>
          ) : isCommand ? (
            <code className="tool-detail-cmd">{detail}</code>
          ) : (
            <span style={{ opacity: 0.7 }}>· {detail}</span>
          )
        )}
        {canExpand && (
          expanded
            ? <ChevronDown style={{ width: 12, height: 12, color: "var(--text-secondary)", marginLeft: "auto", flexShrink: 0 }} />
            : <ChevronRight style={{ width: 12, height: 12, color: "var(--text-secondary)", marginLeft: "auto", flexShrink: 0 }} />
        )}
      </div>
      {expanded && visibleOutput && (
        <pre className="tool-output-pane">{visibleOutput}</pre>
      )}
    </div>
  );
};
