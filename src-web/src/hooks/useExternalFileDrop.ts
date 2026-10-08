import { useEffect, useRef, useState } from "react";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

/**
 * Receives real files dragged in from an external window (e.g. Windows
 * Explorer) over a single target area -- can't use native HTML5 drag and
 * drop here: Tauri's `dragDropEnabled` defaults to `true`, and on Windows
 * that makes WebView2 swallow HTML5 drag events entirely, so
 * `dataTransfer.files` is always empty. This listens to Tauri's window-level
 * `onDragDropEvent` stream instead and does hit-testing against `targetRef`
 * to figure out whether the pointer is over this particular target area.
 *
 * `event.payload.paths` is an array of disk paths, not browser `File`
 * objects -- the caller still needs to read file contents itself (e.g. via
 * `local_read_binary_preview`/`local_read_file`); this hook only identifies
 * that an external drop landed on its target and hands back the paths.
 */
export function useExternalFileDrop(
  targetRef: React.RefObject<HTMLElement | null>,
  onFilesDropped: (paths: string[]) => void,
) {
  const [isDragOver, setIsDragOver] = useState(false);
  const onFilesDroppedRef = useRef(onFilesDropped);
  onFilesDroppedRef.current = onFilesDropped;

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    (async () => {
      const fn = await getCurrentWebviewWindow().onDragDropEvent((event) => {
        if (event.payload.type === "leave") {
          setIsDragOver(false);
          return;
        }
        const rect = targetRef.current?.getBoundingClientRect();
        if (!rect) {
          setIsDragOver(false);
          return;
        }
        const ratio = window.devicePixelRatio || 1;
        const x = event.payload.position.x / ratio;
        const y = event.payload.position.y / ratio;
        const inside = x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
        if (event.payload.type === "drop") {
          setIsDragOver(false);
          if (inside && event.payload.paths.length > 0) onFilesDroppedRef.current(event.payload.paths);
          return;
        }
        setIsDragOver(inside);
      });
      if (cancelled) fn();
      else unlisten = fn;
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return { isDragOver };
}
