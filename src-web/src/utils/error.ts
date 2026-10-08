export type AppErrorKind =
  | "Connection"
  | "Auth"
  | "HostKeyRejected"
  | "PermissionDenied"
  | "NotFound"
  | "Database"
  | "Conflict"
  | "Internal";

export interface AppError {
  kind: AppErrorKind;
  message: string;
}

export function isAppError(e: unknown): e is AppError {
  return typeof e === "object" && e !== null && "kind" in e && "message" in e;
}

const KIND_LABEL: Record<string, string> = {
  Connection: "连接失败",
  Auth: "认证失败",
  HostKeyRejected: "主机指纹校验被拒绝",
  PermissionDenied: "权限不足",
  NotFound: "未找到",
  Database: "数据库错误",
  Conflict: "冲突",
  Internal: "内部错误",
};

/**
 * Tauri `invoke()`'s rejection is a serialized backend `AppError`, a plain
 * object `{ kind, message }`, not a JS `Error` instance -- `String(e)`
 * alone would produce "[object Object]". Every catch branch should go
 * through this instead of formatting the error itself.
 */
export function formatError(e: unknown): string {
  if (isAppError(e)) {
    const label = KIND_LABEL[e.kind];
    return label ? `${label}：${e.message}` : e.message;
  }
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}
