import { useCallback, useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import type { ISearchOptions } from "@xterm/addon-search";
import "@xterm/xterm/css/xterm.css";
import type { Protocol, Session } from "../api/session";
import { errorDetail, errorKind, errorMessage } from "../api/session";
import { clampFontSize } from "../api/settings";
import type { ThemeKind, ThemePalette } from "../lib/themes";
import { themeToXterm } from "../lib/themes";
import type { HostKeyInfo, SshAuthRequest, TerminalEvent } from "../api/terminal";
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
  /**
   * 终端建立/销毁时上报后端注册表里的终端 id。
   *
   * 远端指标要用这个 id 去查底层 SSH 连接（不是会话 id）；
   * 面板卸载或终端自然退出时上报 null，让上层停止对该会话采集。
   * 第三个参数是「要求注销的那个 id」：分屏下同一会话有两个终端，
   * 上层只在记录仍等于它时才删除，避免先退出的那一块把还活着的那块也注销掉。
   */
  onTerminalId?: (sessionId: string, terminalId: string | null, expectedId?: string) => void;
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

/**
 * 终端面板，本地与 SSH 共用。
 *
 * 参数：session 为要运行的会话，password 为 SSH 密码
 * 核心流程：挂载时创建 xterm → 按协议选择启动方式 → 双向转发数据 →
 * 监听容器尺寸同步 resize；卸载时关闭会话并回收实例。
 */
export function TerminalPanel({
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
  onTerminalId,
}: Props) {
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

  useEffect(() => {
    searchOpenRef.current = searchOpen;
  }, [searchOpen]);

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

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return undefined;

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
    fit.fit();

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

    // 鼠标按下/焦点进入都算「用户在操作这一块」-> 重新认领，抢回上报权
    host.addEventListener("pointerdown", claimActive, true);
    host.addEventListener("focusin", claimActive, true);
    const textarea = term.textarea;
    textarea?.addEventListener("focus", claimActive);

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

    // 后端推来的字节直接喂给 xterm -> 保留原始控制序列
    const handleEvent = (event: TerminalEvent) => {
      if (disposed) return;
      if (event.kind === "data") {
        term.write(base64ToBytes(event.chunk));
        return;
      }
      setStatus("exited");
      // 自然退出（exit / Ctrl+D / 远端断开）也要注销终端 id，否则上层会继续
      // 拿一个已死的 id 采集指标；终端内容保持原样，仍然显示「[会话已结束]」
      if (!exitReported) {
        exitReported = true;
        if (terminalId) onTerminalId?.(session.id, null, terminalId);
      }
      const suffix = event.code === null ? "" : ` (${event.code})`;
      term.write(`\r\n\x1b[38;5;244m[会话已结束${suffix}]\x1b[0m\r\n`);
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
        setStatus("running");
        onTerminalId?.(session.id, id);
        if (session.protocol === "local") {
          pollLocalCwd();
          cwdTimer = setInterval(pollLocalCwd, 1200);
        }
      })
      .catch((reason) => {
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

    // 键盘输入 -> 编码后写入会话
    const dataSubscription = term.onData((data) => {
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
      host.removeEventListener("pointerdown", claimActive, true);
      host.removeEventListener("focusin", claimActive, true);
      textarea?.removeEventListener("focus", claimActive);
      visibility.disconnect();
      if (cwdTimer !== null) clearInterval(cwdTimer);
      // 活动面板卸载 -> 让位，避免之后没有任何面板能上报目录
      if (activePaneToken === paneToken) activePaneToken = null;
      observer.disconnect();
      window.removeEventListener("resize", refit);
      dataSubscription.dispose();
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
      {searchOpen && (
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
      {status === "error" && <div className="terminal-overlay is-error">{error}</div>}
      {pendingPaste !== null && (
        <PasteConfirmDialog text={pendingPaste} onConfirm={confirmPaste} onCancel={cancelPaste} />
      )}
    </div>
  );
}
