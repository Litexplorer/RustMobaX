import { invoke } from "@tauri-apps/api/core";

/** 后端已支持的协议；ssh/sftp/local/telnet/wsl 都有终端入口，rdp/vnc/serial 暂时只在数据层可用。 */
export type Protocol = "ssh" | "sftp" | "local" | "telnet" | "rdp" | "vnc" | "serial" | "wsl";
export type AuthKind = "password" | "key" | "agent";

export type Session = {
  id: string;
  groupId: string | null;
  name: string;
  protocol: Protocol;
  host: string | null;
  port: number | null;
  username: string | null;
  authKind: AuthKind;
  extra: string | null;
  createdAt: number;
  updatedAt: number;
};

export type SessionGroup = {
  id: string;
  parentId: string | null;
  name: string;
  sortOrder: number;
};

export type SessionInput = {
  name: string;
  groupId: string | null;
  protocol: Protocol;
  host: string | null;
  port: number | null;
  username: string | null;
  authKind: AuthKind;
  extra: string | null;
};

export type SkippedEntry = { section: string; reason: string };

export type PendingSession = {
  name: string;
  groupPath: string[];
  protocol: Protocol;
  host: string | null;
  port: number | null;
  username: string | null;
  authKind: AuthKind;
};

export type ImportPreview = {
  sessions: PendingSession[];
  skipped: SkippedEntry[];
};

export type ImportReport = {
  imported: number;
  groupsCreated: number;
  skipped: SkippedEntry[];
};

export type ApiError = { kind: string; message: string; detail?: unknown };

/**
 * 调后端命令。
 *
 * 参数：command 为命令名，args 为参数
 * 核心流程：统一走 invoke，失败时由调用方的 errorMessage 归一成文案。
 */
function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(command, args);
}

/**
 * 从任意异常里提取用户可读文案。
 *
 * 参数：err 为 invoke 抛出的错误
 * 核心流程：后端错误是 {kind,message} 对象，非对象则直接字符串化。
 */
export function errorMessage(err: unknown): string {
  if (typeof err === "object" && err !== null && "message" in err) {
    return String((err as ApiError).message);
  }
  return String(err);
}

/**
 * 取后端错误的分类标识。
 *
 * 参数：err 为 invoke 抛出的错误
 * 核心流程：前端按 kind 决定是否需要弹指纹确认之类的专门交互。
 */
export function errorKind(err: unknown): string | null {
  if (typeof err === "object" && err !== null && "kind" in err) {
    return String((err as ApiError).kind);
  }
  return null;
}

/**
 * 取后端错误携带的结构化附加信息。
 *
 * 参数：err 为 invoke 抛出的错误
 * 核心流程：目前用于主机指纹（host/port/sha256/md5）。
 */
export function errorDetail<T>(err: unknown): T | null {
  if (typeof err === "object" && err !== null && "detail" in err) {
    return ((err as ApiError).detail as T) ?? null;
  }
  return null;
}

export const api = {
  listGroups: () => call<SessionGroup[]>("list_groups"),
  createGroup: (name: string, parentId: string | null) =>
    call<SessionGroup>("create_group", { name, parentId }),
  renameGroup: (id: string, name: string) =>
    call<void>("rename_group", { id, name }),
  deleteGroup: (id: string) => call<void>("delete_group", { id }),
  moveGroup: (id: string, parentId: string | null) =>
    call<void>("move_group", { id, parentId }),

  listSessions: (keyword: string) => call<Session[]>("list_sessions", { keyword }),
  createSession: (input: SessionInput) => call<Session>("create_session", { input }),
  updateSession: (id: string, input: SessionInput) =>
    call<Session>("update_session", { id, input }),
  deleteSession: (id: string) => call<void>("delete_session", { id }),
  moveSession: (id: string, groupId: string | null) =>
    call<void>("move_session", { id, groupId }),
  copySession: (id: string) => call<Session>("copy_session", { id }),

  previewImport: (content: string) =>
    call<ImportPreview>("preview_import", { content }),
  applyImport: (preview: ImportPreview) =>
    call<ImportReport>("apply_import_preview", { preview }),
  exportSessions: () => call<string>("export_sessions"),

  readTextFile: (path: string) => call<string>("read_text_file", { path }),
  writeTextFile: (path: string, content: string) =>
    call<void>("write_text_file", { path, content }),
};
