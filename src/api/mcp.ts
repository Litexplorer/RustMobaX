import { invoke } from "@tauri-apps/api/core";

/**
 * MCP 服务的运行状态。
 *
 * 无参数
 * 核心流程：字段与后端 McpStatusView 一一对应（后端按 camelCase 序列化），
 * addr 是 IPv4 回环地址加端口（形如 127.0.0.1:47821），未运行时为 null；
 * token 是当前生效的 Bearer 令牌，lastError 是最近一次启动失败的原因。
 */
export interface McpStatus {
  enabled: boolean;
  port: number;
  running: boolean;
  addr: string | null;
  token: string | null;
  lastError: string | null;
}

/**
 * 查询 MCP 服务状态。
 *
 * 参数：无
 * 核心流程：直接调 mcp_status；前端没有事件订阅，状态靠「打开弹窗时拉一次 +
 * 改完设置或重置令牌后再拉一次」保持新鲜。
 */
export function mcpStatus(): Promise<McpStatus> {
  return invoke<McpStatus>("mcp_status");
}

/**
 * 重置 MCP 访问令牌。
 *
 * 参数：无
 * 核心流程：后端生成新令牌写回设置并按当前配置重启服务，旧令牌立刻失效；
 * 失败时抛出中文错误，由调用方展示。
 */
export function mcpResetToken(): Promise<null> {
  return invoke<null>("mcp_reset_token");
}

/**
 * 按当前设置重新应用一次 MCP 配置。
 *
 * 参数：无
 * 核心流程：转交 mcp_apply，返回 null；失败时同样抛中文错误。
 */
export function mcpApply(): Promise<null> {
  return invoke<null>("mcp_apply");
}

/**
 * 一条 MCP 工具调用审计。
 *
 * 无参数
 * 核心流程：字段与后端 McpAuditEntry 一一对应（后端按 camelCase 序列化）；
 * argsDigest 只含键名与字节数，不含任何参数原文；error 只在失败时非空。
 */
export interface McpAuditEntry {
  id: number;
  ts: number;
  tool: string;
  argsDigest: string;
  ok: boolean;
  error: string | null;
  durationMs: number;
}

/**
 * 查询最近的 MCP 工具调用审计。
 *
 * 参数：limit 为最多返回条数，缺省 20，后端会再夹到 500 以内
 * 核心流程：直接调 mcp_audit，新的在前；设置页打开时与点「刷新」时各拉一次。
 */
export function mcpAudit(limit = 20): Promise<McpAuditEntry[]> {
  return invoke<McpAuditEntry[]>("mcp_audit", { limit });
}
