/** Windows（Agent 目标）路径工具，原样搬自宿主 `src-web/src/utils/windowsPath.ts`
 * ——目标机器没有单一根目录 "/" 的概念，用空字符串表示"此电脑"下的盘符列表这一虚拟
 * 层级，选中某个盘符（如 "C:\\"）之后才是真正的目录路径。 */
export const AGENT_ROOT = "";

export function isAgentRoot(path: string): boolean {
  return path === AGENT_ROOT;
}

export function agentParentPath(path: string): string {
  const trimmed = path.replace(/\\+$/, "");
  const lastSep = trimmed.lastIndexOf("\\");
  if (lastSep <= 2) {
    return AGENT_ROOT;
  }
  return trimmed.slice(0, lastSep);
}
