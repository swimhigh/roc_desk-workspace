import React, { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { listen } from "@tauri-apps/api/event";
import "@xterm/xterm/css/xterm.css";
import { ptyService } from "../services";

interface TerminalPanelProps {
  cwd: string;
}

interface PtyDataEvent {
  channelId: string;
  data: number[];
}
interface PtyStatusEvent {
  channelId: string;
  status: string;
}

/**
 * Local terminal backed by `pty_open`/`pty_write`/`pty_resize`/`pty_close`
 * (ported 1:1 from the host's PTY manager -- see `lib/src/pty.rs`).
 * Deliberately a much simpler component than the host's `TerminalView.tsx`:
 * no theme store, no reconnect UI, no multi-exec fan-out -- just "one local
 * shell in the workspace root".
 */
export const TerminalPanel: React.FC<TerminalPanelProps> = ({ cwd }) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const channelIdRef = useRef<string | null>(null);

  useEffect(() => {
    if (!containerRef.current) return;
    let disposed = false;
    let term: Terminal | null = null;
    let fitAddon: FitAddon | null = null;
    let resizeObserver: ResizeObserver | null = null;
    let unlistenData: (() => void) | null = null;
    let unlistenStatus: (() => void) | null = null;

    const setup = async () => {
      term = new Terminal({
        fontFamily: "'Cascadia Mono', Consolas, monospace",
        fontSize: 13,
        cursorBlink: true,
        scrollback: 5000,
        theme: { background: "#1e1e1e", foreground: "#e6e6e6" },
      });
      fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      if (!containerRef.current || disposed) return;
      term.open(containerRef.current);
      fitAddon.fit();

      const channelId = await ptyService.open(cwd, term.rows, term.cols);
      if (disposed) {
        void ptyService.close(channelId);
        return;
      }
      channelIdRef.current = channelId;

      term.onData((data) => {
        void ptyService.write(channelId, new TextEncoder().encode(data));
      });

      resizeObserver = new ResizeObserver(() => {
        if (!term || !fitAddon) return;
        fitAddon.fit();
        void ptyService.resize(channelId, term.rows, term.cols);
      });
      resizeObserver.observe(containerRef.current);

      const decoder = new TextDecoder();
      unlistenData = await listen<PtyDataEvent>("pty:data", (event) => {
        if (event.payload.channelId !== channelId || !term) return;
        const bytes = new Uint8Array(event.payload.data);
        term.write(decoder.decode(bytes, { stream: true }));
      }).then((fn) => fn);

      unlistenStatus = await listen<PtyStatusEvent>("pty:status", (event) => {
        if (event.payload.channelId !== channelId || !term) return;
        if (event.payload.status === "disconnected") {
          term.write("\r\n\x1b[31m[终端已退出]\x1b[0m\r\n");
        }
      }).then((fn) => fn);
    };

    void setup();

    return () => {
      disposed = true;
      resizeObserver?.disconnect();
      unlistenData?.();
      unlistenStatus?.();
      term?.dispose();
      if (channelIdRef.current) void ptyService.close(channelIdRef.current);
      channelIdRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cwd]);

  return <div ref={containerRef} style={{ width: "100%", height: "100%", padding: 6, boxSizing: "border-box" }} />;
};
