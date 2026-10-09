import React, { useEffect, useState } from "react";
import { TerminalView, type TerminalTab, connectionService, sshService, agentService } from "@roc_desk/tool-ssh";
import { formatError } from "../utils/error";

interface RemoteTerminalPanelProps {
  connectionId: string;
  cwd: string;
}

/**
 * 远程工作区的"终端"标签页——`@roc_desk/tool-ssh` 的 `TerminalView` 只负责
 * 渲染一个已经打开的 shell 通道（`tab.id` 就是 `openShell` 返回的
 * channelId），这里先查一下连接档案的协议（SSH 走 `sshService`，Agent 走
 * `agentService`，两边的 Tauri 命令不同），再调一次对应的 `openShell`，拿到
 * channelId 之后才挂载 `TerminalView`；卸载时调 `closeChannel` 收尾。
 */
export const RemoteTerminalPanel: React.FC<RemoteTerminalPanelProps> = ({ connectionId, cwd }) => {
  const [tab, setTab] = useState<TerminalTab | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    let openedChannelId: string | null = null;
    let openedKind: "ssh" | "agent" | null = null;

    const setup = async () => {
      try {
        const profiles = await connectionService.list();
        const profile = profiles.find((p) => p.id === connectionId);
        const kind: "ssh" | "agent" = profile?.protocol === "agent" ? "agent" : "ssh";
        const channelId =
          kind === "ssh"
            ? await sshService.openShell(connectionId, 24, 80, cwd)
            : await agentService.openShell(connectionId, 24, 80, cwd);
        if (disposed) {
          void (kind === "ssh" ? sshService.closeChannel(connectionId, channelId) : agentService.closeChannel(connectionId, channelId));
          return;
        }
        openedChannelId = channelId;
        openedKind = kind;
        setTab({ id: channelId, kind, profileId: connectionId, title: "terminal" });
      } catch (e) {
        if (!disposed) setError(formatError(e));
      }
    };
    void setup();

    return () => {
      disposed = true;
      if (openedChannelId && openedKind) {
        void (openedKind === "ssh"
          ? sshService.closeChannel(connectionId, openedChannelId)
          : agentService.closeChannel(connectionId, openedChannelId));
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [connectionId, cwd]);

  if (error) {
    return <div style={{ padding: 12, fontSize: 12, color: "var(--danger)" }}>打开终端失败：{error}</div>;
  }
  if (!tab) {
    return <div style={{ padding: 12, fontSize: 12, color: "var(--text-secondary)" }}>正在连接…</div>;
  }
  return <TerminalView tab={tab} cwd={cwd} />;
};
