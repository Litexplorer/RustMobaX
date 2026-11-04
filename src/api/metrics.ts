import { invoke } from "@tauri-apps/api/core";

export type SystemMetrics = {
  /** CPU 使用率，0 到 100 */
  cpuPercent: number;
  /** 内存使用率，0 到 100 */
  memPercent: number;
  /** 已用内存（MiB） */
  memUsedMb: number;
  /** 总内存（MiB） */
  memTotalMb: number;
  /** 下行速率（KB/s） */
  rxKbps: number;
  /** 上行速率（KB/s） */
  txKbps: number;
  /** 当前平台是否支持采样 */
  supported: boolean;
};

/**
 * 单块 GPU 的采样结果。
 *
 * measurable 为 false 表示只认出了型号（驱动没装、nouveau 不给计数），
 * 此时利用率/显存/温度都是 0，前端只展示型号，绝不能把这些 0 当读数画出来；
 * measurable 为 true 时取不到的数值才退化成 0、温度取不到为 null。
 */
export type GpuMetrics = {
  /** GPU 名称，取不到时为 "GPU" */
  name: string;
  /** 利用率，0 到 100 */
  utilPercent: number;
  /** 已用显存（MiB），取不到为 0 */
  memUsedMb: number;
  /** 显存总量（MiB），取不到为 0 */
  memTotalMb: number;
  /** 温度（摄氏度），取不到为 null */
  tempC: number | null;
  /** 能否采到指标；false 表示只知道型号，数值字段不可信 */
  measurable: boolean;
};

/**
 * 远端主机指标。
 *
 * metrics 的字段口径与本机完全一致，host 是被采样的主机地址，
 * scope 由后端固定成 "remote"，前端靠它标注数据来自哪台机器；
 * gpus 在没有 GPU 或采集失败时是空数组（不是 null），直接遍历即可。
 */
export type RemoteMetrics = {
  metrics: SystemMetrics;
  gpus: GpuMetrics[];
  host: string;
  scope: "remote";
};

/**
 * 指标来源口径。
 *
 * remote：数据来自当前活动 SSH 会话指向的远端主机
 * local：数据来自本机（本地终端或没有活动标签时的兜底）
 * unavailable：远端采样失败，既没有远端数字也没有回落到本机
 */
export type MetricsScope = "remote" | "local" | "unavailable";

/**
 * 采一次本机系统指标。
 *
 * 参数：无
 * 核心流程：后端保留上次基线算差值，因此调用间隔越稳定速率越准；
 * 前端按固定间隔轮询即可，不需要在后端常驻线程。
 */
export function systemMetrics(): Promise<SystemMetrics> {
  return invoke<SystemMetrics>("system_metrics");
}

/**
 * 采一次远端主机指标。
 *
 * 参数：terminalId 为 SSH 终端（也就是会话）的标识
 * 核心流程：后端从终端注册表取该会话的 SSH 连接并在上面采样；
 * 本地终端、终端已关闭、远端没有 /proc 都会抛错，前端按「不可用」处理，
 * 绝不把本机数字当成远端数字展示。
 */
export function remoteMetrics(terminalId: string): Promise<RemoteMetrics> {
  return invoke<RemoteMetrics>("remote_metrics", { terminalId });
}

/**
 * 把过长的地址省略中间，保留首尾便于辨认。
 *
 * 参数：host 为原始地址，max 为期望的最大字符数
 * 核心流程：不超长原样返回；超长时留前半段与后半段，中间用省略号替代，
 * 完整地址由调用方放进 title 供悬停查看。
 */
export function shortenAddress(host: string, max = 18): string {
  if (host.length <= max) return host;
  if (max <= 3) return host.slice(0, max);
  const head = Math.ceil((max - 1) / 2);
  const tail = max - 1 - head;
  return `${host.slice(0, head)}…${host.slice(host.length - tail)}`;
}

/**
 * 把 KB/s 格式化成紧凑文案。
 *
 * 参数：kbps 为每秒千字节数
 * 核心流程：小于 1000 显示 KB/s，超过按 MB/s 保留一位小数，
 * 让状态栏芯片宽度保持稳定。
 */
export function formatRate(kbps: number): string {
  if (!Number.isFinite(kbps) || kbps <= 0) return "0 KB/s";
  if (kbps >= 1024) return `${(kbps / 1024).toFixed(1)} MB/s`;
  return `${Math.round(kbps)} KB/s`;
}

/**
 * 把 MiB 格式化成 GB 或 MB。
 *
 * 参数：mb 为兆字节数
 * 核心流程：超过 1024 MB 用 GB 表示，保留一位小数。
 */
export function formatMemory(mb: number): string {
  if (!Number.isFinite(mb) || mb <= 0) return "0 MB";
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} GB`;
  return `${Math.round(mb)} MB`;
}
