import { invoke } from "@tauri-apps/api/core";

/** 转发方向：本地转发 / 远程转发 / 动态 SOCKS5。后端与前端共用这一套命名。 */
export type TunnelKindName = "local" | "remote" | "dynamic";

/**
 * 一条隧道的持久化配置。
 *
 * id 是稳定标识，运行态里的 TunnelInfo.id 与它一致，
 * 因此前端可以用 id 直接把「配置」与「是否在跑」拼在一起。
 */
export interface TunnelConfig {
  id: string;
  /** 人可读名称，允许为空字符串（后端可自动生成） */
  name: string;
  /** 作为隧道源的 SSH 会话 */
  sessionId: string;
  kind: TunnelKindName;
  /** 空串表示使用默认 127.0.0.1 */
  listenHost: string;
  /** 0 表示由系统自动分配 */
  listenPort: number;
  /** dynamic 类型可为空 */
  targetHost: string;
  /** dynamic 类型可为 0 */
  targetPort: number;
  /** 启动应用时是否自动拉起这条隧道（老数据缺该字段时后端按 false 处理） */
  autoStart: boolean;
  createdAt: number;
  updatedAt: number;
}

/** 新建 / 更新配置时提交的字段，不含后端维护的 id 与时间戳。 */
export interface TunnelConfigInput {
  name: string;
  sessionId: string;
  kind: TunnelKindName;
  listenHost: string;
  listenPort: number;
  targetHost: string;
  targetPort: number;
  /** 启动应用时是否自动拉起；不传时后端按 false 处理 */
  autoStart: boolean;
}

/** 一条隧道的运行时信息，字段与后端 TunnelInfo 一一对应。 */
export interface TunnelInfo {
  /** 与配置 id 相同 */
  id: string;
  name: string;
  kind: TunnelKindName;
  listenHost: string;
  /** 实际生效的监听端口：请求 0 时由系统或远端分配，回填在这里 */
  listenPort: number;
  targetHost: string;
  targetPort: number;
  running: boolean;
}

/**
 * 列出全部隧道配置。
 *
 * 参数：无
 * 核心流程：只读库里的配置，不代表已经在跑；运行态另由 tunnelList 提供。
 */
export function tunnelConfigList(): Promise<TunnelConfig[]> {
  return invoke<TunnelConfig[]>("tunnel_config_list");
}

/**
 * 新建一条隧道配置。
 *
 * 参数：input 为界面上的字段
 * 核心流程：后端落库并回填 id / 时间戳，前端一律以返回值为准。
 */
export function tunnelConfigCreate(input: TunnelConfigInput): Promise<TunnelConfig> {
  return invoke<TunnelConfig>("tunnel_config_create", { input });
}

/**
 * 更新一条隧道配置。
 *
 * 参数：id 为配置标识，input 为新的完整字段
 * 核心流程：整条覆盖，未给出的字段按默认值处理；
 * 已在运行的隧道是否需要重启由调用方决定。
 */
export function tunnelConfigUpdate(id: string, input: TunnelConfigInput): Promise<TunnelConfig> {
  return invoke<TunnelConfig>("tunnel_config_update", { id, input });
}

/**
 * 删除一条隧道配置。
 *
 * 参数：id 为配置标识
 * 核心流程：只删配置，不影响已经在跑的隧道，调用方需先 tunnelStop。
 */
export function tunnelConfigDelete(id: string): Promise<void> {
  return invoke("tunnel_config_delete", { id });
}

/**
 * 按配置启动一条隧道。
 *
 * 参数：id 为配置标识（不再传完整连接参数，主机与认证由后端按 sessionId 解析）
 * 核心流程：认证、建连、开始监听都在后端完成；返回值里的 listenPort
 * 是实际生效的端口，前端一律以它为准显示；失败原因原样透出给界面。
 */
export function tunnelStart(id: string): Promise<TunnelInfo> {
  return invoke<TunnelInfo>("tunnel_start", { id });
}

/**
 * 停止一条隧道。
 *
 * 参数：id 为配置标识
 * 核心流程：后端会撤销转发并断开连接，未命中同样返回成功，
 * 因此前端不必先判断是否存在。
 */
export function tunnelStop(id: string): Promise<void> {
  return invoke("tunnel_stop", { id });
}

/**
 * 列出当前运行态。
 *
 * 参数：无
 * 核心流程：运行状态由后端实时取自句柄，连接被服务端断开时这里能看出来；
 * 配置已删除但仍在跑的项也会出现在结果里。
 */
export function tunnelList(): Promise<TunnelInfo[]> {
  return invoke<TunnelInfo[]>("tunnel_list");
}
