import type { Protocol } from "../api/session";

/**
 * 协议分组表：界面里所有「按协议分支」的判断都只认这里。
 *
 * 无参数
 * 核心流程：三张表描述三类协议的实际能力，下面的判定函数是它们的唯一读法；
 * 新增协议时只改这三张表和对应判定，不用再去 App / 各个组件里逐个补白名单。
 */

/** 能开终端标签页、并且真的会起一个终端的协议（TerminalPanel 的启动表按它对齐）。 */
export const TERMINAL_PROTOCOLS: Protocol[] = ["local", "ssh", "telnet", "wsl", "serial"];

/** 连远端 shell 的协议：需要认证方式、能挂 SFTP 面板、能开隧道。 */
export const SSH_LIKE_PROTOCOLS: Protocol[] = ["ssh", "sftp"];

/** 远程桌面协议：不自研渲染，连接交给本机已安装的客户端。 */
export const REMOTE_DESKTOP_PROTOCOLS: Protocol[] = ["rdp", "vnc"];

/**
 * 该协议是否有终端面板（开标签页 + 渲染 xterm）。
 *
 * 参数：protocol 为会话协议
 * 核心流程：查 TERMINAL_PROTOCOLS，不匹配的协议（含 SFTP）不开终端。
 */
export function isTerminalProtocol(protocol: Protocol): boolean {
  return TERMINAL_PROTOCOLS.includes(protocol);
}

/**
 * 该协议是否是 SSH 家族（ssh / sftp）。
 *
 * 参数：protocol 为会话协议
 * 核心流程：查 SSH_LIKE_PROTOCOLS；认证、SFTP 挂载、隧道候选都按这个判定。
 */
export function isSshLike(protocol: Protocol): boolean {
  return SSH_LIKE_PROTOCOLS.includes(protocol);
}

/**
 * 该协议是否是远程桌面（rdp / vnc）。
 *
 * 参数：protocol 为会话协议
 * 核心流程：查 REMOTE_DESKTOP_PROTOCOLS，只有详情页与客户端检测需要它。
 */
export function isRemoteDesktop(protocol: Protocol): boolean {
  return REMOTE_DESKTOP_PROTOCOLS.includes(protocol);
}
