import { forwardRef, useCallback, useEffect, useImperativeHandle, useRef, useState } from "react";
import { Terminal, type ILink } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import type { ISearchOptions } from "@xterm/addon-search";
import "@xterm/xterm/css/xterm.css";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { Protocol, Session } from "../api/session";
import { api, errorDetail, errorKind, errorMessage } from "../api/session";
import { clampFontSize } from "../api/settings";
import type { ThemeKind, ThemePalette } from "../lib/themes";
import { mix, themeToXterm } from "../lib/themes";
import type { HostKeyInfo, SshAuthRequest, TerminalEvent } from "../api/terminal";
import { tunnelConfigList } from "../api/tunnel";
import { IconClose, IconSearch } from "./icons";
import { TerminalSearchBar } from "./TerminalSearchBar";
import { PasteConfirmDialog, splitPasteLines } from "./PasteConfirmDialog";
import {
  base64ToBytes,
  closeLocalTerminal,
  resizeLocalTerminal,
  startLocalTerminal,
  startSerialTerminal,
  SERIAL_DEFAULTS,
  SERIAL_MAX_BAUD_RATE,
  startSshTerminal,
  startTelnetTerminal,
  startWslTerminal,
  terminalCwd,
  textToBase64,
  writeLocalTerminal,
} from "../api/terminal";

type Props = {
  session: Session;
  /** 当前生效主题（明暗 + xterm 配色），由 App 从主题注册表解析后传入 */
  theme: ThemePalette;
  /** 终端字号（CSS px），10 ~ 24 */
  fontSize: number;
  /** 终端字体族（CSS font-family 字符串），由 App 从预设映射而来 */
  fontFamily: string;
  /** Ctrl + 滚轮调整字号后回调，持久化由 App 负责 */
  onFontSizeChange?: (next: number) => void;
  /** SSH 认证方式，仅在内存中传递。 */
  sshAuth?: SshAuthRequest | null;
  /** 用户已确认过该主机指纹。 */
  acceptUnknownHostKey?: boolean;
  /** 首次连接遇到未知主机 -> 交给上层弹确认框。 */
  onHostKeyPrompt?: (sessionId: string, info: HostKeyInfo) => void;
  /** 指纹与记录不一致 -> 交给上层做硬阻断提示。 */
  onHostKeyMismatch?: (sessionId: string, info: HostKeyInfo) => void;
  /** shell 通过 OSC 7 上报了工作目录 -> 交给上层同步给同主机的 SFTP 面板。 */
  onCwdChange?: (session: Session, path: string) => void;
  /** 关闭本面板，只有分屏出来的第二块才传，点了回到单栏。 */
  onClosePane?: () => void;
  /** 关闭整个标签（结束面板的 Enter / 「关闭标签」按钮），由上层从标签列表里移除。 */
  onCloseTab?: () => void;
  /**
   * 上报一条状态栏提示，目前只有「打开链接失败」会用到。
   *
   * 面板自己不持有全局提示 -> 直接接到上层的 notice 机制上；
   * tone 缺省按 info 处理，失败类文案传 "error" 让状态栏变红。
   */
  onNotice?: (text: string, tone?: "info" | "error") => void;
  /**
   * 终端建立/销毁时上报后端注册表里的终端 id。
   *
   * 远端指标要用这个 id 去查底层 SSH 连接（不是会话 id）；
   * 面板卸载或终端自然退出时上报 null，让上层停止对该会话采集。
   * 第三个参数是「要求注销的那个 id」：分屏下同一会话有两个终端，
   * 上层只在记录仍等于它时才删除，避免先退出的那一块把还活着的那块也注销掉。
   */
  onTerminalId?: (sessionId: string, terminalId: string | null, expectedId?: string) => void;
  /**
   * 连接成功后是否在终端顶部画一块信息横幅（设置项 terminal.banner）。
   *
   * 只在「建连那一刻」读一次 -> 中途关掉开关不会重连，也不会擦掉已经画好的横幅；
   * 组件自己不读设置库，缺省开启由 DEFAULT_SETTINGS 与这里的默认值共同保证。
   */
  banner?: boolean;
  /** 文件面板是否跟随终端目录（设置项 sftp.followCwd），横幅里展示「开 / 关」 */
  followCwd?: boolean;
  /**
   * 本面板是不是分屏里当前活动的这一块。
   *
   * 只用于上层加 is-active 样式与表达语义，组件内部不改任何行为 ->
   * 这里只声明不解构，避免多一个用不上的变量。
   */
  active?: boolean;
  /** 用户点击或聚焦到本面板时上报，上层据此挪动活动态（只影响样式，不影响连接） */
  onActivate?: () => void;
};

type Status = "starting" | "running" | "exited" | "error";

/**
 * 解析 OSC 7 上报的工作目录。
 *
 * 参数：data 为 OSC 7 的负载，形如 file://host/path
 * 核心流程：只认 file:// 前缀，跳过主机段取出路径并解 %XX；
 * 不是 file 协议或没有路径部分时返回 null，宁可不同步也不伪造路径。
 */
function parseOsc7Cwd(data: string): string | null {
  if (!data.startsWith("file://")) return null;
  const rest = data.slice("file://".length);
  const slash = rest.indexOf("/");
  if (slash < 0) return null;
  const raw = rest.slice(slash);
  try {
    return decodeURIComponent(raw);
  } catch {
    return raw;
  }
}

/**
 * 把会话 extra 解析成对象。
 *
 * 参数：session 为要运行的会话
 * 核心流程：extra 是自由 JSON，解析失败、不是对象或为空时一律回空对象，
 * 让各个取值都退回默认值，而不是让老会话的脏数据把连接挡在门外。
 */
function sessionExtraObject(session: Session): Record<string, unknown> {
  if (!session.extra) return {};
  try {
    const parsed: unknown = JSON.parse(session.extra);
    if (typeof parsed === "object" && parsed !== null) {
      return parsed as Record<string, unknown>;
    }
    return {};
  } catch {
    return {};
  }
}

/**
 * 从会话 extra 里取串口设备路径。
 *
 * 参数：session 为要运行的会话
 * 核心流程：extra 是自由 JSON，键名为 serialPort；解析失败或值不是非空字符串时
 * 返回空串，交给后端报「串口设备不能为空」，而不是在这里拼出一个坏路径。
 */
function serialDevicePathOf(session: Session): string {
  const value = sessionExtraObject(session).serialPort;
  return typeof value === "string" ? value.trim() : "";
}

/** 串口线路参数的五个取值。 */
type SerialLineOptions = {
  baudRate: number;
  dataBits: number;
  stopBits: number;
  parity: string;
  flowControl: string;
};

/** 各参数的合法取值，与核心层 `serial.rs` 的映射一一对应。 */
const SERIAL_PARITIES = ["none", "odd", "even"];
const SERIAL_FLOW_CONTROLS = ["none", "software", "hardware"];
const SERIAL_DATA_BITS = [5, 6, 7, 8];
/** 1.5 保留在列表里：后端会因为平台没有对应枚举而明确拒绝 */
const SERIAL_STOP_BITS = [1, 1.5, 2];

/**
 * 从会话 extra 里取串口线路参数。
 *
 * 参数：session 为要运行的会话
 * 核心流程：逐项读 serialBaud / serialDataBits / serialStopBits / serialParity /
 * serialFlow，缺键、类型不对或取值不认识时回落到默认值
 * —— 老会话没有这些键也能照常连接，不会被当成错误。
 */
function serialLineOptionsOf(session: Session): SerialLineOptions {
  const extra = sessionExtraObject(session);

  const rawBaud = extra.serialBaud;
  const baudRate =
    typeof rawBaud === "number" &&
    Number.isInteger(rawBaud) &&
    rawBaud > 0 &&
    rawBaud <= SERIAL_MAX_BAUD_RATE
      ? rawBaud
      : SERIAL_DEFAULTS.baudRate;

  const rawDataBits = extra.serialDataBits;
  const dataBits =
    typeof rawDataBits === "number" && SERIAL_DATA_BITS.includes(rawDataBits)
      ? rawDataBits
      : SERIAL_DEFAULTS.dataBits;

  const rawStopBits = extra.serialStopBits;
  const stopBits =
    typeof rawStopBits === "number" && SERIAL_STOP_BITS.includes(rawStopBits)
      ? rawStopBits
      : SERIAL_DEFAULTS.stopBits;

  const rawParity = extra.serialParity;
  const parity =
    typeof rawParity === "string" && SERIAL_PARITIES.includes(rawParity)
      ? rawParity
      : SERIAL_DEFAULTS.parity;

  const rawFlow = extra.serialFlow;
  const flowControl =
    typeof rawFlow === "string" && SERIAL_FLOW_CONTROLS.includes(rawFlow)
      ? rawFlow
      : SERIAL_DEFAULTS.flowControl;

  return { baudRate, dataBits, stopBits, parity, flowControl };
}

/**
 * 从会话 extra 里取 WSL 发行版名。
 *
 * 参数：session 为要运行的会话
 * 核心流程：extra 是一段自由 JSON 字符串，键名为 wslDistro；
 * 解析失败、不是对象、值不是字符串或为空时一律回空串，
 * 空串在后端表示「用 WSL 默认发行版」，不会拼出 `-d ""` 这种坏参数。
 */
function wslDistroOf(session: Session): string {
  if (!session.extra) return "";
  try {
    const parsed: unknown = JSON.parse(session.extra);
    if (typeof parsed !== "object" || parsed === null) return "";
    const value = (parsed as { wslDistro?: unknown }).wslDistro;
    return typeof value === "string" ? value.trim() : "";
  } catch {
    return "";
  }
}

/** 一次终端启动需要的全部上下文：协议只决定「用哪个启动器」。 */
type LaunchParams = {
  /** 要运行的会话，SSH 地址、串口参数、WSL 发行版都从这里取 */
  session: Session;
  /** SSH 认证方式，只有 ssh 用得到 */
  sshAuth?: SshAuthRequest | null;
  /** 用户已确认过该主机指纹，只有 ssh 用得到 */
  acceptUnknownHostKey: boolean;
  cols: number;
  rows: number;
  /** 数据与退出事件的回调 */
  onEvent: (event: TerminalEvent) => void;
};

/** 一个启动器：把启动上下文换成后端登记的终端 id */
type TerminalLauncher = (params: LaunchParams) => Promise<string>;

/**
 * 协议 -> 启动器。
 *
 * 无参数
 * 核心流程：把原先的三元链换成一张查表，新增终端协议时只在这张表里加一行；
 * 表里漏了某个协议不会静默回落到本地 shell（见 launchFor）。
 */
const TERMINAL_LAUNCHERS: Partial<Record<Protocol, TerminalLauncher>> = {
  ssh: ({ session, sshAuth, acceptUnknownHostKey, cols, rows, onEvent }) =>
    startSshTerminal(
      {
        host: session.host ?? "",
        port: session.port ?? 22,
        username: session.username ?? "",
        auth: sshAuth ?? { kind: "password", password: "" },
        cols,
        rows,
        acceptUnknownHostKey,
      },
      onEvent,
    ),
  telnet: ({ session, cols, rows, onEvent }) =>
    startTelnetTerminal(
      {
        host: session.host ?? "",
        port: session.port ?? 23,
        cols,
        rows,
      },
      onEvent,
    ),
  serial: ({ session, onEvent }) => {
    // 设备路径与线路参数都存在会话 extra 里；缺键就是 9600-8-N-1 无流控
    const { baudRate, dataBits, stopBits, parity, flowControl } = serialLineOptionsOf(session);
    return startSerialTerminal(
      {
        path: serialDevicePathOf(session),
        baudRate,
        dataBits,
        stopBits,
        parity,
        flowControl,
      },
      onEvent,
    );
  },
  wsl: ({ session, cols, rows, onEvent }) =>
    startWslTerminal(
      {
        // 发行版存在会话 extra 里，空串 = WSL 默认发行版
        distro: wslDistroOf(session),
        cols,
        rows,
      },
      onEvent,
    ),
  local: ({ cols, rows, onEvent }) => startLocalTerminal(cols, rows, onEvent),
};

/**
 * 按协议挑一个启动器。
 *
 * 参数：protocol 为会话协议，params 为启动上下文
 * 核心流程：查表命中就直接调；表里没有该协议 -> 回一个明确的错误，
 * 不再像旧三元链那样「除 ssh/telnet/serial/wsl 外一律当本地终端」，
 * 免得非法协议悄悄在本机起一个 shell。
 */
function launchFor(protocol: Protocol, params: LaunchParams): Promise<string> {
  const launcher = TERMINAL_LAUNCHERS[protocol];
  if (!launcher) return Promise.reject(new Error(`不支持的终端协议：${protocol}`));
  return launcher(params);
}

/** 搜索命中高亮：明暗各一套，否则浅色主题下命中块与文字会撞色 */
const SEARCH_OPTIONS: Record<ThemeKind, ISearchOptions> = {
  dark: {
    decorations: {
      matchBackground: "#2b3f57",
      matchBorder: "#4a9eff",
      matchOverviewRuler: "#4a9eff",
      activeMatchBackground: "#4a9eff",
      activeMatchBorder: "#ffd479",
      activeMatchColorOverviewRuler: "#ffd479",
    },
  },
  light: {
    decorations: {
      matchBackground: "#ffe9a8",
      matchBorder: "#b8860b",
      matchOverviewRuler: "#b8860b",
      activeMatchBackground: "#ffd479",
      activeMatchBorder: "#8a5a00",
      activeMatchColorOverviewRuler: "#8a5a00",
    },
  },
};

/**
 * 把一段文本写进系统剪贴板。
 *
 * 参数：text 为要写入的文本
 * 核心流程：先走异步 Clipboard API（在键盘事件里调用算用户手势，Tauri 的 webview 支持），
 * 被拒（非安全上下文 / 没有权限）时回落到临时 textarea + execCommand("copy")。
 * 不做这个回落就会变成「按了复制、剪贴板还是旧内容」这种最难排查的静默失败。
 */
async function writeClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // 落到下面的兼容路径
  }
  const previous = document.activeElement as HTMLElement | null;
  const helper = document.createElement("textarea");
  helper.value = text;
  helper.setAttribute("readonly", "");
  // 放到视口外且透明：不闪一下，也不会把布局顶歪
  helper.style.position = "fixed";
  helper.style.top = "-1000px";
  helper.style.opacity = "0";
  document.body.appendChild(helper);
  helper.select();
  let copied = false;
  try {
    copied = document.execCommand("copy");
  } catch {
    copied = false;
  }
  helper.remove();
  // 临时 textarea 会把焦点从终端抢走，复制完必须还回去，否则下一个键打不进终端
  previous?.focus?.();
  return copied;
}

/**
 * 复制终端选区到剪贴板。
 *
 * 参数：term 为 xterm 实例
 * 核心流程：没有选区就什么都不做（调用方据此决定是否把 Ctrl+C 当 SIGINT 放行）；
 * 有选区才写剪贴板，避免把空串写进去把用户原来的剪贴板内容清掉。
 */
function copySelection(term: Terminal): void {
  const text = term.getSelection();
  if (!text) return;
  void writeClipboard(text);
}

/**
 * 同一时刻「正在被用户操作」的面板令牌。
 *
 * 参数：无
 * 核心流程：分屏下两块面板共用同一个会话（连的是同一台机器、同一个目标目录），
 * 谁上报 cwd 谁就决定文件面板跟哪里。这里用模块级变量记录最后一次获得焦点的
 * 面板，只有自己的令牌等于它时才上报 -> 切焦点即切换跟随目标，互不覆盖。
 * 放在模块级是为了不依赖上层传参，也不需要额外的 Context。
 */
let activePaneToken: symbol | null = null;

/** 面板向搜索条暴露的最小能力 */
type SearchApi = {
  /** 参数：query 为关键字，backwards 为 true 时向上找 */
  find: (query: string, backwards: boolean) => void;
  /** 参数：无，可用于清掉所有高亮 */
  clear: () => void;
};

/* ------------------------------------------------------------------ *
 * 连接横幅：客户端自己画在终端最上方的一块方框
 *
 * 参照 MobaXterm 的做法：连上之后由本地渲染一段文本（不是远端发来的），
 * 用来看清「这次连接实际生效了什么」。只画客户端已知的事实，
 * 不探测远端信息，也不往远端注入任何字节。
 * ------------------------------------------------------------------ */

/** 横幅宽度区间（可见列数）：太窄装不下文案，太宽在宽屏上显得空 */
const BANNER_MIN_WIDTH = 32;
const BANNER_MAX_WIDTH = 78;
/** 版式固定的几段文字宽度，" RustMobaX " 正好 11 列（首角与两侧各 1 列另算） */
const BANNER_TITLE = "RustMobaX";
/** 事实之间的分隔符 */
const BANNER_SEPARATOR = " · ";

const ANSI_RESET = "\x1b[0m";
const ANSI_BOLD = "\x1b[1m";

/**
 * 宽字符区间：中日韩文字、全角标点在东亚洲字宽下各占两列。
 *
 * 参数：无（正则常量）
 * 核心流程：只做区间粗判，覆盖常见中文/日文/韩文/全角符号；
 * 万一把某个字符判错，后果也只是右侧边框差一列，不影响功能。
 */
const WIDE_CHAR =
  /[\u1100-\u115F\u2E80-\u303E\u3041-\u33FF\u3400-\u4DBF\u4E00-\u9FFF\uA000-\uA4CF\uAC00-\uD7A3\uF900-\uFAFF\uFE30-\uFE4F\uFF00-\uFF60\uFFE0-\uFFE6]/;

/**
 * 单个字符在终端里占几列。
 *
 * 参数：ch 为单个码位对应的字符
 * 核心流程：命中宽字符区间返回 2，其余（含 emoji 之外的普通字符）按 1 算。
 */
function charWidth(ch: string): number {
  return WIDE_CHAR.test(ch) ? 2 : 1;
}

/**
 * 一段文本在终端里占几列。
 *
 * 参数：text 为纯文本（不能带 ANSI 转义，转义序列不占列）
 * 核心流程：逐码位累加显示宽度 -> 补空格与右侧边框才能对齐。
 */
function textWidth(text: string): number {
  let width = 0;
  for (const ch of text) width += charWidth(ch);
  return width;
}

/**
 * 按显示宽度截断，超出部分用省略号收尾。
 *
 * 参数：text 为原始文本，max 为允许的最大显示列数
 * 核心流程：先量宽度，装得下就原样返回；装不下就逐字累加到 max-1 为止，
 * 末尾补一个「…」（占 1 列）-> 结果宽度永远不超过 max。
 */
function truncateToWidth(text: string, max: number): string {
  if (textWidth(text) <= max) return text;
  let used = 0;
  let out = "";
  for (const ch of text) {
    const width = charWidth(ch);
    if (used + width > max - 1) break;
    used += width;
    out += ch;
  }
  return `${out}…`;
}

/**
 * 把十六进制主题色转成 RGB 三元组。
 *
 * 参数：hex 为 #rgb 或 #rrggbb
 * 核心流程：补全三位简写后取前 6 位解析，解析不出来时回落到中灰
 * -> 脏色值最多让横幅发灰，不会抛异常也不会写出非法转义。
 */
function hexToRgb(hex: string): [number, number, number] {
  const raw = hex.trim().replace(/^#/, "");
  const full = raw.length === 3 ? raw.split("").map((ch) => ch + ch).join("") : raw;
  const value = Number.parseInt(full.slice(0, 6), 16);
  if (!Number.isFinite(value)) return [140, 140, 140];
  return [(value >> 16) & 0xff, (value >> 8) & 0xff, value & 0xff];
}

/**
 * 真彩色前景色的 ANSI 序列。
 *
 * 参数：hex 为十六进制色
 * 核心流程：走 24 位真彩色（38;2;r;g;b）而不是 256 色索引，
 * 这样浅色/深色主题都能用上主题里那一档精确的 accent，不必凑近似色。
 */
function ansiFg(hex: string): string {
  const [r, g, b] = hexToRgb(hex);
  return `\x1b[38;2;${r};${g};${b}m`;
}

/** 横幅里的一格「键 + 值」：键用暗一档的文字色，值用 accent 高亮 */
type BannerFact = { key: string; value: string };

/** 串口校验位的单字母写法，与 8-N-1 这种口语记法一致 */
const PARITY_LETTER: Record<string, string> = { none: "N", odd: "O", even: "E" };
/** 串口流控的中文说明 */
const FLOW_LABEL: Record<string, string> = {
  none: "无流控",
  software: "软件流控",
  hardware: "硬件流控",
};

/**
 * 横幅里的认证方式文案。
 *
 * 参数：session 为要连接的会话
 * 核心流程：先看 extra 里是否引用了保险库里的凭据（credentialId 是密码凭据、
 * passphraseCredentialId 是私钥口令），再按 authKind 落到「密码 / 密钥 / Agent」。
 * 只做文案映射，不去读凭据本身。
 */
function bannerAuthLabel(session: Session): string {
  const extra = sessionExtraObject(session);
  const vaultPassword =
    typeof extra.credentialId === "string" && extra.credentialId.trim() !== "";
  const vaultPassphrase =
    typeof extra.passphraseCredentialId === "string" &&
    extra.passphraseCredentialId.trim() !== "";
  if (session.authKind === "agent") return "Agent";
  if (session.authKind === "key") return vaultPassphrase ? "密钥（保险库）" : "密钥";
  return vaultPassword ? "密码（保险库）" : "密码";
}

/**
 * 横幅第二行的「连到哪」。
 *
 * 参数：session 为要连接的会话
 * 核心流程：SSH / Telnet 拼 user@host:port（没填用户名就省掉 user@），
 * 串口显示设备路径，WSL 显示发行版，本地会话就写「本地 shell」。
 */
function bannerTarget(session: Session): string {
  if (session.protocol === "serial") return serialDevicePathOf(session) || "串口设备";
  if (session.protocol === "wsl") {
    const distro = wslDistroOf(session);
    return distro ? `WSL ${distro}` : "WSL 默认发行版";
  }
  if (session.protocol !== "ssh" && session.protocol !== "telnet") return "本地 shell";
  const host = session.host?.trim() || "—";
  const port = session.port ?? (session.protocol === "ssh" ? 22 : 23);
  const user = session.username?.trim() ?? "";
  return user ? `${user}@${host}:${port}` : `${host}:${port}`;
}

/**
 * 横幅第三行的若干条事实。
 *
 * 参数：session 为会话，followCwd 为目录跟随开关，tunnelCount 为该会话的
 *       autoStart 隧道条数（null = 还没拿到，显示「—」）
 * 核心流程：按协议只挑说得准的字段 —— SSH 才有认证方式、SFTP、隧道；
 * Telnet 只说协议，串口说线路参数，本地/WSL 只说目录跟随。
 * 不显示任何需要探测远端才能知道的东西。
 */
function bannerFacts(
  session: Session,
  followCwd: boolean,
  tunnelCount: number | null,
): BannerFact[] {
  if (session.protocol === "ssh") {
    return [
      { key: "认证", value: bannerAuthLabel(session) },
      // SFTP 走 SSH 子系统、复用同一条连接 -> 只要是 SSH 会话就必然可用
      { key: "SFTP", value: "可用" },
      { key: "目录跟随", value: followCwd ? "开" : "关" },
      { key: "隧道", value: tunnelCount === null ? "—" : `${tunnelCount} 条` },
    ];
  }
  if (session.protocol === "telnet") {
    return [{ key: "协议", value: "Telnet" }];
  }
  if (session.protocol === "serial") {
    const line = serialLineOptionsOf(session);
    const parity = PARITY_LETTER[line.parity] ?? "N";
    return [
      { key: "线路", value: `${line.baudRate} ${line.dataBits}-${parity}-${line.stopBits}` },
      { key: "流控", value: FLOW_LABEL[line.flowControl] ?? "无流控" },
    ];
  }
  return [{ key: "目录跟随", value: followCwd ? "开" : "关" }];
}

/** 画一块横幅需要的全部输入 */
type BannerOptions = {
  /** 终端列数，决定横幅宽度（写一次就定下来，之后 resize 不重绘） */
  cols: number;
  /** 当前主题，颜色全部取自它 -> 深色/浅色主题下都能看清 */
  theme: ThemePalette;
  session: Session;
  followCwd: boolean;
  tunnelCount: number | null;
};

/**
 * 拼出整块横幅的文本（含 ANSI 真彩色与前后空行）。
 *
 * 参数：见 BannerOptions
 * 核心流程：按 term.cols 定宽度并夹进 32~78 -> 逐行padding 到同一列宽
 * （中文按两列算，否则右边框会错位）-> 关键字上色：
 * 边框 accent 压暗一档、标题加粗 accent、键名 textDim、值 accent。
 * 内容过长时优先截断值、必要时丢弃末尾的事实，宁可少说一句也不撑破方框。
 */
function buildBanner(options: BannerOptions): string {
  const { cols, theme, session, followCwd, tunnelCount } = options;
  const width = Math.max(BANNER_MIN_WIDTH, Math.min(cols - 1, BANNER_MAX_WIDTH));
  /** 内容区可用列数：左右边框各 1 列 + 左侧固定一个空格 + 右侧至少留一个空格 */
  const contentMax = width - 4;

  // 边框用 accent 与背景混合后的暗一档，深色下是暗蓝、浅色下是浅蓝，都不会抢内容
  const border = ansiFg(mix(theme.colors.accent, theme.colors.bg, 0.45));
  const title = `${ANSI_BOLD}${ansiFg(theme.colors.accent)}${BANNER_TITLE}${ANSI_RESET}`;
  const keyPaint = ansiFg(theme.colors.textDim);
  const valuePaint = ansiFg(theme.colors.accent);
  const namePaint = ansiFg(theme.colors.text);

  /**
   * 给一行内容套上左右边框。
   *
   * 参数：body 为已带 ANSI 的内容，bodyWidth 为它的可见列数
   * 核心流程：按可见列数补右侧空格 -> 右边界永远对齐在 width 列。
   */
  const row = (body: string, bodyWidth: number): string => {
    const pad = " ".repeat(Math.max(1, width - 3 - bodyWidth));
    return `${border}│${ANSI_RESET} ${body}${pad}${border}│${ANSI_RESET}`;
  };

  // 第二行：会话名 + 连到哪
  const nameRaw = session.name.trim() || "未命名会话";
  const nameText = truncateToWidth(nameRaw, Math.max(6, Math.floor(contentMax / 2) - 2));
  const targetText = truncateToWidth(bannerTarget(session), Math.max(6, contentMax - 2 - textWidth(nameText)));
  const line2 = `${namePaint}${nameText}${ANSI_RESET}  ${valuePaint}${targetText}${ANSI_RESET}`;
  const line2Width = textWidth(nameText) + 2 + textWidth(targetText);

  // 第三行：键值对，放不下就丢末尾的事实（值为空也不留「隧道 —」那样的半截）
  const chosen: BannerFact[] = [];
  let line3Width = 0;
  const sepWidth = textWidth(BANNER_SEPARATOR);
  for (const fact of bannerFacts(session, followCwd, tunnelCount)) {
    const cost = textWidth(fact.key) + 1 + textWidth(fact.value) + (chosen.length > 0 ? sepWidth : 0);
    if (chosen.length === 0) {
      // 第一条永远保留，值太长就截断，保证第三行至少有一句有用的话
      const valueMax = Math.max(2, contentMax - textWidth(fact.key) - 1);
      const value = truncateToWidth(fact.value, valueMax);
      chosen.push({ key: fact.key, value });
      line3Width = textWidth(fact.key) + 1 + textWidth(value);
      continue;
    }
    if (line3Width + cost > contentMax) break;
    chosen.push(fact);
    line3Width += cost;
  }
  const line3 = chosen
    .map((fact) => `${keyPaint}${fact.key}${ANSI_RESET} ${valuePaint}${fact.value}${ANSI_RESET}`)
    .join(`${keyPaint}${BANNER_SEPARATOR}${ANSI_RESET}`);

  // 顶栏：┌ RustMobaX ───┐，" " + 标题 + " " 占 11 列，两侧角各 1 列
  const fill = Math.max(1, width - BANNER_TITLE.length - 4);
  const top = `${border}┌${ANSI_RESET} ${title} ${border}${"─".repeat(fill)}┐${ANSI_RESET}`;
  const bottom = `${border}└${"─".repeat(width - 2)}┘${ANSI_RESET}`;

  // 前后各留一个空行，避免横幅与 motd、shell 提示符粘在一起
  return `\r\n${top}\r\n${row(line2, line2Width)}\r\n${row(line3, line3Width)}\r\n${bottom}\r\n\r\n`;
}

/**
 * 生成「保存输出」默认文件名用的本地时间戳。
 *
 * 参数：date 为要格式化的时间，缺省取当前时刻
 * 核心流程：手拼 YYYYMMDD-HHmmss，只依赖本地时区，不引入日期库。
 */
function fileStamp(date = new Date()): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return (
    `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}` +
    `-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`
  );
}

/**
 * 终端里「纯文本 URL」的匹配规则。
 *
 * 只认带 scheme 的 http/https（大小写不敏感），并且不吃空白、引号与成对括号 ——
 * 反过来 file://、ftp://、javascript: 与裸域名都不会命中。带 g 是为了 exec 循环，
 * 同一份实例被反复使用，每次进 provideLinks 都要把 lastIndex 归零。
 */
const URL_PATTERN = /https?:\/\/[^\s"'`<>()[\]{}]+/gi;

/** 面板向父层暴露的最小能力：把键盘焦点交给本块的 xterm */
export type TerminalPanelHandle = {
  /** 参数：无，等价于点一下终端本体 */
  focus: () => void;
};

/**
 * 终端面板，本地与 SSH 共用。
 *
 * 参数：session 为要运行的会话，password 为 SSH 密码
 * 核心流程：挂载时创建 xterm → 按协议选择启动方式 → 双向转发数据 →
 * 监听容器尺寸同步 resize；卸载时关闭会话并回收实例。
 *
 * 用 forwardRef 只为把 focus 暴露给上层（分屏切换焦点），不改变挂载/卸载时机；
 * 调用点仍然写 `<TerminalPanel ... />`，只是多了一个可选 ref。
 */
export const TerminalPanel = forwardRef<TerminalPanelHandle, Props>(
  function TerminalPanel({
    session,
    theme,
    fontSize,
    fontFamily,
    onFontSizeChange,
    sshAuth,
    acceptUnknownHostKey = false,
    onHostKeyPrompt,
    onHostKeyMismatch,
    onCwdChange,
    onClosePane,
    onCloseTab,
    onNotice,
    onTerminalId,
    banner = true,
    followCwd = true,
    onActivate,
  }: Props, ref) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const searchApiRef = useRef<SearchApi | null>(null);
  /** 当前主题的搜索高亮，放 ref 里 -> 切主题后 find 立刻用新配色 */
  const searchOptionsRef = useRef<ISearchOptions>(SEARCH_OPTIONS[theme.kind]);
  /** 「fit + 同步远端尺寸」的引用，供字号/字体变化时复用 */
  const refitRef = useRef<(() => void) | null>(null);
  const [status, setStatus] = useState<Status>("starting");
  const [error, setError] = useState<string | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchResult, setSearchResult] = useState({ index: -1, count: 0 });
  /** 待确认的多行粘贴内容，null 表示没有待确认项 */
  const [pendingPaste, setPendingPaste] = useState<string | null>(null);
  /** 搜索条是否打开，供终端内的 Esc 处理读取（避免闭包拿到旧值） */
  const searchOpenRef = useRef(false);
  /** 最近一次会话的退出码，null 表示不是正常退出（或还没退出），只在结束面板上展示 */
  const [exitCode, setExitCode] = useState<number | null>(null);
  /** 结束面板上的临时提示（保存输出成功/失败），null 表示没有 */
  const [exitNotice, setExitNotice] = useState<string | null>(null);
  /**
   * 会话是否已经结束（正常退出或连接失败）。
   *
   * 必须用 ref 而不是 status：建连 effect 只跑一次，里面的 keydown 处理器
   * 闭包拿到的永远是首帧的 status（"starting"），读 state 会导致结束态判不出来。
   */
  const endedRef = useRef(false);
  /** 是否正在重连，防止结束面板上「R 重新连接」被连点两次 */
  const restartingRef = useRef(false);
  /** 每次重连自增 -> 让建连 effect 重跑（卸载旧终端并重新建连） */
  const [restartToken, setRestartToken] = useState(0);
  /** 手动重连后要不要把焦点交给新终端（新 xterm 不会自动抢焦点） */
  const focusAfterConnectRef = useRef(false);

  useEffect(() => {
    searchOpenRef.current = searchOpen;
  }, [searchOpen]);

  // 分屏切换焦点：上层拿到句柄后调 focus()，等价于点一下终端本体。
  // 只依赖稳定的 termRef -> 同一个句柄对象整个生命周期都有效
  useImperativeHandle(
    ref,
    () => ({
      focus: () => {
        termRef.current?.focus();
      },
    }),
    [],
  );

  /**
   * 横幅开关与目录跟随都只在「建连那一刻」读一次。
   *
   * 放 ref 而不是塞进建连 effect 的依赖数组：那两个值一变就会重建终端，
   * 等于改个设置把用户的会话掐了 —— 这里只更新引用，当前连接不受影响。
   * 初始值取 useRef 的入参，所以首帧就连上时读到的也是正确值。
   */
  const bannerRef = useRef(banner);
  const followCwdRef = useRef(followCwd);
  useEffect(() => {
    bannerRef.current = banner;
    followCwdRef.current = followCwd;
  }, [banner, followCwd]);

  /**
   * 收起搜索条。
   *
   * 参数：无
   * 核心流程：清掉命中高亮 -> 重置计数 -> 把焦点还给终端，方便继续输入。
   */
  const closeSearch = useCallback(() => {
    setSearchOpen(false);
    setSearchResult({ index: -1, count: 0 });
    searchApiRef.current?.clear();
    termRef.current?.focus();
  }, []);

  /**
   * 重连（结束面板的 R / 「R 重新连接」按钮）。
   *
   * 参数：无
   * 核心流程：先合上重连闸门防止连点 -> 清掉结束态标记 -> 自增 restartToken 让
   * 建连 effect 重跑。旧终端与旧后端会话交给 effect 的 cleanup 回收，这里不重复清理；
   * 重连后 effect 开头会把 restartingRef 复位。
   */
  const restart = useCallback(() => {
    if (restartingRef.current) return;
    restartingRef.current = true;
    endedRef.current = false;
    // 新旧 xterm 是两个实例：不把焦点交接过去的话，按完 R 直接敲键会没人收
    focusAfterConnectRef.current = true;
    setStatus("starting");
    setRestartToken((n) => n + 1);
  }, []);

  /**
   * 把终端当前缓冲区导出成纯文本文件（结束面板的 S / 「S 保存输出」按钮）。
   *
   * 参数：无
   * 核心流程：逐行 translateToString（去掉行尾空白）-> 砍掉尾部空行 -> 弹保存对话框 ->
   * 走已有的 write_text_file 命令写盘。不装 addon-serialize：这里只要人眼可见的文本，
   * 不需要保留颜色与光标控制序列，手写反而更可控。
   */
  const saveOutput = useCallback(async () => {
    const buf = termRef.current?.buffer.active;
    if (!buf) return;
    const lines: string[] = [];
    for (let i = 0; i < buf.length; i += 1) {
      lines.push(buf.getLine(i)?.translateToString(true) ?? "");
    }
    // 去掉尾部空行：缓冲里未使用的行全是空串，留着会让文件尾巴拖很长一截空白
    while (lines.length > 0 && lines[lines.length - 1].trim() === "") lines.pop();
    const text = `${lines.join("\n")}\n`;
    // 会话名里的 \ / : * ? " < > | 在部分文件系统上非法 -> 统一换成下划线
    const base = (session.name.trim() || "terminal").replace(/[\\/:*?"<>|]/g, "_");
    try {
      const target = await saveDialog({
        defaultPath: `${base}-${fileStamp()}.txt`,
        filters: [{ name: "文本", extensions: ["txt"] }],
      });
      if (!target) return;
      await api.writeTextFile(target, text);
      setExitNotice(`已保存到 ${target}`);
    } catch (err) {
      // 失败也放进结束面板的提示行，不静默吞掉
      setExitNotice(`保存失败：${errorMessage(err)}`);
    } finally {
      // 对话框关掉后焦点会飘走 -> 收回终端，否则之后按 R / Enter / S 都没反应
      termRef.current?.focus();
    }
  }, [session.name]);

  /**
   * 结束面板上的三个动作。
   *
   * 参数：无
   * 核心流程：建连 effect 里的 keydown 处理器按 ref 取当帧实现 -> 关标签回调与
   * session.name 变化都能跟上，又不用把它们塞进 effect 依赖去重建终端。
   */
  const actionsRef = useRef({
    restart,
    closeTab: () => onCloseTab?.(),
    saveOutput,
  });
  useEffect(() => {
    actionsRef.current = { restart, closeTab: () => onCloseTab?.(), saveOutput };
  });

  /**
   * 提示通道。
   *
   * 放 ref 而不是让建连 effect 直接闭包取：上层传进来的 onNotice 是个内联箭头函数，
   * 每次渲染都换引用 -> 塞进依赖数组会让建连 effect 反复重跑（等于把会话掐了）；
   * 这里每次渲染同步一次，effect 里读到的永远是最新那份。
   */
  const noticeRef = useRef(onNotice);
  useEffect(() => {
    noticeRef.current = onNotice;
  });

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return undefined;

    // 每次建连（含手动重连）都从「未结束」起步：结束态标记、重连闸门、
    // 上一次的退出码与提示都必须清掉，否则新终端一挂上就被当成已结束
    endedRef.current = false;
    restartingRef.current = false;
    setStatus("starting");
    setError(null);
    setExitCode(null);
    setExitNotice(null);

    /**
     * 用系统默认浏览器打开一个链接。
     *
     * 参数：text 为要打开的地址（调用方已保证是 http/https）
     * 核心流程：先把 scheme 归一成小写再交给 Tauri 的 opener 插件走系统默认应用；
     * 失败绝不静默 —— 用户在终端里点了没反应只会当成 bug，所以把错误文案送到状态栏。
     *
     * scheme 必须归一：终端里可能输出 HTTPS:// 这种大写，而 capability 的
     * url glob 是大小写敏感的，放行它的只有 https://*，大写会被插件拒掉。
     */
    const openWithBrowser = (text: string) => {
      const target = text.replace(/^(https?):\/\//i, (_, scheme: string) => `${scheme.toLowerCase()}://`);
      openUrl(target).catch((err) => {
        noticeRef.current?.(`打开链接失败：${errorMessage(err)}`, "error");
      });
    };

    /**
     * 链接激活的唯一闸门（OSC 8 与纯文本 URL 共用）。
     *
     * 参数：event 为触发点击的鼠标事件，text 为该链接的地址
     * 核心流程：只放行 http/https；必须按住 Ctrl（mac 上是 Cmd）才交给系统浏览器。
     * 不满足时什么都不做，尤其**不能** preventDefault —— 否则普通左键的拖选会被吃掉。
     *
     * 收成具名函数而不是两处各写一遍：linkHandler.activate（三参）与
     * ILink.activate（两参）都指向它，判据只有这一份，不会漂移。
     */
    const activateLink = (event: MouseEvent, text: string) => {
      // 只认 http/https：OSC 8 里可能塞 javascript: 之类的伪协议
      if (!/^https?:\/\//i.test(text)) return;
      // 非 Ctrl 的点击什么都不做，且不能 preventDefault -> 否则拖选被吃掉
      if (!(event.ctrlKey || event.metaKey)) return;
      event.preventDefault();
      openWithBrowser(text);
    };
    // xterm 自己会给链接加 pointer 光标与下划线，这个 class 只留作样式钩子；
    // hover/leave 可能在该元素卸载后才被调 -> 走 ref 判空
    const enterLink = () => hostRef.current?.classList.add("is-link-hover");
    const leaveLink = () => hostRef.current?.classList.remove("is-link-hover");

    const term = new Terminal({
      fontFamily,
      fontSize,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 10000,
      theme: themeToXterm(theme),
      // 搜索高亮要靠 xterm 的 registerDecoration / registerMarker，这两个接口
      // 在 xterm 6 里仍属于 proposed API：不开这个开关时 addon-search 的
      // findNext/findPrevious 会直接抛 "You must set the allowProposedApi
      // option to true to use proposed API"，于是一个都搜不到、也跳不动。
      allowProposedApi: true,
      /**
       * 终端里的链接交给系统默认浏览器打开。
       *
       * 必须自己把 Ctrl（mac 上是 Cmd）这道闸：xterm 的 Linkifier.handleMouseUp 不判
       * 修饰键 —— mousedown / mouseup 落在同一个链接上就直接调 activate，不拦的话
       * 普通左键点一下链接也会弹浏览器，还会跟拖选抢事件。
       */
      linkHandler: {
        // OSC 8 超链接继续走这条路，与下面纯文本 URL 的 provider 共用同一份闸门
        activate: activateLink,
        hover: enterLink,
        leave: leaveLink,
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    const search = new SearchAddon();
    term.loadAddon(search);
    termRef.current = term;
    searchApiRef.current = {
      find: (query, backwards) => {
        // 每次搜索都读 ref -> 主题切换后命中配色跟着变，不用重建终端
        if (backwards) search.findPrevious(query, searchOptionsRef.current);
        else search.findNext(query, searchOptionsRef.current);
      },
      clear: () => search.clearDecorations(),
    };
    const searchResultsSubscription = search.onDidChangeResults((event) => {
      setSearchResult({ index: event.resultIndex, count: event.resultCount });
    });
    // Ctrl/Cmd + F 只在面板内唤出搜索条，按键不再下发给 shell
    term.attachCustomKeyEventHandler((event) => {
      if (event.type !== "keydown") return true;
      const ctrlOrMeta = event.ctrlKey || event.metaKey;
      /**
       * 会话结束后键盘改由结束面板接管：R 重连、Enter 关标签、S 保存输出。
       *
       * 判定一律读 endedRef（effect 闭包里的 status 永远是首帧值）。其余按键
       * 全部吞掉 —— 先把 endedRef 设成 true、再 return false，等价于「不转发给后端」，
       * 顺手消掉了「退出后还往已死的会话写数据 -> 后端返回 Err」的静默 rejection。
       * 带 Ctrl/Alt/Meta 的组合键照旧放行，全局快捷键（Ctrl+W 关窗、Ctrl+Tab 切标签）不受影响。
       */
      if (endedRef.current) {
        if (event.ctrlKey || event.altKey || event.metaKey) return true;
        // 走 ref 取最新回调：建连 effect 不会因回调变化而重建终端
        const actions = actionsRef.current;
        const key = event.key.toLowerCase();
        if (key === "r") {
          event.preventDefault();
          actions.restart();
          return false;
        }
        if (event.key === "Enter") {
          event.preventDefault();
          actions.closeTab();
          return false;
        }
        if (key === "s") {
          event.preventDefault();
          actions.saveOutput();
          return false;
        }
        return false;
      }
      /**
       * 复制：Ctrl+Shift+C 一律复制，Ctrl+C 只在有选区时复制（无选区继续发 SIGINT）。
       *
       * 这两条路原本都不通：xterm 把 Ctrl+C 编码成 0x03 并在 _keyDown 里 preventDefault，
       * 浏览器的 copy 命令轮不到执行；Ctrl+Shift+C 则被它算成「无数据」直接放行，而
       * Chromium/WebKit 都没把 Ctrl+Shift+C 绑成复制命令，于是浏览器也没动作。
       * 所以这里自己判定并写剪贴板。返回 false + preventDefault 是为了把键彻底按住，
       * 不走 xterm 的编码路径，避免复制的同时还往 shell 里塞一个 0x03。
       */
      if (ctrlOrMeta && !event.altKey && event.key.toLowerCase() === "c") {
        if (event.shiftKey || term.hasSelection()) {
          event.preventDefault();
          copySelection(term);
          return false;
        }
      }
      // Ctrl+V 与 Ctrl+Shift+V 一律让浏览器默认的 paste 事件去处理（见 handlePaste），
      // 返回 false 拦住 xterm 把它编码成 0x16 发出去；这里不能再 preventDefault，
      // 否则浏览器默认粘贴被吃掉，paste 事件就不会触发。
      if (ctrlOrMeta && event.key.toLowerCase() === "v") {
        return false;
      }
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "f") {
        event.preventDefault();
        setSearchOpen(true);
        return false;
      }
      // 搜索条开着时 Esc 只用来收搜索条，不能漏给远端 shell
      if (event.key === "Escape" && searchOpenRef.current) {
        event.preventDefault();
        closeSearch();
        return false;
      }
      return true;
    });
    term.open(host);

    /**
     * 纯文本 URL 的链接识别。
     *
     * xterm 6 核心只给 OSC 8 转义序列里的地址建链接（内置 OscLinkProvider），
     * 命令行里直接打印出来的普通 http(s) 地址没有链接对象 -> 没有下划线、点了没反应。
     * 这里补一个 provider 填上这一路：只认带 scheme 的 http/https，
     * 不做跨行合并，也不认 file://、ftp://、javascript: 之类的协议。
     *
     * 坐标全 1-based（IBufferRange 约定）：start.x 是首格列号，end.x 是**末格列号（含）**，
     * 与核心 OscLinkProvider 的取值方式一致 —— 写成「末格 + 1」会让下划线多出一格。
     * 回调里只处理传入的那一行：Linkifier 在悬停/重绘时会按行反复调用这里。
     */
    const linkProviderDisposable = term.registerLinkProvider({
      provideLinks: (bufferLineNumber, callback) => {
        // 回调给的行号是 1-based 的 buffer 行（Linkifier 传的是视口行 + ydisp），
        // 而 IBuffer.getLine 收 0-based 的绝对行下标
        const line = term.buffer.active.getLine(bufferLineNumber - 1);
        if (!line) {
          callback(undefined);
          return;
        }
        // 绝大多数行里没有地址：先做一次便宜的整行判定，命中才走逐格重建
        if (!/https?:\/\//i.test(line.translateToString(true))) {
          callback(undefined);
          return;
        }
        // 逐格拼一份「一格一字符」的行文本。translateToString 会把宽字符（中文、emoji）
        // 压成 1 个字符串下标却仍占 2 个终端列，直接拿 match.index 当列号会让链接整体左移；
        // 按每格宽度补齐后，字符串下标就等于 0-based 列号。
        const cell = term.buffer.active.getNullCell();
        let text = "";
        for (let x = 0; x < line.length; ) {
          line.getCell(x, cell);
          const chars = cell.getChars();
          const width = cell.getWidth() || 1;
          text += chars.length === width ? chars : chars.slice(0, width).padEnd(width, " ");
          // 宽字符的后半格是空壳，不能再算一列（translateToString 同样按宽度跳格）
          x += width;
        }
        const links: ILink[] = [];
        URL_PATTERN.lastIndex = 0;
        for (let match = URL_PATTERN.exec(text); match; match = URL_PATTERN.exec(text)) {
          let url = match[0];
          // 剥掉句末标点：英文句子里的 "见 https://a.com/x." 句号不属于地址。
          // 括号类字符已被正则排除，这里只兜标点。
          while (url.length > 0 && /[.,;:!?)]$/.test(url)) url = url.slice(0, -1);
          if (!/^https?:\/\/.+/i.test(url)) continue;
          links.push({
            range: {
              start: { x: match.index + 1, y: bufferLineNumber },
              end: { x: match.index + url.length, y: bufferLineNumber },
            },
            text: url,
            activate: activateLink,
            hover: enterLink,
            leave: leaveLink,
          });
        }
        callback(links.length > 0 ? links : undefined);
      },
    });
    fit.fit();

    // 手动重连（R / 结束面板按钮）后新终端不会自动拿焦点 -> 这里补一次，
    // 用户按完 R 就能直接接着敲命令，不用再点一下
    if (focusAfterConnectRef.current) {
      focusAfterConnectRef.current = false;
      term.focus();
    }

    let disposed = false;
    let terminalId: string | null = null;

    /**
     * 本面板的身份令牌：只有它等于 activePaneToken 时才允许上报 cwd。
     *
     * 参数：无
     * 核心流程：挂载即认领（单面板场景下一挂上就能上报），之后鼠标按下、
     * 获得焦点都会重新认领 -> 分屏下跟随目标始终是用户正在操作的那一块。
     */
    const paneToken = Symbol("terminal-pane");
    /** 本面板最近一次的目录，切焦点时用它立刻把跟随目标切过来 */
    let lastReportedCwd: string | null = null;

    const reportCwd = (path: string) => {
      if (activePaneToken !== paneToken) return;
      if (path === lastReportedCwd) return;
      lastReportedCwd = path;
      onCwdChange?.(session, path);
    };

    const claimActive = () => {
      activePaneToken = paneToken;
      // 已经有目录的面板 -> 立刻上报一次，不用等远端再画一次提示符
      if (lastReportedCwd) onCwdChange?.(session, lastReportedCwd);
    };
    claimActive();

    // 鼠标按下/焦点进入都算「用户在操作这一块」-> 重新认领，抢回上报权；
    // 只有真实用户交互才把「活动 pane」上报给上层（初始挂载/自动认领不上报，
    // 否则分屏刚打开时会把 activePaneIndex 覆盖成后挂载的那块）
    const claimByUser = () => {
      claimActive();
      onActivate?.();
    };
    host.addEventListener("pointerdown", claimByUser, true);
    host.addEventListener("focusin", claimByUser, true);
    const textarea = term.textarea;
    textarea?.addEventListener("focus", claimByUser);

    // 切标签时终端是常驻的（只是被 is-hidden 藏起来）-> 隐藏的面板不能继续
    // 霸占上报权，重新显示出来的那一块要立刻接上，否则目录跟随会「卡在上一个标签」
    const visibility = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        claimActive();
        return;
      }
      if (activePaneToken === paneToken) activePaneToken = null;
    });
    visibility.observe(host);

    // shell 的 OSC 7 会带上当前工作目录 -> 只有活动面板才上报，
    // 否则分屏里两块会同一条会话互相覆盖（后建连的赢）
    const cwdSubscription = term.parser.registerOscHandler(7, (data) => {
      const resolved = parseOsc7Cwd(data);
      if (resolved) reportCwd(resolved);
      return true;
    });

    /**
     * 终端「结束」是否已经上报过注销。
     *
     * 参数：无
     * 核心流程：同一次退出后端可能送多个事件，用这个标记保证
     * onTerminalId(session.id, null) 只发一次；卸载注销走另一条路径，父层
     * handleTerminalId 遇到条目已不存在时会直接返回原 state，重复调用不会抖动。
     */
    let exitReported = false;

    /**
     * 横幅是否已经画过。
     *
     * 参数：无
     * 核心流程：一个面板实例一个标记 -> 重置发生在 effect 重跑（重连）与卸载重建时，
     * 因此重连一定重新画，同一次连接绝不会画第二遍。
     */
    let bannerWritten = false;
    /** 该会话下 autoStart 的隧道条数，null 表示还没取到，横幅里显示「—」 */
    let tunnelCount: number | null = null;

    /**
     * 画一次连接横幅。
     *
     * 参数：无（读本 effect 闭包里的 session / theme / term）
     * 核心流程：先置位再写 -> 并发路径重入时直接返回；
     * 开关关掉时只置位不写，后续路径也不会再画。
     * 写入走 term.write，xterm 内部有写队列，先写的横幅一定排在后面的远端数据前面。
     */
    const flushBanner = () => {
      if (bannerWritten) return;
      bannerWritten = true;
      if (!bannerRef.current) return;
      term.write(
        buildBanner({
          cols: term.cols,
          theme,
          session,
          followCwd: followCwdRef.current,
          tunnelCount,
        }),
      );
    };

    // 隧道条数：SSH 会话才有意义，横幅关着就完全不碰隧道库；
    // 这里只发一次轻量请求，读失败就当「未知」（显示「—」），不打扰用户。
    if (bannerRef.current && session.protocol === "ssh") {
      void tunnelConfigList()
        .then((configs) => {
          if (disposed) return;
          tunnelCount = configs.filter(
            (config) => config.sessionId === session.id && config.autoStart,
          ).length;
        })
        .catch(() => {
          tunnelCount = null;
        });
    }

    // 后端推来的字节直接喂给 xterm -> 保留原始控制序列
    const handleEvent = (event: TerminalEvent) => {
      if (disposed) return;
      if (event.kind === "data") {
        // 远端 motd 可能比建连成功的 Promise 回调更早到达 -> 首个字节到达时补画，
        // 保证横幅永远排在第一段远端输出之前（画过了就是空操作）
        flushBanner();
        term.write(base64ToBytes(event.chunk));
        return;
      }
      setStatus("exited");
      // 结束态用 ref 记：keydown / onData 都靠它判断，别再往已死的会话写数据。
      // 退出码存进 state 只为在结束面板上显示；终端里不再另写一行提示，避免和面板重复
      endedRef.current = true;
      setExitCode(event.code);
      // 自然退出（exit / Ctrl+D / 远端断开）也要注销终端 id，否则上层会继续
      // 拿一个已死的 id 采集指标；终端内容保持原样，由底部结束面板提示后续操作
      if (!exitReported) {
        exitReported = true;
        if (terminalId) onTerminalId?.(session.id, null, terminalId);
      }
    };

    // 按协议查表选启动器 -> 本地起 shell，SSH / Telnet / WSL / 串口各自换后端
    const start = () =>
      launchFor(session.protocol, {
        session,
        sshAuth,
        acceptUnknownHostKey,
        cols: term.cols,
        rows: term.rows,
        onEvent: handleEvent,
      });

    /**
     * 本地会话的目录轮询。
     *
     * 参数：无
     * 核心流程：本地 shell 不发 OSC 7（那是远端才注入的），所以按固定间隔问
     * 后端子进程的 cwd；只在「本面板是活动面板」时上报，读不到或没变化就什么都不做。
     * SSH 会话不轮询 -> 它的目录由远端 shell 用 OSC 7 主动上报。
     */
    let cwdTimer: ReturnType<typeof setInterval> | null = null;
    const pollLocalCwd = () => {
      if (disposed || !terminalId) return;
      if (activePaneToken !== paneToken) return;
      void terminalCwd(terminalId)
        .then((path) => {
          if (disposed || !path) return;
          reportCwd(path);
        })
        .catch(() => {
          // 进程已退出/平台不支持 -> 保持上一个目录，不打扰用户
        });
    };

    void start()
      .then((id) => {
        if (disposed) {
          void closeLocalTerminal(id);
          return;
        }
        terminalId = id;
        // 建连成功 -> 立刻画横幅；首字节先到的话这里已是空操作
        flushBanner();
        setStatus("running");
        onTerminalId?.(session.id, id);
        if (session.protocol === "local") {
          pollLocalCwd();
          cwdTimer = setInterval(pollLocalCwd, 1200);
        }
      })
      .catch((reason) => {
        // 连接失败同样是「结束态」：结束面板换成「连接失败」，R 可以重试
        endedRef.current = true;
        const kind = errorKind(reason);
        const detail = errorDetail<HostKeyInfo>(reason);
        // 指纹相关的失败需要专门交互，不是普通的连接错误
        if (kind === "hostKeyUnknown" && detail) {
          setStatus("error");
          setError("需要确认主机指纹后才能连接");
          onHostKeyPrompt?.(session.id, detail);
          return;
        }
        if (kind === "hostKeyMismatch" && detail) {
          setStatus("error");
          setError("主机密钥已变更，连接已阻断");
          onHostKeyMismatch?.(session.id, detail);
          return;
        }
        setError(errorMessage(reason));
        setStatus("error");
      });

    // 键盘输入 -> 编码后写入会话。
    // 结束态双保险：keydown 已经把普通按键拦掉了，但输入法/粘贴等路径未必走 xterm 的
    // keydown 编码，那时写进已关闭的会话只会换回一个 Err -> 这里直接丢弃
    const dataSubscription = term.onData((data) => {
      if (endedRef.current) return;
      if (terminalId) void writeLocalTerminal(terminalId, textToBase64(data));
    });

    // Ctrl+V 与 Ctrl+Shift+V 都靠浏览器默认的 paste 事件（上面 keydown 已放行）：
    // 在宿主 div 的捕获阶段拦住，否则 xterm 自己的 textarea 会直接写进去，
    // 来不及弹确认框
    const handlePaste = (event: ClipboardEvent) => {
      const text = event.clipboardData?.getData("text/plain") ?? "";
      if (!text) return;
      event.preventDefault();
      event.stopPropagation();
      if (splitPasteLines(text).length > 1) {
        setPendingPaste(text);
        return;
      }
      term.paste(text);
    };
    host.addEventListener("paste", handlePaste, true);

    // 容器尺寸变化 -> 终端重排并同步给远端
    // 侧栏折叠、工具栏/状态栏高度调整都会改容器尺寸，ResizeObserver 负责这一路；
    // 窗口 resize 再补一次，避免只改渲染不改容器时漏算
    const refit = () => {
      fit.fit();
      if (terminalId) void resizeLocalTerminal(terminalId, term.cols, term.rows);
    };
    refitRef.current = refit;
    const observer = new ResizeObserver(refit);
    observer.observe(host);
    window.addEventListener("resize", refit);

    return () => {
      disposed = true;
      host.removeEventListener("paste", handlePaste, true);
      host.removeEventListener("pointerdown", claimByUser, true);
      host.removeEventListener("focusin", claimByUser, true);
      textarea?.removeEventListener("focus", claimByUser);
      visibility.disconnect();
      if (cwdTimer !== null) clearInterval(cwdTimer);
      // 活动面板卸载 -> 让位，避免之后没有任何面板能上报目录
      if (activePaneToken === paneToken) activePaneToken = null;
      observer.disconnect();
      window.removeEventListener("resize", refit);
      dataSubscription.dispose();
      linkProviderDisposable.dispose();
      searchResultsSubscription.dispose();
      cwdSubscription.dispose();
      searchApiRef.current = null;
      refitRef.current = null;
      termRef.current = null;
      if (terminalId) {
        onTerminalId?.(session.id, null, terminalId);
        void closeLocalTerminal(terminalId);
      }
      term.dispose();
    };
  }, [
    session.id,
    session.protocol,
    session.host,
    session.port,
    session.username,
    sshAuth,
    acceptUnknownHostKey,
    onHostKeyPrompt,
    onHostKeyMismatch,
    onCwdChange,
    onTerminalId,
    closeSearch,
    // 「R 重新连接」只改这个计数 -> effect 重跑 = 关旧终端、重新建连
    restartToken,
  ]);

  // 字号/字体变化 -> 直接改 xterm 选项再 refit（fit 后会把新行列数同步给远端），
  // 不重建终端 -> 滚动缓冲与当前会话都保留
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.fontFamily = fontFamily;
    term.options.fontSize = fontSize;
    refitRef.current?.();
    return undefined;
  }, [fontSize, fontFamily]);

  // 主题变化 -> 只换配色，xterm 不会清屏，已打开的终端也立刻跟着变
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    term.options.theme = themeToXterm(theme);
    searchOptionsRef.current = SEARCH_OPTIONS[theme.kind];
    return undefined;
  }, [theme]);

  // Ctrl + 滚轮缩放字号：拦掉浏览器默认缩放，一次滚动只调 1px，
  // 夹在 10 ~ 24；持久化交给 App 的 saveSetting（同 key 300ms 防抖）
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return undefined;
    const handleWheel = (event: WheelEvent) => {
      if (!event.ctrlKey) return;
      event.preventDefault();
      if (event.deltaY === 0) return;
      const next = clampFontSize(fontSize + (event.deltaY < 0 ? 1 : -1));
      if (next !== fontSize) onFontSizeChange?.(next);
    };
    // 捕获阶段监听 -> 先于 xterm 自己的滚轮处理执行，Ctrl+滚轮不会被它吃掉
    host.addEventListener("wheel", handleWheel, { passive: false, capture: true });
    return () => host.removeEventListener("wheel", handleWheel, { capture: true });
  }, [fontSize, onFontSizeChange]);

  /**
   * 确认本次多行粘贴。
   *
   * 参数：无
   * 核心流程：走终端自身的 paste 通道（应用启用 bracketed paste 时由 xterm
   * 自动包上 \x1b[200~ / \x1b[201~），外面不再手工拼标记。
   * 然后关掉弹窗并把焦点还给终端。
   */
  const confirmPaste = () => {
    if (pendingPaste !== null) termRef.current?.paste(pendingPaste);
    setPendingPaste(null);
    termRef.current?.focus();
  };

  /**
   * 取消本次多行粘贴。
   *
   * 参数：无
   * 核心流程：只丢弃剪贴板内容，不动终端里已有的输出。
   */
  const cancelPaste = () => {
    setPendingPaste(null);
    termRef.current?.focus();
  };

  /**
   * 结束面板上的按钮点一下 = 先把焦点还给终端再执行动作。
   *
   * 参数：action 为要执行的动作
   * 核心流程：按钮拿到焦点后按键会被按钮吃掉 -> 动作执行前把焦点交回 xterm，
   * 这样之后的 R / Enter / S 仍然有效（尤其是保存对话框关闭之后）。
   */
  const withTermFocus = (action: () => void) => () => {
    termRef.current?.focus();
    action();
  };

  /** 会话是否已收尾（正常退出或连不上），结束面板与键盘接管都以它为准 */
  const ended = status === "exited" || status === "error";
  /** 结束面板右侧那句补充说明：连接失败的原因，或保存输出的结果 */
  const exitMessage = status === "error" ? error : exitNotice;

  return (
    <div className="terminal-panel">
      <div className="terminal-host" ref={hostRef} />
      <div className="terminal-tools">
        <button
          type="button"
          className="terminal-tool"
          title="搜索 (Ctrl+F)"
          aria-label="搜索"
          onClick={() => setSearchOpen(true)}
        >
          <IconSearch size={11} />
        </button>
        {onClosePane && (
          <button
            type="button"
            className="terminal-tool"
            title="关闭分屏"
            aria-label="关闭分屏"
            onClick={onClosePane}
          >
            <IconClose size={11} />
          </button>
        )}
      </div>
      {ended && (
        // 结束面板贴在终端底部：一块面板同时说明「怎么结束的」和「接下来能按什么」，
        // 快捷键与鼠标点击走同一批动作，两者都只是调这三个函数
        <div className="terminal-exit-bar" role="group" aria-label="会话已结束">
          <span className="exit-title">
            {status === "error" ? "连接失败" : `会话已结束${exitCode !== null ? ` (${exitCode})` : ""}`}
          </span>
          <button type="button" className="exit-action" onClick={withTermFocus(restart)}>
            R 重新连接
          </button>
          <button type="button" className="exit-action" onClick={withTermFocus(() => onCloseTab?.())}>
            Enter 关闭标签
          </button>
          <button type="button" className="exit-action" onClick={withTermFocus(saveOutput)}>
            S 保存输出
          </button>
          {exitMessage && <span className="exit-notice">{exitMessage}</span>}
        </div>
      )}
      {/* 结束态下搜索条一律不渲染 -> Enter 不会被搜索条抢走 */}
      {searchOpen && !ended && (
        <TerminalSearchBar
          resultIndex={searchResult.index}
          resultCount={searchResult.count}
          onFind={(query, backwards) => {
            if (query.trim() === "") {
              searchApiRef.current?.clear();
              return;
            }
            searchApiRef.current?.find(query, backwards);
          }}
          onClose={closeSearch}
        />
      )}
      {status === "starting" && <div className="terminal-overlay">正在连接…</div>}
      {/* 连接失败的提示已经由上面的结束面板承担，这里不再叠一块居中遮罩 */}
      {pendingPaste !== null && (
        <PasteConfirmDialog text={pendingPaste} onConfirm={confirmPaste} onCancel={cancelPaste} />
      )}
    </div>
  );
});
