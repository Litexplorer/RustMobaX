import { Channel, invoke } from "@tauri-apps/api/core";

export type TerminalEvent =
  | { kind: "data"; chunk: string }
  | { kind: "exit"; code: number | null };

/**
 * 把 UTF-8 文本编码成 base64。
 *
 * 参数：text 为待编码文本
 * 核心流程：先转字节再分块拼接，避免大段输出时一次性展开参数导致爆栈。
 */
export function textToBase64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  const chunkSize = 0x8000;
  for (let index = 0; index < bytes.length; index += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunkSize));
  }
  return btoa(binary);
}

/**
 * 把 base64 解码成字节。
 *
 * 参数：encoded 为 base64 文本
 * 核心流程：逐字节写进 Uint8Array，直接交给 xterm 渲染，绕开字符串转换。
 */
export function base64ToBytes(encoded: string): Uint8Array {
  const binary = atob(encoded);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

/**
 * 启动本地终端。
 *
 * 参数：cols/rows 为初始尺寸，onEvent 为数据与退出事件回调
 * 核心流程：建立 Tauri Channel → 调后端开 PTY → 返回终端标识。
 */
export function startLocalTerminal(
  cols: number,
  rows: number,
  onEvent: (event: TerminalEvent) => void,
): Promise<string> {
  const channel = new Channel<TerminalEvent>();
  channel.onmessage = onEvent;
  return invoke<string>("start_local_terminal", {
    request: { cols, rows },
    onEvent: channel,
  });
}

/**
 * 写入用户输入。
 *
 * 参数：id 为终端标识，data 为 base64 字节
 * 核心流程：只做转发，编码在前端完成以减少 Rust 侧字符串处理。
 */
export function writeLocalTerminal(id: string, data: string): Promise<void> {
  return invoke("write_local_terminal", { id, data });
}

/**
 * 调整终端尺寸。
 *
 * 参数：id 为终端标识，cols/rows 为新尺寸
 * 核心流程：由 shell 收到 SIGWINCH 后自行重排。
 */
export function resizeLocalTerminal(id: string, cols: number, rows: number): Promise<void> {
  return invoke("resize_local_terminal", { id, cols, rows });
}

/**
 * 关闭终端。
 *
 * 参数：id 为终端标识
 * 核心流程：后端会杀掉子进程；重复调用不报错。
 */
export function closeLocalTerminal(id: string): Promise<void> {
  return invoke("close_local_terminal", { id });
}

/**
 * 查终端当前的本地工作目录。
 *
 * 参数：id 为终端标识
 * 核心流程：本地 PTY 由后端读子进程的 cwd；远端会话没有本地目录，
 * 目录由远端 shell 自己用 OSC 7 上报，所以这里通常返回 null。
 * 拿不到（进程已退出、平台不支持）就是 null，不抛错。
 */
export function terminalCwd(id: string): Promise<string | null> {
  return invoke<string | null>("terminal_cwd", { terminalId: id });
}

export type SshAuthRequest =
  | { kind: "password"; password: string }
  | { kind: "privateKey"; path: string; passphrase?: string | null }
  /** 只给凭据库里的标识，明文由后端在连接前解出。 */
  | { kind: "credential"; id: string };

export type SshStartRequest = {
  host: string;
  port: number;
  username: string;
  auth: SshAuthRequest;
  cols: number;
  rows: number;
  /** 用户已确认过该主机指纹。 */
  acceptUnknownHostKey: boolean;
};

/**
 * 启动 SSH 终端。
 *
 * 参数：request 为连接参数，onEvent 为数据与退出事件回调
 * 核心流程：与本地终端同一条事件通道，只是后端换成 SSH 会话。
 */
export function startSshTerminal(
  request: SshStartRequest,
  onEvent: (event: TerminalEvent) => void,
): Promise<string> {
  const channel = new Channel<TerminalEvent>();
  channel.onmessage = onEvent;
  return invoke<string>("start_ssh_terminal", { request, onEvent: channel });
}

export type WslStartRequest = {
  /** 发行版名，空串表示用 WSL 默认发行版（后端此时不传 -d）。 */
  distro: string;
  cols: number;
  rows: number;
};

/**
 * 启动 WSL 终端。
 *
 * 参数：request 为发行版与尺寸，onEvent 为数据与退出事件回调
 * 核心流程：与本地终端同一条事件通道，后端只是把 shell 换成
 * `wsl.exe -d <发行版> --cd ~`，PTY 与输出泵完全复用。
 */
export function startWslTerminal(
  request: WslStartRequest,
  onEvent: (event: TerminalEvent) => void,
): Promise<string> {
  const channel = new Channel<TerminalEvent>();
  channel.onmessage = onEvent;
  return invoke<string>("start_wsl_terminal", { request, onEvent: channel });
}

/**
 * 列出本机已安装的 WSL 发行版。
 *
 * 参数：无
 * 核心流程：后端起 `wsl.exe -l -q` 解析；非 Windows 或未安装 WSL 时
 * 返回空列表（不是错误），前端据空列表给出安装引导。
 */
export function wslListDistros(): Promise<string[]> {
  return invoke<string[]>("wsl_list_distros");
}

export type TelnetStartRequest = {
  host: string;
  port: number;
  cols: number;
  rows: number;
  /** 终端类型，给后续 TERMINAL-TYPE 协商预留；留空由后端取默认值。 */
  term?: string | null;
};

/**
 * 启动 Telnet 终端。
 *
 * 参数：request 为地址与尺寸，onEvent 为数据与退出事件回调
 * 核心流程：与本地终端同一条事件通道，后端换成裸 TCP 直通的 Telnet 会话
 * （这一步还不做 IAC 协商）。
 */
export function startTelnetTerminal(
  request: TelnetStartRequest,
  onEvent: (event: TerminalEvent) => void,
): Promise<string> {
  const channel = new Channel<TerminalEvent>();
  channel.onmessage = onEvent;
  return invoke<string>("start_telnet_terminal", { request, onEvent: channel });
}

/** 一台本机串口设备：path 可直接交给打开接口，kind 只是粗分类（可能为空串）。 */
export type SerialPortInfo = {
  path: string;
  /** "usb" | "pci" | "bluetooth" | ""（识别不出来时为空串） */
  kind: string;
};

export type SerialStartRequest = {
  /** 设备路径，例如 /dev/ttyUSB0、COM3 */
  path: string;
  /** 以下线路参数都可省略，后端按 9600-8-N-1 无流控打开 */
  baudRate?: number | null;
  dataBits?: number | null;
  /** 1 / 1.5 / 2；1.5 目前没有底层枚举，后端会回「当前平台不支持停止位 1.5」 */
  stopBits?: number | null;
  /** "none" | "odd" | "even" */
  parity?: string | null;
  /** "none" | "software" | "hardware" */
  flowControl?: string | null;
};

/** 串口线路参数默认值：与核心层 `SerialOptions::defaults` 保持一致。 */
export const SERIAL_DEFAULTS = {
  baudRate: 9600,
  dataBits: 8,
  stopBits: 1,
  parity: "none",
  flowControl: "none",
} as const;

/** 波特率上限：与核心层 `MAX_BAUD_RATE` 保持一致，用来挡住明显手滑的输入。 */
export const SERIAL_MAX_BAUD_RATE = 4_000_000;

/** 下拉里列出的常用波特率（同时允许手填其它正整数）。 */
export const SERIAL_BAUD_RATES = [
  300, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600,
] as const;

/**
 * 列出本机串口设备。
 *
 * 参数：无
 * 核心流程：后端在阻塞线程里枚举；查不到设备时返回空数组（不是错误），
 * 前端据空数组给出「未检测到串口设备」的提示。
 */
export function serialListPorts(): Promise<SerialPortInfo[]> {
  return invoke<SerialPortInfo[]>("serial_list_ports");
}

/**
 * 启动串口终端。
 *
 * 参数：request 为设备路径与可选线路参数，onEvent 为数据与退出事件回调
 * 核心流程：与 Telnet 同一条事件通道；后端打开串口后起读线程把字节推进输出泵。
 * 串口没有窗口协商，前端后续的 resize 请求在核心层是空操作。
 */
export function startSerialTerminal(
  request: SerialStartRequest,
  onEvent: (event: TerminalEvent) => void,
): Promise<string> {
  const channel = new Channel<TerminalEvent>();
  channel.onmessage = onEvent;
  return invoke<string>("start_serial_terminal", { request, onEvent: channel });
}

export type HostKeyInfo = {
  host: string;
  port: number;
  keyType: string;
  sha256: string;
  md5: string;
};

/**
 * 信任并记录主机指纹。
 *
 * 参数：info 为用户核对过的指纹信息
 * 核心流程：写库成功后再重连，避免"点了信任但其实没存"。
 */
export function trustHostKey(info: HostKeyInfo): Promise<void> {
  return invoke("trust_host_key", {
    host: info.host,
    port: info.port,
    keyType: info.keyType,
    sha256: info.sha256,
  });
}

/**
 * 删除某台主机的指纹记录。
 *
 * 参数：host/port 定位
 * 核心流程：用于"密钥已变更"场景，删掉旧记录后重新确认。
 */
export function forgetHostKey(host: string, port: number): Promise<number> {
  return invoke("forget_host_key", { host, port });
}

/** 一条已知主机记录，后端字段是 camelCase。 */
export type KnownHost = {
  host: string;
  port: number;
  keyType: string;
  fingerprint: string;
};

/**
 * 列出已记录的全部主机指纹。
 *
 * 参数：无
 * 核心流程：供已知主机管理界面展示与逐条删除，排序由后端保证。
 */
export function listKnownHosts(): Promise<KnownHost[]> {
  return invoke<KnownHost[]>("list_known_hosts");
}
