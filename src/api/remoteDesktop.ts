import { invoke } from "@tauri-apps/api/core";

/** 一个可用于打开远程桌面的系统客户端，kind 是稳定的启动参数选择键。 */
export type RemoteDesktopClient = {
  kind: "mstsc" | "xfreerdp" | "remmina" | "vncviewer";
  name: string;
  path: string;
};

/**
 * 列出本机已安装的远程桌面客户端。
 *
 * 无参数
 * 核心流程：交给后端扫 PATH / 判断 .app 是否存在；一个都没有时后端回空列表，
 * 前端据「空列表」给安装引导，因此这里不需要把空结果当成错误。
 */
export function remoteDesktopClients(): Promise<RemoteDesktopClient[]> {
  return invoke<RemoteDesktopClient[]>("remote_desktop_clients");
}

/**
 * 用系统客户端打开一个远程桌面会话。
 *
 * 参数：kind 为客户端类型，host/port/username 为目标会话信息
 * 核心流程：只转发参数。密码刻意不传：它会出现在同机可见的进程列表里，
 * 改由系统客户端自己弹框询问。
 */
export function launchRemoteDesktop(
  kind: RemoteDesktopClient["kind"],
  host: string,
  port: number,
  username: string,
): Promise<void> {
  return invoke("launch_remote_desktop", { kind, host, port, username });
}
