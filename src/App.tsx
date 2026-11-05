import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api, errorDetail, errorKind, errorMessage } from "./api/session";
import {
  DEFAULT_SETTINGS,
  SETTING_KEYS,
  SFTP_WIDTH_MAX,
  SFTP_WIDTH_MIN,
  clampSftpWidth,
  parseSettings,
  resolveFontStack,
  serializeSetting,
  settingsGetAll,
  settingsSet,
} from "./api/settings";
import type { AppSettings } from "./api/settings";
import { themeListCustom } from "./api/theme";
import type { CustomThemeRecord } from "./api/theme";
import { applyTheme, cacheTheme, getTheme, BUILTIN_THEMES } from "./lib/themes";
import type { ThemePalette } from "./lib/themes";
import type { SidebarTab } from "./api/settings";
import { paletteFromRecord } from "./lib/themeImport";
import { isRemoteDesktop, isSshLike, isTerminalProtocol } from "./lib/protocols";
import { trustHostKey, forgetHostKey } from "./api/terminal";
import type { HostKeyInfo, SshAuthRequest } from "./api/terminal";
import { launchRemoteDesktop, remoteDesktopClients } from "./api/remoteDesktop";
import type { RemoteDesktopClient } from "./api/remoteDesktop";
import {
  credentialDelete,
  credentialList,
  credentialReveal,
  credentialSave,
  vaultLock,
  vaultSetup,
  vaultStatus,
  vaultUnlock,
} from "./api/vault";
import type { Credential, VaultStatus } from "./api/vault";
import type {
  ImportPreview,
  ImportReport,
  Session,
  SessionGroup,
  SessionInput,
} from "./api/session";
import { MenuBar } from "./components/MenuBar";
import type { MenuGroup } from "./components/MenuBar";
import { Toolbar } from "./components/Toolbar";
import type { ToolbarAction } from "./components/Toolbar";
import { TabBar } from "./components/TabBar";
import type { TabItem } from "./components/TabBar";
import { SessionTree } from "./components/SessionTree";
import { SessionDialog } from "./components/SessionDialog";
import { GroupPromptDialog } from "./components/GroupPromptDialog";
import { ImportPreviewDialog } from "./components/ImportPreviewDialog";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { AboutDialog } from "./components/AboutDialog";
import { SecretPromptDialog } from "./components/SecretPromptDialog";
import { VaultDialog } from "./components/VaultDialog";
import { HostKeyDialog } from "./components/HostKeyDialog";
import { KnownHostsDialog } from "./components/KnownHostsDialog";
import { TunnelDialog } from "./components/TunnelDialog";
import { TunnelFormDialog } from "./components/TunnelFormDialog";
import { CommandPalette } from "./components/CommandPalette";
import { tunnelConfigList, tunnelList, tunnelStart } from "./api/tunnel";
import type { TunnelConfig, TunnelInfo } from "./api/tunnel";
import { TerminalPanel } from "./components/TerminalPanel";
import { SftpPanel } from "./components/SftpPanel";
import { formatMemory, formatRate, remoteMetrics, shortenAddress, systemMetrics } from "./api/metrics";
import { remoteDisk } from "./api/sftp";
import type { RemoteDisk } from "./api/sftp";
import { formatCombo, matches } from "./lib/shortcuts";
import type { Command } from "./lib/commands";
import type { ShortcutActionId } from "./lib/shortcuts";
import type { GpuMetrics, MetricsScope, SystemMetrics } from "./api/metrics";
import { SidebarRail, SIDEBAR_RAIL } from "./components/SidebarRail";
import { SidebarResizer } from "./components/SidebarResizer";
import { ToolsPanel } from "./components/ToolsPanel";
import type { ToolDialogKind } from "./components/ToolsPanel";
import { MacrosPanel } from "./components/MacrosPanel";
import { SettingsDialog } from "./components/SettingsDialog";
import {
  IconActivity,
  IconChevronRight,
  IconCollapseAll,
  IconExpandAll,
  IconExport,
  IconFolder,
  IconImport,
  IconInfo,
  IconKey,
  IconSearch,
  IconServer,
  IconSessions,
  IconSplit,
  IconTerminal,
  IconTunnel,
} from "./components/icons";
import { groupPath } from "./lib/tree";
import "./App.css";

const COLLAPSED_KEY = "rustmobax.collapsedGroups";
/** 旧版把文件面板宽度写在 localStorage；这一版起改存设置键 sftp.width，
    这个常量只用于启动时做一次迁移 */
const LEGACY_SFTP_WIDTH_KEY = "rustmobax.sftpWidth";
/** 设置写库的防抖间隔（毫秒）：拖动滑块时只落最后一次 */
const SETTINGS_SAVE_DELAY = 300;
/** 凭据库没解锁（未初始化，或锁着但这批隧道不碰库）时，自动启动等应用安静下来再试的延迟（毫秒） */
const AUTO_START_FALLBACK_DELAY = 3000;

type DialogState =
  | { kind: "none" }
  | { kind: "session"; mode: "create" | "edit"; session: Session | null; draft?: Partial<SessionInput> }
  | { kind: "group"; mode: "create" | "rename"; group: SessionGroup | null }
  | { kind: "import"; preview: ImportPreview }
  | { kind: "vault" }
  | { kind: "knownHosts" }
  | { kind: "tunnel" }
  /** 隧道新建/编辑表单：config 为 null 表示新增，叠加在隧道列表之上 */
  | { kind: "tunnelForm"; config: TunnelConfig | null }
  | { kind: "settings" }
  | { kind: "about" };

type Notice = { tone: "info" | "error"; text: string };

/** RDP / VNC 会话的实际端口：没填就按协议回落到默认值，与后端 `effective_port()` 一致。 */
function remoteDesktopPort(session: Session): number {
  if (session.port) return session.port;
  return session.protocol === "vnc" ? 5900 : 3389;
}

/**
 * 没检测到任何远程桌面客户端时，按平台给出的安装引导。
 *
 * 无参数
 * 核心流程：用 WebView 的 UA 判断平台（它与 Tauri 运行平台一致，无需额外 IPC），
 * 返回一句说明加几条可直接复制的安装命令/应用名。
 */
function remoteDesktopInstallHint(): { intro: string; commands: string[] } {
  const ua = navigator.userAgent;
  if (/windows/i.test(ua)) {
    return {
      intro:
        "Windows 自带「远程桌面连接」(mstsc)，若未找到请在「设置 → 系统 → 可选功能」里添加后重新打开本会话。",
      commands: ["mstsc"],
    };
  }
  if (/mac os x|macintosh/i.test(ua)) {
    return {
      intro:
        "macOS 没有内置远程桌面客户端，请从 App Store 安装任一客户端后重新打开本会话。",
      commands: ["Microsoft Remote Desktop（App Store）", "VNC Viewer（App Store）"],
    };
  }
  return {
    intro: "请先安装一个远程桌面客户端（FreeRDP 或 Remmina），装好后重新打开本会话。",
    commands: [
      "sudo apt install freerdp2-x11",
      "sudo apt install remmina remmina-plugin-rdp remmina-plugin-vnc",
    ],
  };
}

/**
 * 指标面板的数据与来源。
 *
 * value 是数值本体（远端不可用时保留上一次远端值，但界面只显示占位符），
 * host 标明数字来自哪台机器，scope 决定界面怎么标注来源。
 */
type MetricsState = {
  value: SystemMetrics | null;
  host: string | null;
  scope: MetricsScope;
  /** 远端不可用时的原因摘要，本机口径为 null */
  reason: string | null;
  /** 远端 GPU 采样结果；本机口径或没有 GPU 时是空数组 */
  gpus: GpuMetrics[];
};

/**
 * 进度条按占用比例上色。
 *
 * 参数：percent 为 0 到 100 的占用率
 * 核心流程：七成以下用主色，七到九成用琥珀，再往上用告警色；
 * 颜色只是辅助，主数值本身始终可读。
 */
function barTone(percent: number): "normal" | "high" | "critical" {
  if (percent >= 90) return "critical";
  if (percent >= 70) return "high";
  return "normal";
}

/**
 * 把字节数格式化成磁盘容量文案。
 *
 * 参数：bytes 为字节数
 * 核心流程：换算成 MiB 后复用 formatMemory，磁盘与内存的量级口径保持一致。
 */
function formatDiskSize(bytes: number): string {
  return formatMemory(bytes / (1024 * 1024));
}

/** 单个指标块的入参：标题 + 主数值 + 可选进度条与副行 */
type MonitorCardProps = {
  /** 块标题：CPU / 内存 / 磁盘 / 网络 / GPU 名称 */
  title: string;
  /** 主数值，取不到时传「—」 */
  value: string;
  /** 副行说明（百分比、显存、温度等），不传则不渲染这一行 */
  detail?: string;
  /** 进度条百分比，不传（或传 null）表示这个块没有进度条，例如网络 */
  percent?: number | null;
  /** 悬停说明：写清数据来自哪台机器 */
  hint: string;
};

/**
 * 远程监控面板里的一个指标块。
 *
 * 参数：见 MonitorCardProps
 * 核心流程：标题与主数值等宽排版，进度条按占用率着色；
 * 高度由面板统一固定，块内不再自己撑高，横向排不下时由外层滚动。
 */
function MonitorCard({ title, value, detail, percent, hint }: MonitorCardProps) {
  const ratio = percent === null || percent === undefined ? null : Math.min(100, Math.max(0, percent));
  return (
    <div className={`monitor-card is-${ratio === null ? "flat" : barTone(ratio)}`} title={hint}>
      <span className="monitor-card-title">{title}</span>
      <span className="monitor-card-value">{value}</span>
      {ratio !== null && (
        <span className="monitor-bar">
          <span className="monitor-bar-fill" style={{ width: `${ratio}%` }} />
        </span>
      )}
      {detail !== undefined && <span className="monitor-card-detail">{detail}</span>}
    </div>
  );
}

/**
 * 读取系统配色偏好。
 *
 * 参数：无
 * 核心流程：matchMedia 不可用时按暗色处理，与缺省主题保持一致。
 */
function readSystemDark(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return true;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/**
 * 读取上次的分组折叠状态。
 *
 * 参数：无
 * 核心流程：从 localStorage 取 JSON，解析失败或缺失时退回空对象。
 */
function loadCollapsed(): Record<string, boolean> {
  try {
    const raw = localStorage.getItem(COLLAPSED_KEY);
    return raw ? (JSON.parse(raw) as Record<string, boolean>) : {};
  } catch {
    return {};
  }
}

/**
 * 解析 Quick connect 输入。
 *
 * 参数：raw 为形如 user@host:port 的输入
 * 核心流程：拆出用户名、主机与端口，格式不合法返回 null 由调用方提示。
 */
function parseQuickConnect(raw: string): Partial<SessionInput> | null {
  const trimmed = raw.trim();
  if (trimmed === "") return null;
  const matched = /^(?:([^@\s]+)@)?([^\s:]+)(?::(\d+))?$/.exec(trimmed);
  if (!matched) return null;
  const [, username, host, port] = matched;
  return {
    name: host,
    protocol: "ssh",
    host,
    port: port ? Number(port) : 22,
    username: username ?? "",
  };
}

/**
 * 取会话的连接目标标识。
 *
 * 参数：session 为会话
 * 核心流程：用 用户名@主机:端口 做 key，让同目标的 SSH 终端与 SFTP 面板
 * 能共用终端上报的工作目录（一个会话只能是一种协议，靠 id 对不上）。
 */
function targetKey(session: Session): string {
  return `${session.username ?? ""}@${session.host ?? ""}:${session.port ?? 22}`;
}

/**
 * 认证解析结果。
 *
 * 无参数
 * 核心流程：够用时直接给出认证；不够用时指明缺哪种口令，
 * 由调用方决定弹哪个输入框。
 */
type ResolvedSshAuth =
  | { kind: "ready"; auth: SshAuthRequest }
  | { kind: "needPassword" }
  | { kind: "needKeyPassphrase"; path: string; credentialId?: string };

/**
 * 判断一个会话启动时是否必须先解锁凭据库。
 *
 * 参数：session 为隧道背后的 SSH 会话
 * 核心流程：只有「要用的密码/口令存在库里」才需要解锁 —— 密码认证且记了
 * credentialId，或者私钥口令记了 passphraseCredentialId；
 * 纯密钥、无口令的会话不碰库，解锁与否都能连上。extra 解析不了就当不需要，
 * 免得为一个坏配置无谓地打断用户。
 */
function sessionNeedsVault(session: Session): boolean {
  if (!session.extra) return false;
  try {
    const parsed = JSON.parse(session.extra) as {
      credentialId?: string;
      passphraseCredentialId?: string;
    };
    if (session.authKind === "password" && (parsed.credentialId ?? "") !== "") return true;
    return (parsed.passphraseCredentialId ?? "") !== "";
  } catch {
    return false;
  }
}

/**
 * 自动启动的推进阶段。
 *
 * 用一个 ref 记住整个进程走到哪一步，保证探测、弹窗、启动各自只发生一次：
 * idle 还没探测；probing 正在读配置判断要不要先解锁；
 * awaitingVault 已主动弹出解锁框、等解锁后续上；declined 用户关掉了这次弹窗，
 * 本次会话不再自动弹（但用户自己解锁后仍允许跑一轮）；
 * done 已执行或已决定不执行，此后任何状态变化都不再推进。
 */
type AutoStartPhase = "idle" | "probing" | "awaitingVault" | "declined" | "done";

/**
 * 从会话配置解析出可用的认证方式。
 *
 * 参数：session 为 SSH 会话，known 为本次运行已输入的认证
 * 核心流程：优先用本次运行已输入的认证（私钥口令也在这里，用完即忘）；
 * 否则按会话保存的配置：私钥路径优先，其次是凭据库引用；
 * 私钥被标记为有口令而内存里还没有口令时，要求上层收一次口令，
 * 并把记在会话里的口令凭据引用一并带出去（上层据此直接取回明文）。
 */
function resolveSshAuth(
  session: Session,
  known: Record<string, SshAuthRequest>,
): ResolvedSshAuth {
  const existing = known[session.id];
  if (existing) return { kind: "ready", auth: existing };
  if (!session.extra) return { kind: "needPassword" };

  try {
    const parsed = JSON.parse(session.extra) as {
      privateKeyPath?: string;
      credentialId?: string;
      keyHasPassphrase?: boolean;
      /** 私钥口令的凭据引用：解密后由 credential_reveal 取回明文用于认证 */
      passphraseCredentialId?: string;
    };
    if (parsed.privateKeyPath) {
      if (parsed.keyHasPassphrase === true) {
        // 口令存过凭据库引用 -> 连同引用一起交出去，由 openSession 去取明文；
        // 这里保持同步，是因为文件分页在渲染期也会用它判断展示哪种占位
        return {
          kind: "needKeyPassphrase",
          path: parsed.privateKeyPath,
          credentialId: parsed.passphraseCredentialId,
        };
      }
      return { kind: "ready", auth: { kind: "privateKey", path: parsed.privateKeyPath } };
    }
    if (parsed.credentialId) {
      return { kind: "ready", auth: { kind: "credential", id: parsed.credentialId } };
    }
  } catch {
    // extra 不是合法 JSON -> 退回密码认证
  }
  return { kind: "needPassword" };
}

function App() {
  const [groups, setGroups] = useState<SessionGroup[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [keyword, setKeyword] = useState("");
  const [quickConnect, setQuickConnect] = useState("");
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>(loadCollapsed);
  const [selectedGroupId, setSelectedGroupId] = useState<string | null>(null);
  const [openTabIds, setOpenTabIds] = useState<string[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  /** 处于左右分屏的标签，最多两块面板 */
  const [splitTabIds, setSplitTabIds] = useState<string[]>([]);
  const [dialog, setDialog] = useState<DialogState>({ kind: "none" });
  /** 命令面板是否展开；面板自身在 open=false 时整棵子树不渲染 */
  const [paletteOpen, setPaletteOpen] = useState(false);
  /** 命令面板用的隧道配置缓存：只在面板打开时拉一次，不参与其它界面 */
  const [paletteTunnels, setPaletteTunnels] = useState<TunnelConfig[]>([]);
  /** 隧道表单保存成功后自增，用来让隧道列表重新拉一次配置 */
  const [tunnelRevision, setTunnelRevision] = useState(0);
  /**
   * 自动启动走到哪一步（见 AutoStartPhase）。
   *
   * 用 ref 而不是 state：整个进程生命周期只推进一轮，凭据库后续再变状态也不重弹窗、
   * 不重跑，更不能因为重渲染多拉起一条隧道。
   */
  const autoStartPhase = useRef<AutoStartPhase>("idle");
  /**
   * 启动探测弹解锁框时，等解锁后自动连接的隧道条数。
   *
   * 只用于在解锁框里说明「为什么要你输密码」，null 表示这次弹窗不是启动探测拉起来的。
   */
  const [autoStartVaultHint, setAutoStartVaultHint] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [confirmState, setConfirmState] = useState<{ message: string; run: () => void } | null>(
    null,
  );
  /** SSH 认证配置只存在内存 -> 关掉应用即失效 */
  const [sshAuths, setSshAuths] = useState<Record<string, SshAuthRequest>>({});
  const [pendingSsh, setPendingSsh] = useState<Session | null>(null);
  /** 私钥有口令、但本次运行还没收到口令的会话 */
  const [pendingKeyPass, setPendingKeyPass] = useState<{ session: Session; path: string } | null>(
    null,
  );
  /** 已确认过指纹的会话 -> 重连时带上 acceptUnknownHostKey */
  const [sshTrusted, setSshTrusted] = useState<Record<string, boolean>>({});
  const [hostKeyPrompt, setHostKeyPrompt] = useState<{
    /** 终端会话触发时带 sessionId，用于信任后重连；隧道触发时为 null */
    sessionId: string | null;
    info: HostKeyInfo;
    mode: "unknown" | "mismatch";
    /**
     * 调用方在等结果（隧道）时用它收尾：true=已信任（自己重试），false=取消/信任失败。
     * 终端走的是 sessionId + sshTrusted 那条既有路径，不带这个回调。
     */
    resolve?: (ok: boolean) => void;
  } | null>(null);
  /** 凭据库状态与凭据列表 */
  const [vault, setVault] = useState<VaultStatus>({ initialized: false, unlocked: false });
  const [credentials, setCredentials] = useState<Credential[]>([]);
  const [vaultError, setVaultError] = useState<string | null>(null);
  /** 因凭据库锁定而暂缓打开的会话 -> 解锁成功后自动续上 */
  const [pendingVaultOpen, setPendingVaultOpen] = useState<Session | null>(null);
  /**
   * 因凭据库锁定而暂缓启动的隧道 id 列表 -> 解锁成功后串行续跑一次。
   *
   * 用 ref 而不是 state：它只表示「有一批待办」，不需要参与渲染；
   * 「全部启动」批量被拦下时整批挂在这里，解锁后跑一遍就够，所以只会弹一次解锁框。
   * 与自动启动的 autoStartPhase 完全独立，两条流程各写各自的标记，互不清对方的账。
   */
  const pendingTunnelStarts = useRef<string[]>([]);
  /**
   * 这批待启动是用户从隧道列表点出来的 -> 解锁后把列表放回去。
   *
   * 命令面板发起的启动不写这个标记：面板那时已经关了，解锁后再强行弹列表会突兀。
   */
  const pendingTunnelStartsFromList = useRef(false);
  /** 凭据库弹窗是被密码/口令弹窗叫起来的 -> 解锁后只关弹窗，回到仍挂着的口令弹窗 */
  const [vaultResumePrompt, setVaultResumePrompt] = useState(false);
  /** 凭据库状态是否真的读到过一次：启动探测要等它，否则会把「还没读到」当成「未初始化」 */
  const [vaultReady, setVaultReady] = useState(false);
  /** 会话列表是否读到过一次：启动探测要用会话的认证方式判断要不要先解锁 */
  const [sessionsReady, setSessionsReady] = useState(false);
  /** 各连接目标（用户名@主机:端口）工作目录，由终端 OSC 7 上报，供文件分页跟随 */
  const [cwdByTarget, setCwdByTarget] = useState<Record<string, string>>({});
  /** 后端持久化的应用设置；按钮/滑块改动都先落到这里再写库 */
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  /** 导入的自定义主题列表，供主题网格与主题解析使用 */
  const [customThemes, setCustomThemes] = useState<CustomThemeRecord[]>([]);
  /** 系统配色是否偏暗，theme=system 时用它解析实际生效的主题 */
  const [systemDark, setSystemDark] = useState(readSystemDark);
  /** 启动阶段是否已读完设置与主题列表：两份都到位才第一次上色，避免覆盖首帧预读 */
  const [settingsReady, setSettingsReady] = useState(false);
  const [themesReady, setThemesReady] = useState(false);
  /** 左侧栏是否展开，标签栏右侧按钮、视图菜单与侧栏拖动条旁边都能切 */
  const [sidebarVisible, setSidebarVisible] = useState(true);
  /** 会话主区底部「远程监控」面板是否展开（只记在内存里，不持久化） */
  const [monitorOpen, setMonitorOpen] = useState(false);
  /** 指标：优先取当前活动 SSH 会话的远端主机，没有远端会话时才采本机 */
  /** 会话 id -> 后端终端注册表里的终端 id（远端指标要用它查底层 SSH 连接） */
  const [terminalIds, setTerminalIds] = useState<Record<string, string>>({});
  /** 活动远端会话的磁盘占用（单独 10 秒节流），null 表示取不到 -> 界面显示「—」 */
  const [disk, setDisk] = useState<RemoteDisk | null>(null);

  const [metrics, setMetrics] = useState<MetricsState>({
    value: null,
    host: null,
    scope: "local",
    reason: null,
    gpus: [],
  });
  /** 图标栏当前选中的分页 */
  const [sidebarTab, setSidebarTab] = useState<SidebarTab>(DEFAULT_SETTINGS.sidebarDefaultTab);
  /** 「文件」分页是否被打开过 -> 打开过就常驻，切分页只做 CSS 隐藏 */
  const [sftpVisited, setSftpVisited] = useState(false);
  /** 关掉自动连接后，用户手动点「连接」才建 SFTP 连接 */
  const [sftpManualConnect, setSftpManualConnect] = useState(false);
  /** 拖动侧栏时的临时宽度，null 表示没在拖（没拖时用设置里的值） */
  const [sidebarWidthLive, setSidebarWidthLive] = useState<number | null>(null);

  /** 各设置键的防抖定时器 -> 拖动滑块时同一个键只写一次库 */
  const saveTimers = useRef(new Map<string, number>());
  /** 设置只读一次 -> 默认分页只在启动完成后应用一次，不抢用户后来的选择 */
  const defaultTabApplied = useRef(false);

  /**
   * 持久化单个设置键。
   *
   * 参数：key 为后端约定键，value 为序列化后的值
   * 核心流程：同一个键 300ms 内只保留最后一次写入（避免拖动滑块刷爆库）；
   * 写失败只在状态栏提示 -> 界面保留当前值，不回滚，用户可再改一次重试。
   */
  const saveSetting = useCallback((key: string, value: string) => {
    const pending = saveTimers.current.get(key);
    if (pending !== undefined) window.clearTimeout(pending);
    const timer = window.setTimeout(() => {
      saveTimers.current.delete(key);
      void settingsSet(key, value).catch((err) => {
        setNotice({ tone: "error", text: `设置保存失败：${errorMessage(err)}` });
      });
    }, SETTINGS_SAVE_DELAY);
    saveTimers.current.set(key, timer);
  }, []);

  /**
   * 应用并持久化一项设置。
   *
   * 参数：field 为设置字段，value 为新值
   * 核心流程：先改状态让界面立刻生效，再把值序列化后按 key 写库；
   * 状态与键名的对应关系只维护在 SETTING_KEYS 一处。
   */
  const applySetting = useCallback(
    <K extends keyof AppSettings,>(field: K, value: AppSettings[K]) => {
      setSettings((prev) => ({ ...prev, [field]: value }));
      saveSetting(SETTING_KEYS[field], serializeSetting(value));
    },
    [saveSetting],
  );

  /**
   * 启动时载入全部设置。
   *
   * 参数：无
   * 核心流程：拉全部键 -> 顺带把 localStorage 里的旧面板宽度迁移到 sftp.width ->
   * parseSettings 校验并夹好区间后一次性应用；
   * 读取失败只提示，界面继续用 DEFAULT_SETTINGS。
   */
  const loadSettings = useCallback(async () => {
    let raw: Record<string, string>;
    try {
      raw = await settingsGetAll();
    } catch (err) {
      setNotice({ tone: "error", text: `读取设置失败，已使用默认值：${errorMessage(err)}` });
      setSettingsReady(true);
      return;
    }

    const next = parseSettings(raw);

    // 迁移逻辑：W1 版本把面板宽度写在 localStorage["rustmobax.sftpWidth"]，
    // 这里读一次搬到后端设置键，随后删掉旧键 -> 之后只认库里的 sftp.width
    try {
      const legacy = localStorage.getItem(LEGACY_SFTP_WIDTH_KEY);
      if (legacy !== null) {
        const legacyWidth = Number(legacy);
        // 库里已经有 sftp.width 时以库为准，旧值只做清理
        if (!(SETTING_KEYS.sftpWidth in raw) && Number.isFinite(legacyWidth)) {
          const width = clampSftpWidth(Math.max(legacyWidth, SFTP_WIDTH_MIN));
          next.sftpWidth = width;
          void settingsSet(SETTING_KEYS.sftpWidth, String(width)).catch(() => {
            // 迁移写失败不影响本次运行；旧键已删，下次用户拖动会重新落库
          });
        }
        localStorage.removeItem(LEGACY_SFTP_WIDTH_KEY);
      }
    } catch {
      // localStorage 不可用（隐私模式等）-> 没有可迁移的数据
    }

    setSettings(next);
    setSettingsReady(true);
  }, []);

  /**
   * 恢复默认设置。
   *
   * 参数：无
   * 核心流程：缺省值一次性套到状态上（界面立刻复原），再逐键写库；
   * 每个键各自防抖，写失败同样只提示。
   */
  const resetSettings = useCallback(() => {
    setSettings(DEFAULT_SETTINGS);
    for (const field of Object.keys(DEFAULT_SETTINGS) as (keyof AppSettings)[]) {
      saveSetting(SETTING_KEYS[field], serializeSetting(DEFAULT_SETTINGS[field]));
    }
  }, [saveSetting]);

  // 启动时读一次设置 -> 主题、缩放、终端字号都从这里来
  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);

  /**
   * 拉取自定义主题列表。
   *
   * 参数：无
   * 核心流程：导入/删除后都要重拉一次；读取失败只提示，界面继续用内置主题
   * （列表拉不到时设置里的 custom:<id> 会回落到暗色，不会白屏）。
   */
  const loadCustomThemes = useCallback(async () => {
    try {
      setCustomThemes(await themeListCustom());
    } catch (err) {
      setNotice({ tone: "error", text: `读取自定义主题失败：${errorMessage(err)}` });
    } finally {
      // 无论成败都放行首次上色 -> 后端没这条命令时也不会一直停在首帧缓存上
      setThemesReady(true);
    }
  }, []);

  // 启动时拉一次自定义主题，供主题网格与 theme=custom:<id> 的解析使用
  useEffect(() => {
    void loadCustomThemes();
  }, [loadCustomThemes]);

  // 监听系统配色：theme=system 时系统一换，实际主题立刻跟上
  useEffect(() => {
    if (typeof window.matchMedia !== "function") return undefined;
    const query = window.matchMedia("(prefers-color-scheme: dark)");
    const handleChange = (event: MediaQueryListEvent) => setSystemDark(event.matches);
    setSystemDark(query.matches);
    query.addEventListener("change", handleChange);
    return () => query.removeEventListener("change", handleChange);
  }, []);

  /** 实际生效的主题：system 交给「系统暗色用/系统浅色用」解析，再查注册表 */
  const themeById = useMemo(() => {
    const map = new Map<string, ThemePalette>();
    for (const record of customThemes) {
      const palette = paletteFromRecord(record);
      // payload 坏掉的记录直接跳过，不让一条脏数据拖垮整个主题列表
      if (palette) map.set(palette.id, palette);
    }
    return map;
  }, [customThemes]);

  /** 按 id 取主题：内置 -> 自定义 -> 缺省 dark（被删或还没拉到列表时用它兜底） */
  const resolveTheme = useCallback(
    (id: string): ThemePalette => getTheme(id) ?? themeById.get(id) ?? BUILTIN_THEMES[0],
    [themeById],
  );

  /** 当前生效主题：system 时按系统明暗分别取用户配好的那套 */
  const activeTheme = useMemo(() => {
    if (settings.theme === "system") {
      return resolveTheme(systemDark ? settings.themeSystemDark : settings.themeSystemLight);
    }
    return resolveTheme(settings.theme);
  }, [settings.theme, settings.themeSystemDark, settings.themeSystemLight, systemDark, resolveTheme]);

  // 把主题颜色写成 CSS 变量、把缩放写到 html 上 -> 组件不用关心主题；
  // data-theme-preference 只记录用户选择，便于排查 system 的解析结果；
  // 顺手写首帧缓存，index.html 的内联脚本下次启动直接用。
  // 启动阶段先等设置与主题列表都读完再上色：否则会用默认暗色把
  // index.html 预读出来的浅色覆盖掉，浅色用户还是会闪一下。
  const bootReady = settingsReady && themesReady;
  useEffect(() => {
    if (!bootReady) return;
    applyTheme(activeTheme);
    cacheTheme(activeTheme);
    const root = document.documentElement;
    root.dataset.themePreference = settings.theme;
    root.style.setProperty("--ui-scale", String(settings.uiScale));
  }, [bootReady, activeTheme, settings.theme, settings.uiScale]);

  /** 终端字体族：预设 id 在设置里，真正给 xterm 的是 CSS font-family 字符串 */
  const terminalFontFamily = resolveFontStack(settings.terminalFontFamily);
  /** 侧栏宽度：拖动时用临时值，松手后回落到设置里的 sftp.width */
  const sidebarWidth = sidebarWidthLive ?? settings.sftpWidth;
  /** 文件分页是否跟随终端目录（设置项 sftp.followCwd） */
  const followCwd = settings.sftpFollowCwd;

  /**
   * 重新拉取分组与会话。
   *
   * 参数：search 为搜索关键字
   * 核心流程：并发请求两个列表，失败只提示不抛白屏。
   */
  const reload = useCallback(async (search: string) => {
    try {
      const [groupList, sessionList] = await Promise.all([
        api.listGroups(),
        api.listSessions(search),
      ]);
      setGroups(groupList);
      setSessions(sessionList);
    } catch (err) {
      setNotice({ tone: "error", text: errorMessage(err) });
    } finally {
      // 成败都放行：读不到会话列表时启动探测按「判断不出要用凭据库」处理，
      // 不会一直卡在等就绪上
      setSessionsReady(true);
    }
  }, []);

  // 关键字变化后延迟 150ms 再查 -> 避免每敲一个字都打一次库
  useEffect(() => {
    const timer = window.setTimeout(() => {
      void reload(keyword);
    }, 150);
    return () => window.clearTimeout(timer);
  }, [keyword, reload]);

  // 会话被删后关掉对应标签 -> 避免标签指向已不存在的会话
  useEffect(() => {
    setOpenTabIds((prev) => prev.filter((id) => sessions.some((item) => item.id === id)));
    setSplitTabIds((prev) => prev.filter((id) => sessions.some((item) => item.id === id)));
  }, [sessions]);

  /**
   * 记录分组折叠状态。
   *
   * 参数：id 为分组标识
   * 核心流程：翻转单个分组并写回 localStorage，重启后仍保留。
   */
  const toggleGroup = (id: string) => {
    setCollapsed((prev) => {
      const next = { ...prev, [id]: !prev[id] };
      localStorage.setItem(COLLAPSED_KEY, JSON.stringify(next));
      return next;
    });
  };

  /**
   * 批量设置折叠状态。
   *
   * 参数：value 为 true 表示全部折叠
   * 核心流程：遍历现有分组生成映射并持久化。
   */
  const setAllCollapsed = (value: boolean) => {
    const next: Record<string, boolean> = {};
    for (const group of groups) {
      next[group.id] = value;
    }
    setCollapsed(next);
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify(next));
  };

  /**
   * 统一包装写操作。
   *
   * 参数：action 为动作，successText 为成功提示
   * 核心流程：置忙 → 执行 → 提示并刷新 → 失败只提示不中断界面。
   */
  const runAction = async (action: () => Promise<void>, successText?: string) => {
    setBusy(true);
    setNotice(null);
    try {
      await action();
      if (successText) setNotice({ tone: "info", text: successText });
      await reload(keyword);
    } catch (err) {
      setNotice({ tone: "error", text: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  /**
   * 重新读取凭据库状态与凭据列表。
   *
   * 参数：无
   * 核心流程：锁定态也照样拉列表（列表只有元信息），失败只提示不阻塞界面。
   */
  const reloadVault = useCallback(async () => {
    try {
      const [status, list] = await Promise.all([vaultStatus(), credentialList()]);
      setVault(status);
      setCredentials(list);
    } catch (err) {
      setVaultError(errorMessage(err));
    } finally {
      // 读失败也放行：启动探测会把「没读到状态」当成未初始化，走延迟兜底尝试
      setVaultReady(true);
    }
  }, []);

  // 启动时读一次凭据库状态 -> 会话是否能用已保存的密码取决于它
  useEffect(() => {
    void reloadVault();
  }, [reloadVault]);

  /**
   * 激活会话标签。
   *
   * 参数：session 为要打开的会话
   * 核心流程：已打开则切过去，否则追加到标签末尾并激活。
   */
  const activateSession = (session: Session) => {
    setOpenTabIds((prev) => (prev.includes(session.id) ? prev : [...prev, session.id]));
    setActiveTabId(session.id);
  };

  /**
   * 从凭据库取回私钥口令明文。
   *
   * 参数：credentialId 为会话 extra 里记下的口令凭据引用
   * 核心流程：没有引用或库还锁着就直接放弃（不打扰用户，退回手输口令）；
   * 取回的明文只塞进本次认证的内存结构，不写日志、不落库、不回显界面。
   */
  const revealStoredPassphrase = async (
    credentialId: string | undefined,
  ): Promise<string | null> => {
    if (!credentialId || !vault.unlocked) return null;
    try {
      return await credentialReveal(credentialId);
    } catch {
      // 凭据被删或密文损坏 -> 当作没有，退回手动输入口令
      return null;
    }
  };

  /**
   * 打开会话标签。
   *
   * 参数：session 为要打开的会话
   * 核心流程：先定认证方式；缺密码或私钥口令时先弹对应的口令框
   * （私钥口令存过凭据库引用时直接取回明文，不再打扰用户）；
   * 走凭据库但库还锁着时先解锁，解锁后自动续上这次打开。
   */
  const openSession = async (session: Session) => {
    // SSH 与 SFTP 都需要认证方式，本地终端不用
    if (isSshLike(session.protocol)) {
      const resolved = resolveSshAuth(session, sshAuths);
      if (resolved.kind === "needPassword") {
        // 没有可用认证 -> 先要密码
        setPendingSsh(session);
        return;
      }
      if (resolved.kind === "needKeyPassphrase") {
        // 私钥带口令：存过凭据库引用就直接取回明文，取不到才让用户手输
        const passphrase = await revealStoredPassphrase(resolved.credentialId);
        if (passphrase !== null) {
          setSshAuths((prev) => ({
            ...prev,
            [session.id]: { kind: "privateKey", path: resolved.path, passphrase },
          }));
          activateSession(session);
          return;
        }
        setPendingKeyPass({ session, path: resolved.path });
        return;
      }
      if (resolved.auth.kind === "credential" && !vault.unlocked) {
        setVaultResumePrompt(false);
        setPendingVaultOpen(session);
        setVaultError(null);
        setDialog({ kind: "vault" });
        return;
      }
      setSshAuths((prev) => ({ ...prev, [session.id]: resolved.auth }));
    }
    activateSession(session);
  };

  /**
   * 凭据库解锁/初始化成功后的收尾。
   *
   * 参数：无
   * 核心流程：两种来源分开处理 ——
   * 从密码/口令弹窗点进来的，只关掉凭据库弹窗，回到仍挂着的口令弹窗；
   * 从「打开会话时发现库锁着」点进来的，取出暂存的会话，
   * 用它的凭据引用（或私钥口令凭据）登记认证方式并激活标签。
   */
  const resumePendingSession = async () => {
    if (vaultResumePrompt) {
      // 解锁是从口令弹窗里点出来的 -> 只收掉凭据库弹窗，
      // 口令弹窗一直挂载着，用户已经输入的密码/口令与凭据名称都还在
      setVaultResumePrompt(false);
      setDialog({ kind: "none" });
      return;
    }
    const target = pendingVaultOpen;
    if (!target) return;
    setPendingVaultOpen(null);
    setDialog({ kind: "none" });
    const resolved = resolveSshAuth(target, sshAuths);
    if (resolved.kind === "ready") {
      setSshAuths((prev) => ({ ...prev, [target.id]: resolved.auth }));
    } else if (resolved.kind === "needKeyPassphrase") {
      const passphrase = await revealStoredPassphrase(resolved.credentialId);
      if (passphrase === null) {
        setPendingKeyPass({ session: target, path: resolved.path });
        return;
      }
      setSshAuths((prev) => ({
        ...prev,
        [target.id]: { kind: "privateKey", path: resolved.path, passphrase },
      }));
    }
    activateSession(target);
  };

  /**
   * 从密码 / 私钥口令弹窗里请求凭据库。
   *
   * 参数：无
   * 核心流程：只置标记并打开凭据库弹窗（未初始化时它自己会切到设置主密码）；
   * 口令弹窗保持挂载，用户已输入的密码与凭据名称都留在原处，
   * 解锁/初始化成功后 resumePendingSession 只收掉凭据库弹窗，直接回到它。
   */
  const requestVaultForPrompt = () => {
    setVaultResumePrompt(true);
    setVaultError(null);
    setDialog({ kind: "vault" });
  };

  /**
   * 把凭据引用合并进会话的 extra。
   *
   * 参数：target 为要更新的会话，patch 为要合并进 extra 的字段
   * 核心流程：解析现有 extra 并合并 -> 用当前会话对象的完整字段调 update_session
   * （authKind 等保持原样，只动 extra）-> 刷新列表让树上的数据同步；
   * 失败时返回原因文案，由调用方提示，绝不阻塞本次连接。
   */
  const persistSessionExtra = async (
    target: Session,
    patch: Record<string, string>,
  ): Promise<string | null> => {
    let extra: Record<string, unknown> = {};
    if (target.extra) {
      try {
        const parsed: unknown = JSON.parse(target.extra);
        if (typeof parsed === "object" && parsed !== null) {
          extra = parsed as Record<string, unknown>;
        }
      } catch {
        // extra 是脏数据 -> 从空对象重建，只保留本次写入的引用
      }
    }
    const input: SessionInput = {
      name: target.name,
      groupId: target.groupId,
      protocol: target.protocol,
      host: target.host,
      port: target.port,
      username: target.username,
      authKind: target.authKind,
      extra: JSON.stringify({ ...extra, ...patch }),
    };
    try {
      await api.updateSession(target.id, input);
      await reload(keyword);
      return null;
    } catch (err) {
      return errorMessage(err);
    }
  };

  /**
   * 收下用户输入的口令并打开会话。
   *
   * 参数：session 为发起连接的会话，secret 为口令明文，
   *       directAuth 为只存在内存的认证方式，
   *       save 为勾选入库后的「凭据分类 + 名称」（null 表示不入库）
   * 核心流程：勾选入库时先加密写库拿到凭据标识，密码认证顺带改成引用凭据，
   * 并把引用回写进会话 extra，让重启后也能直接用；
   * 回写失败只提示、不阻塞连接，最后关闭弹窗并激活标签。
   */
  const acceptSecret = async (
    session: Session,
    secret: string,
    directAuth: SshAuthRequest,
    save: { kind: string; label: string } | null,
  ) => {
    let auth: SshAuthRequest = directAuth;
    if (save) {
      setBusy(true);
      try {
        const id = await credentialSave(save.kind, save.label, secret);
        if (directAuth.kind === "password") {
          // 密码认证换成凭据引用，并把引用写回会话 -> 下次启动不再问密码
          auth = { kind: "credential", id };
          const failure = await persistSessionExtra(session, { credentialId: id });
          setNotice(
            failure === null
              ? { tone: "info", text: "已存入凭据库并保存到会话设置，下次启动直接连接" }
              : { tone: "error", text: `已存入凭据库，但会话设置未保存：${failure}` },
          );
        } else {
          // 私钥口令：记下凭据引用，下次连接由前端按 id 取回明文自动带上
          const failure = await persistSessionExtra(session, { passphraseCredentialId: id });
          setNotice(
            failure === null
              ? { tone: "info", text: `已存入凭据库「${save.label}」，下次连接自动使用` }
              : { tone: "error", text: `已存入凭据库，但会话设置未保存：${failure}` },
          );
        }
      } catch (err) {
        setNotice({ tone: "error", text: `凭据未保存：${errorMessage(err)}` });
      } finally {
        setBusy(false);
      }
      await reloadVault();
    }
    setSshAuths((prev) => ({ ...prev, [session.id]: auth }));
    setPendingSsh(null);
    setPendingKeyPass(null);
    activateSession(session);
  };

  /**
   * 统一包装凭据库操作。
   *
   * 参数：action 为动作，after 为成功后的收尾
   * 核心流程：置忙 → 执行 → 刷新状态 → 收尾；失败留在弹窗里显示原因。
   */
  const runVaultAction = async (
    action: () => Promise<void>,
    after?: () => void | Promise<void>,
  ) => {
    setBusy(true);
    setVaultError(null);
    try {
      await action();
      await reloadVault();
      await after?.();
    } catch (err) {
      setVaultError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  /**
   * 关闭标签。
   *
   * 参数：id 为会话标识
   * 核心流程：移除标签，若关的是当前标签则切到相邻项。
   */
  const closeTab = (id: string) => {
    setSplitTabIds((prev) => prev.filter((item) => item !== id));
    setOpenTabIds((prev) => {
      const next = prev.filter((item) => item !== id);
      if (activeTabId === id) {
        const index = prev.indexOf(id);
        const fallback = next[Math.min(index, next.length - 1)] ?? null;
        setActiveTabId(fallback);
      }
      return next;
    });
  };

  /**
   * 切换标签分屏。
   *
   * 参数：id 为会话标识
   * 核心流程：第一次点击给当前标签加第二块面板，再次点击收回单栏；
   * 分屏列表由顶层维护，关闭标签时一并清理。
   */
  const toggleSplit = (id: string) => {
    setSplitTabIds((prev) =>
      prev.includes(id) ? prev.filter((item) => item !== id) : [...prev, id],
    );
  };

  /**
   * 切换侧栏分页。
   *
   * 参数：tab 为目标分页
   * 核心流程：只改选中项；第一次进「文件」分页时把 sftpVisited 置真，
   * 之后该分页常驻（切走只做 CSS 隐藏，不卸载组件，SFTP 连接保留）。
   */
  const selectSidebarTab = (tab: SidebarTab) => {
    setSidebarTab(tab);
    if (tab === "sftp") setSftpVisited(true);
  };

  /**
   * 拖动中更新侧栏宽度。
   *
   * 参数：next 为拖动条算出的宽度（已由拖动条夹进区间）
   * 核心流程：只写临时宽度 -> 拖动期间不碰设置，避免每移动一像素写一次库。
   */
  const previewSidebarWidth = useCallback((next: number) => {
    setSidebarWidthLive(next);
  }, []);

  /**
   * 提交侧栏宽度。
   *
   * 参数：next 为松手或键盘微调后的宽度
   * 核心流程：先清掉临时值让界面回落到设置值，再按 sftp.width 落库
   * （键名沿用旧值，不新增后端键）；值没变时不写库。
   */
  const commitSidebarWidth = useCallback(
    (next: number) => {
      setSidebarWidthLive(null);
      if (next !== settings.sftpWidth) applySetting("sftpWidth", next);
    },
    [applySetting, settings.sftpWidth],
  );

  /**
   * 保存会话表单。
   *
   * 参数：input 为表单内容
   * 核心流程：按弹窗模式新建或更新，成功后关闭弹窗并打开该会话标签。
   */
  const submitSession = async (input: SessionInput) => {
    const current = dialog;
    if (current.kind !== "session") return;
    setBusy(true);
    setNotice(null);
    try {
      const saved =
        current.mode === "create"
          ? await api.createSession(input)
          : await api.updateSession(current.session!.id, input);
      setDialog({ kind: "none" });
      setNotice({ tone: "info", text: `已保存「${saved.name}」` });
      await reload(keyword);
      openSession(saved);
    } catch (err) {
      setNotice({ tone: "error", text: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  /**
   * 提交分组名称。
   *
   * 参数：name 为分组名
   * 核心流程：新建时挂到当前选中分组下，重命名时改原分组。
   */
  const submitGroup = async (name: string) => {
    const current = dialog;
    if (current.kind !== "group") return;
    setBusy(true);
    try {
      if (current.mode === "create") {
        const created = await api.createGroup(name, selectedGroupId);
        setSelectedGroupId(created.id);
      } else {
        await api.renameGroup(current.group!.id, name);
      }
      setDialog({ kind: "none" });
      setNotice({ tone: "info", text: current.mode === "create" ? "分组已创建" : "分组已重命名" });
      await reload(keyword);
    } catch (err) {
      setNotice({ tone: "error", text: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  /**
   * 删除分组并说明影响。
   *
   * 参数：group 为待删除分组
   * 核心流程：先讲清会话与子分组会上移，确认后再删除。
   */
  const askDeleteGroup = (group: SessionGroup) => {
    const affected = sessions.filter((session) => session.groupId === group.id).length;
    const children = groups.filter((item) => item.parentId === group.id).length;
    setConfirmState({
      message:
        `删除分组「${group.name}」？组内 ${affected} 个会话与 ${children} 个子分组` +
        `会移到上一级，会话本身不会被删除。`,
      run: () => {
        void runAction(async () => {
          await api.deleteGroup(group.id);
          if (selectedGroupId === group.id) setSelectedGroupId(group.parentId);
        }, "分组已删除");
      },
    });
  };

  /**
   * 删除会话。
   *
   * 参数：session 为待删除会话
   * 核心流程：二次确认后删除，标签会随会话消失自动清理。
   */
  const askDeleteSession = (session: Session) => {
    setConfirmState({
      message: `删除会话「${session.name}」？该操作不可撤销。`,
      run: () => {
        void runAction(async () => {
          await api.deleteSession(session.id);
        }, "会话已删除");
      },
    });
  };

  /**
   * 复制会话。
   *
   * 参数：session 为源会话
   * 核心流程：后端派生不冲突名称，复制后打开新会话标签。
   */
  const copySession = async (session: Session) => {
    setBusy(true);
    try {
      const copy = await api.copySession(session.id);
      setNotice({ tone: "info", text: `已复制为「${copy.name}」` });
      await reload(keyword);
      openSession(copy);
    } catch (err) {
      setNotice({ tone: "error", text: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  /**
   * Quick connect 回车处理。
   *
   * 参数：raw 为用户输入
   * 核心流程：解析成会话草稿 → 打开新建弹窗预填；解析失败给出提示。
   */
  const handleQuickConnect = (raw: string) => {
    const draft = parseQuickConnect(raw);
    if (!draft) {
      setNotice({ tone: "error", text: "格式应为 user@host:port" });
      return;
    }
    setQuickConnect("");
    setDialog({ kind: "session", mode: "create", session: null, draft });
  };

  /**
   * 选择 MobaXterm 文件并生成导入预览。
   *
   * 参数：无
   * 核心流程：选文件 → 读全文 → 后端只解析 → 弹预览等待确认。
   */
  const startImport = async () => {
    try {
      const picked = await openDialog({
        multiple: false,
        filters: [{ name: "MobaXterm 会话", extensions: ["mxtsessions", "ini", "txt"] }],
      });
      if (typeof picked !== "string") return;
      const content = await api.readTextFile(picked);
      const preview = await api.previewImport(content);
      setDialog({ kind: "import", preview });
    } catch (err) {
      setNotice({ tone: "error", text: errorMessage(err) });
    }
  };

  /**
   * 落地导入。
   *
   * 参数：preview 为已确认的预览
   * 核心流程：写库 → 汇总结果 → 关闭弹窗 → 刷新列表。
   */
  const confirmImport = async (preview: ImportPreview) => {
    setBusy(true);
    try {
      const report: ImportReport = await api.applyImport(preview);
      setDialog({ kind: "none" });
      const skipped = report.skipped.length;
      setNotice({
        tone: "info",
        text: `导入 ${report.imported} 条会话，新建 ${report.groupsCreated} 个分组${
          skipped > 0 ? `，跳过 ${skipped} 条` : ""
        }`,
      });
      await reload(keyword);
    } catch (err) {
      setNotice({ tone: "error", text: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  /**
   * 导出会话库。
   *
   * 参数：无
   * 核心流程：选保存路径 → 后端生成脱敏 JSON → 写盘 → 提示路径。
   */
  const startExport = async () => {
    try {
      const target = await saveDialog({
        defaultPath: "rustmobax-sessions.json",
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!target) return;
      const content = await api.exportSessions();
      await api.writeTextFile(target, content);
      setNotice({ tone: "info", text: `已导出到 ${target}` });
    } catch (err) {
      setNotice({ tone: "error", text: errorMessage(err) });
    }
  };

  /**
   * 把会话拖到目标分组。
   *
   * 参数：sessionId 为会话标识，groupId 为目标分组（null 表示未分组）
   * 核心流程：交给后端持久化后刷新列表；失败只提示不打断界面。
   */
  const moveSessionTo = async (sessionId: string, groupId: string | null) => {
    await runAction(async () => {
      await api.moveSession(sessionId, groupId);
    });
  };

  /**
   * 把分组拖到新的父级。
   *
   * 参数：groupId 为分组标识，parentId 为目标父级（null 表示根）
   * 核心流程：环检测在后端完成，命中时会带回可读错误。
   */
  const moveGroupTo = async (groupId: string, parentId: string | null) => {
    await runAction(async () => {
      await api.moveGroup(groupId, parentId);
    });
  };

  /**
   * 记下终端上报的工作目录。
   *
   * 参数：source 为上报的会话，next 为绝对路径
   * 核心流程：按连接目标归档；路径没变时保持原对象，
   * 避免 SFTP 面板被无意义地重列目录。
   */
  const handleCwdChange = useCallback((source: Session, next: string) => {
    const key = targetKey(source);
    setCwdByTarget((prev) => (prev[key] === next ? prev : { ...prev, [key]: next }));
  }, []);

  /**
   * 未知主机 -> 弹指纹确认。
   *
   * 参数：sessionId 为触发的会话，info 为观察到的指纹
   * 核心流程：用固定引用避免 TerminalPanel 的 effect 反复重建。
   */
  const handleHostKeyPrompt = useCallback((sessionId: string, info: HostKeyInfo) => {
    setHostKeyPrompt({ sessionId, info, mode: "unknown" });
  }, []);

  /**
   * 指纹变更 -> 弹硬阻断提示。
   *
   * 参数：sessionId 为触发的会话，info 为观察到的指纹
   * 核心流程：与未知主机分开处理，不允许直接信任。
   */
  const handleHostKeyMismatch = useCallback((sessionId: string, info: HostKeyInfo) => {
    setHostKeyPrompt({ sessionId, info, mode: "mismatch" });
  }, []);

  /**
   * 隧道等外部调用方请求指纹确认。
   *
   * 参数：info 为后端报上来的指纹
   * 核心流程：复用同一个 HostKeyDialog，用 Promise 把「信任 / 取消」回给调用方；
   * resolve(true) 后不触发终端重连，由调用方自己重试 tunnel_start。
   */
  const requestHostKeyTrust = useCallback(
    (info: HostKeyInfo) =>
      new Promise<boolean>((resolve) => {
        setHostKeyPrompt({ sessionId: null, info, mode: "unknown", resolve });
      }),
    [],
  );

  /**
   * 启动隧道的前置检查：需要凭据库但还锁着时，先把解锁框请出来。
   *
   * 参数：ids 为本次想启动的隧道配置 id（单条传一个元素的数组，批量传整批）；
   * options.fromList 表示这次启动来自隧道列表，解锁后要把列表放回去。
   * 核心流程：库已解锁直接放行（不读配置、不打扰用户）；否则拉一次配置，
   * 按 config.sessionId 找到会话，用 sessionNeedsVault 判断这些隧道里是否有要取库里的
   * 密码/私钥口令的。有且库还锁着（未初始化也算）就记下整批待启动的 id、弹一次解锁框并
   * 返回 true —— 调用方立即返回，既不发起 tunnel_start，也不写错误提示。
   * 记的是整批而不是只记「需要库的那几条」：列表被解锁框顶掉后没人能接着跑剩下的，
   * 只记一部分会让用户点了「全部启动」却只起来一半。
   * 配置读不到、或会话已被删（配置成了孤儿）时返回 false，交回原有报错路径，
   * 后端会给出明确原因，不会为一个不存在的会话误弹解锁框。
   */
  const deferTunnelStartsForVault = useCallback(
    async (ids: string[], options?: { fromList?: boolean }): Promise<boolean> => {
      if (vault.unlocked || ids.length === 0) return false;
      let configs: TunnelConfig[];
      try {
        configs = await tunnelConfigList();
      } catch (err) {
        console.warn("启动前置检查：读取隧道配置失败，按原路径启动", err);
        return false;
      }
      const needsVault = ids.some((id) => {
        const config = configs.find((item) => item.id === id);
        if (!config) return false;
        const session = sessions.find((item) => item.id === config.sessionId);
        return session ? sessionNeedsVault(session) : false;
      });
      if (!needsVault) return false;
      pendingTunnelStarts.current = [...ids];
      pendingTunnelStartsFromList.current = options?.fromList === true;
      setVaultError(null);
      setDialog({ kind: "vault" });
      return true;
    },
    [vault.unlocked, sessions],
  );

  /**
   * 从命令面板启动一条隧道。
   *
   * 参数：id 为隧道配置 id，options.silent 表示「不是用户主动点的」（自动启动用）：
   * 全程不写状态栏、不弹指纹确认，失败只落 console.warn。
   * 核心流程：与隧道列表弹窗同一套语义（只读参考 TunnelDialog 的私有实现，不 import）：
   * 先直接 tunnel_start；只有 kind=hostKeyUnknown 且带 detail 时复用 requestHostKeyTrust
   * 弹指纹确认，用户信任后重试一次，重试再失败只报原始错误；用户取消则停在取消文案。
   * 结果统一写状态栏 notice（项目没有 toast，也不用原生 alert）并同时落 console.error；
   * silent 时只留控制台，避免应用启动阶段突然弹窗或改状态栏打扰用户。
   * 用户主动启动（非 silent）时先过一道前置检查：目标会话要从保险库取密码/口令而库
   * 还锁着，就改成弹解锁框并在解锁后由续跑 effect 重新调一次本函数，而不是直接报错。
   */
  const startTunnel = useCallback(
    async (id: string, options?: { silent?: boolean }) => {
      const silent = options?.silent === true;
      if (!silent && (await deferTunnelStartsForVault([id]))) return;
      try {
        const runtime = await tunnelStart(id);
        if (silent) return;
        setNotice({
          tone: "info",
          text: `隧道已启动：${runtime.name ? `${runtime.name} ` : ""}${runtime.listenHost}:${runtime.listenPort}`,
        });
      } catch (err) {
        const detail = errorKind(err) === "hostKeyUnknown" ? errorDetail<HostKeyInfo>(err) : null;
        if (silent) {
          // 自动启动不弹指纹确认：用户没在等这次连接，突然弹窗只会被打断
          console.warn("隧道自动启动失败", id, detail ? "主机指纹未知，需手动启动确认" : err);
          return;
        }
        if (!detail) {
          console.error("隧道启动失败", id, err);
          setNotice({ tone: "error", text: `隧道启动失败：${errorMessage(err)}` });
          return;
        }
        const trusted = await requestHostKeyTrust(detail);
        if (!trusted) {
          setNotice({ tone: "error", text: "已取消，未信任主机指纹" });
          return;
        }
        try {
          const retried = await tunnelStart(id);
          setNotice({
            tone: "info",
            text: `隧道已启动：${retried.name ? `${retried.name} ` : ""}${retried.listenHost}:${retried.listenPort}`,
          });
        } catch (retryErr) {
          console.error("隧道启动重试失败", id, retryErr);
          setNotice({ tone: "error", text: `隧道启动失败：${errorMessage(retryErr)}` });
        }
      }
    },
    [requestHostKeyTrust, deferTunnelStartsForVault],
  );

  /**
   * 自动启动一轮：把所有标了 autoStart 的隧道按顺序拉起来。
   *
   * 参数：无
   * 核心流程：先拉配置与运行态；已经在跑的跳过（用户的会话不该被重启），
   * 其余逐条串行 await（同时建多条 SSH 会把凭据库/网络打满）。
   * 单条失败只 console.warn 继续下一条；全程不写状态栏、不弹窗：用户没主动点，
   * 失败静默跳过才不打扰人，全部成功也不提示。
   */
  const runAutoStartTunnels = useCallback(async () => {
    let configs: TunnelConfig[];
    try {
      configs = await tunnelConfigList();
    } catch (err) {
      console.warn("自动启动：读取隧道配置失败，跳过本轮", err);
      return;
    }

    const targets = configs.filter((config) => config.autoStart === true);
    if (targets.length === 0) return;

    // 运行态读不到就当作都没在跑：后端对已运行的隧道会自己报错，静默跳过即可
    let runtime: TunnelInfo[] = [];
    try {
      runtime = await tunnelList();
    } catch (err) {
      console.warn("自动启动：读取隧道运行态失败，仍尝试启动", err);
    }
    const running = new Set(runtime.filter((item) => item.running).map((item) => item.id));

    for (const config of targets) {
      if (running.has(config.id)) continue;
      await startTunnel(config.id, { silent: true });
    }
  }, [startTunnel]);

  /**
   * 启动探测 + 自动启动：有自动启动的隧道、而凭据库还锁着时，主动请用户解锁。
   *
   * 参数：无（取自 settings / sessions / vault 状态）
   * 核心流程：
   * 1. 等设置、会话列表、凭据库状态都读到过再动手 —— 否则会把「还没读到」
   *    当成「没配自动启动 / 没设过主密码」；
   * 2. 拉隧道配置，没有 autoStart === true 的直接收工（不弹、不提示、不启动）；
   * 3. 有的话按配置里的 sessionId 找到会话，用 sessionNeedsVault 判断这批隧道
   *    要不要用库里的密码/私钥口令：
   *    - 需要且库锁着 -> 弹解锁框，阶段停在 awaitingVault，解锁成功后由下面那个
   *      effect 续上自动启动（用户就知道为什么要输密码了）；
   *    - 不需要（纯密钥且无口令）或库已解锁 -> 直接跑，不弹窗；
   *    - 未初始化（没设过主密码）-> 不弹，退回延迟几秒的兜底尝试，静默失败即可。
   * 幂等靠 autoStartPhase 这个 ref：只有 idle 能进入探测，探测过之后任何重渲染
   * 都不再弹窗、不再重复启动。
   */
  useEffect(() => {
    if (autoStartPhase.current !== "idle") return;
    if (!settingsReady || !vaultReady || !sessionsReady) return;
    autoStartPhase.current = "probing";
    void (async () => {
      let targets: TunnelConfig[];
      try {
        const configs = await tunnelConfigList();
        targets = configs.filter((config) => config.autoStart === true);
      } catch (err) {
        console.warn("自动启动：读取隧道配置失败，跳过本轮", err);
        autoStartPhase.current = "done";
        return;
      }

      // 没有自动启动的隧道 -> 什么都不做，用户看不到任何变化
      if (targets.length === 0) {
        autoStartPhase.current = "done";
        return;
      }

      // 隧道只记 sessionId，会话的认证方式才说明要不要从库里取密码/口令
      const needsVault = targets.some((config) => {
        const session = sessions.find((item) => item.id === config.sessionId);
        return session ? sessionNeedsVault(session) : false;
      });

      if (vault.initialized && !vault.unlocked && needsVault) {
        // 主动弹解锁框：这次是应用自己请用户输密码，所以带上条数说明
        autoStartPhase.current = "awaitingVault";
        setAutoStartVaultHint(targets.length);
        setVaultError(null);
        setDialog({ kind: "vault" });
        return;
      }

      autoStartPhase.current = "done";
      if (vault.unlocked) {
        void runAutoStartTunnels();
        return;
      }
      // 没设过主密码或这批隧道不碰库：等应用安静下来再试一次，失败静默跳过
      window.setTimeout(() => void runAutoStartTunnels(), AUTO_START_FALLBACK_DELAY);
    })();
  }, [
    settingsReady,
    vaultReady,
    sessionsReady,
    sessions,
    vault.initialized,
    vault.unlocked,
    runAutoStartTunnels,
  ]);

  /**
   * 解锁成功后把自动启动续上。
   *
   * 参数：无（取自 vault.unlocked 与 autoStartVaultHint）
   * 核心流程：阶段停在 awaitingVault（启动探测弹过解锁框）时，解锁成功就收掉弹窗、
   * 立刻跑一轮自动启动；阶段是 declined 说明用户当时关掉了那个弹窗 ——
   * declined 只表示「本次会话别再自动弹」，用户自己手动解锁后仍然允许跑，
   * 所以这里把两种情况一起收尾，剩下的交给 runAutoStartTunnels。
   * 依赖里带上 autoStartVaultHint：万一探测读到的是稍早的「还锁着」，弹窗与解锁
   * 撞在同一瞬间，这条 hint 的变化能让本轮也补上一次判断，不至于卡住不启动。
   */
  useEffect(() => {
    const phase = autoStartPhase.current;
    if (phase !== "awaitingVault" && phase !== "declined") return;
    if (!vault.unlocked) return;
    autoStartPhase.current = "done";
    setAutoStartVaultHint(null);
    if (phase === "awaitingVault") {
      // 这次解锁就是为自动启动输的密码 -> 顺手收掉弹窗，直接看着隧道连上
      setDialog((prev) => (prev.kind === "vault" ? { kind: "none" } : prev));
    }
    void runAutoStartTunnels();
  }, [vault.unlocked, autoStartVaultHint, runAutoStartTunnels]);

  /**
   * 解锁成功后把被前置检查拦下的那批隧道启动续上。
   *
   * 参数：无（取自 vault.unlocked 与 pendingTunnelStarts）
   * 核心流程：解锁成功且确实有一批待办时，先取出并清空标记、顺手清掉「来自列表」
   * 的标记（只续跑一次，后续任何重渲染都不会重复启动），再决定弹窗去哪：
   * 从隧道列表发起的那次把列表放回去（用户能立刻看到隧道变成运行中），
   * 命令面板 / 其它发起方式只收掉解锁框，不硬塞一个列表出来。
   * 最后串行逐条走一遍用户当初的 startTunnel —— 这次库已解锁，前置检查直接放行，
   * 结果照常写状态栏；单条抛错只记控制台，不打断后面几条（与批量启动同一语义）。
   * 跑完 bump 一次 tunnelRevision，让刚挂回去的列表拉到最新运行态。
   * 与自动启动互不相干：这里不碰 autoStartPhase / autoStartVaultHint，
   * 那个 effect 也不碰 pendingTunnelStarts，两条流程可以同时挂着。
   */
  useEffect(() => {
    if (!vault.unlocked) return;
    const ids = pendingTunnelStarts.current;
    if (ids.length === 0) return;
    pendingTunnelStarts.current = [];
    const backToList = pendingTunnelStartsFromList.current;
    pendingTunnelStartsFromList.current = false;
    setDialog((prev) => {
      if (prev.kind !== "vault") return prev;
      return backToList ? { kind: "tunnel" } : { kind: "none" };
    });
    void (async () => {
      for (const id of ids) {
        try {
          await startTunnel(id);
        } catch (err) {
          console.error("隧道启动续跑失败", id, err);
        }
      }
      if (backToList) setTunnelRevision((value) => value + 1);
    })();
  }, [vault.unlocked, startTunnel]);

  /**
   * 面板打开时才拉隧道配置。
   *
   * 参数：无（取自 paletteOpen）
   * 核心流程：不在启动阶段碰隧道库，避免拖慢首屏；失败静默只记控制台，
   * 面板里就只剩「隧道列表」一条常驻命令，不打扰用户。
   */
  useEffect(() => {
    if (!paletteOpen) return;
    let cancelled = false;
    void tunnelConfigList()
      .then((list) => {
        if (!cancelled) setPaletteTunnels(list);
      })
      .catch((err) => {
        console.error("命令面板加载隧道配置失败", err);
      });
    return () => {
      cancelled = true;
    };
  }, [paletteOpen]);

  /**
   * 信任指纹并重连。
   *
   * 参数：无（取自当前弹窗）
   * 核心流程：先写库；隧道调用方拿到 resolve(true) 后自己重试，
   * 终端则标记该会话已信任 -> props 变化触发面板重连。
   */
  const trustAndReconnect = async () => {
    const current = hostKeyPrompt;
    if (!current) return;
    setBusy(true);
    try {
      await trustHostKey(current.info);
      setHostKeyPrompt(null);
      setNotice({ tone: "info", text: `已记录 ${current.info.host} 的主机指纹` });
      if (current.resolve) {
        current.resolve(true);
        return;
      }
      if (current.sessionId) {
        const sessionId = current.sessionId;
        setSshTrusted((prev) => ({ ...prev, [sessionId]: true }));
      }
    } catch (err) {
      setHostKeyPrompt(null);
      // 写库失败等同于没确认：调用方不要继续重试，避免绕过指纹校验
      current.resolve?.(false);
      setNotice({ tone: "error", text: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  /**
   * 取消指纹确认。
   *
   * 参数：无（取自当前弹窗）
   * 核心流程：关掉弹窗并回传 false，等结果的调用方（隧道）据此显示提示。
   */
  const cancelHostKeyPrompt = () => {
    const current = hostKeyPrompt;
    setHostKeyPrompt(null);
    current?.resolve?.(false);
  };

  /**
   * 删除旧指纹并关闭该标签。
   *
   * 参数：无（取自当前弹窗）
   * 核心流程：删记录后收起标签，用户重新点击会话会再次进入确认流程。
   */
  const forgetAndClose = async () => {
    const current = hostKeyPrompt;
    if (!current) return;
    setBusy(true);
    try {
      await forgetHostKey(current.info.host, current.info.port);
      setHostKeyPrompt(null);
      if (current.sessionId) {
        const sessionId = current.sessionId;
        setSshTrusted((prev) => {
          const next = { ...prev };
          delete next[sessionId];
          return next;
        });
        setOpenTabIds((prev) => prev.filter((id) => id !== sessionId));
      }
      setNotice({ tone: "info", text: "已删除旧指纹记录，请重新连接并核对新指纹" });
    } catch (err) {
      setNotice({ tone: "error", text: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  const searching = keyword.trim() !== "";
  const activeSession = useMemo(
    () => sessions.find((session) => session.id === activeTabId) ?? null,
    [sessions, activeTabId],
  );

  /** 详情页可用的系统远程桌面客户端；null 表示正在检测 */
  const [remoteClients, setRemoteClients] = useState<RemoteDesktopClient[] | null>(null);
  /** 远程桌面区块的启动失败提示（检测不到客户端不算失败，只有 spawn 失败才写） */
  const [remoteError, setRemoteError] = useState<string | null>(null);
  /** 安装引导文案只取决于平台 -> 算一次即可 */
  const remoteInstallHint = useMemo(remoteDesktopInstallHint, []);

  // RDP / VNC 的详情页要列出本机客户端 -> 只在这两类会话上检测，
  // 其它协议不为一次 PATH 扫描付出开销。检测失败一律按「一个都没装」处理，
  // 与后端「回空列表而不是报错」的约定保持一致。
  useEffect(() => {
    if (!(activeSession && isRemoteDesktop(activeSession.protocol))) {
      return undefined;
    }
    let cancelled = false;
    setRemoteClients(null);
    setRemoteError(null);
    void remoteDesktopClients()
      .then((list) => {
        if (!cancelled) setRemoteClients(list);
      })
      .catch(() => {
        if (!cancelled) setRemoteClients([]);
      });
    return () => {
      cancelled = true;
    };
  }, [activeSession?.id, activeSession?.protocol]);

  /**
   * 用本机的系统客户端打开远程桌面会话。
   *
   * 参数：client 为选中的客户端，session 为目标会话
   * 核心流程：先二次确认（密码不会自动传递，客户端会单独询问）→
   * 确认后才调后端 spawn；失败只写进详情页的错误文本，不弹原生 alert。
   */
  const openRemoteDesktop = (client: RemoteDesktopClient, session: Session) => {
    const port = remoteDesktopPort(session);
    const target = `${session.username ? `${session.username}@` : ""}${session.host ?? ""}:${port}`;
    setRemoteError(null);
    setConfirmState({
      message: `将用本机的 ${client.name} 打开 ${target}。密码不会自动传递，客户端会单独询问；关闭客户端窗口即断开连接。`,
      run: () => {
        void launchRemoteDesktop(client.kind, session.host ?? "", port, session.username ?? "")
          .then(() => setNotice({ tone: "info", text: `已用 ${client.name} 打开 ${target}` }))
          .catch((err) => setRemoteError(errorMessage(err)));
      },
    });
  };

  /**
   * 记录终端面板上报的终端 id。
   *
   * 参数：sessionId 为会话 id，terminalId 为后端注册表 id（null 表示终端已销毁），
   * expectedId 为要求注销的那个 id（分屏下同会话有两个终端，用来判断该不该删）
   * 核心流程：只增删映射，不触发终端重建；注销时若记录已经被另一块面板改写成
   * 新的 id，就原样返回，避免先退出的那一块把还活着的终端一起注销。
   */
  const handleTerminalId = useCallback(
    (sessionId: string, terminalId: string | null, expectedId?: string) => {
      setTerminalIds((prev) => {
        if (terminalId === null) {
          if (!(sessionId in prev)) return prev;
          if (expectedId !== undefined && prev[sessionId] !== expectedId) return prev;
          const next = { ...prev };
          delete next[sessionId];
          return next;
        }
        if (prev[sessionId] === terminalId) return prev;
        return { ...prev, [sessionId]: terminalId };
      });
    },
    [],
  );

  /**
   * 活动标签对应会话的活跃终端 id；没有终端或终端已注销时为 null。
   *
   * 参数：无（读 activeSession 与 terminalIds）
   * 核心流程：只查已有映射，不新增轮询或后端调用；终端卸载/断开时
   * TerminalPanel 会用 handleTerminalId(sessionId, null) 把条目删掉，这里随之变 null。
   */
  const activeTerminalId =
    activeSession !== null ? (terminalIds[activeSession.id] ?? null) : null;

  /** 指标采样的目标：活动标签是 SSH 会话就采那台远端，其余情况退回本机 */
  const metricsTargetId =
    activeSession?.protocol === "ssh" ? (terminalIds[activeSession.id] ?? null) : null;
  /** SSH 标签已激活但终端还没注册完成 -> 显示「等待终端连接」而不是回落本机 */
  const metricsAwaitingTerminal = activeSession?.protocol === "ssh" && metricsTargetId === null;
  const metricsTargetHost =
    activeSession?.protocol === "ssh" ? (activeSession.host ?? null) : null;

  /**
   * 每 2 秒采一次指标，远端优先。
   *
   * 参数：无
   * 核心流程：目标（活动 SSH 会话）一变就先换来源、清数值，再立刻重采一次，
   * 不用等 2 秒，也不会把上一台机器的数字标成当前主机；
   * 远端采样失败只把来源标成不可用并记下原因，保留上一次远端值但不展示假数字，
   * 绝不悄悄回落到本机口径；
   * GPU 列表只跟着远端采样走，换主机或降级时一并清空，不显示上一台机器的显卡。
   */
  useEffect(() => {
    let disposed = false;
    setMetrics({
      value: null,
      host: metricsTargetHost,
      scope: metricsTargetId ? "remote" : "local",
      reason: null,
      gpus: [],
    });
    const tick = () => {
      if (document.visibilityState !== "visible") return;
      if (metricsAwaitingTerminal) {
        setMetrics({
          value: null,
          host: metricsTargetHost,
          scope: "unavailable",
          reason: "等待终端连接后再采样",
          gpus: [],
        });
        return;
      }
      if (metricsTargetId) {
        void remoteMetrics(metricsTargetId)
          .then((payload) => {
            if (disposed) return;
            setMetrics({
              value: payload.metrics,
              host: payload.host,
              scope: "remote",
              reason: null,
              gpus: payload.gpus,
            });
          })
          .catch((err) => {
            if (disposed) return;
            setMetrics((prev) => ({
              value: prev.value,
              host: prev.host ?? metricsTargetHost,
              scope: "unavailable",
              reason: errorMessage(err),
              gpus: [],
            }));
          });
        return;
      }
      void systemMetrics()
        .then((value) => {
          if (disposed) return;
          setMetrics({ value, host: null, scope: "local", reason: null, gpus: [] });
        })
        .catch(() => {
          // 本机采样失败不打扰用户：保留上一次的值，不弹错误
        });
    };
    tick();
    const timer = window.setInterval(tick, 2000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [metricsTargetId, metricsTargetHost, metricsAwaitingTerminal]);

  /**
   * 磁盘占用单独按 10 秒节流。
   *
   * 参数：无（读 metricsTargetId 与 monitorOpen）
   * 核心流程：面板收起、没有活动远端终端时不发命令 -> 不在远端不停 exec 一次 df；
   * 展开时立刻取一次，之后 10 秒一跳（与侧栏文件面板同一节奏）；
   * 取不到就清空，界面显示「—」，不猜也不沿用旧值。
   */
  useEffect(() => {
    if (!monitorOpen || !metricsTargetId) {
      setDisk(null);
      return undefined;
    }
    let disposed = false;
    const load = () => {
      if (document.visibilityState !== "visible") return;
      void remoteDisk(metricsTargetId, "/")
        .then((value) => {
          if (disposed) return;
          setDisk(value.totalBytes > 0 ? value : null);
        })
        .catch(() => {
          if (disposed) return;
          setDisk(null);
        });
    };
    setDisk(null);
    load();
    const timer = window.setInterval(load, 10000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [monitorOpen, metricsTargetId]);

  /**
   * 指标展示用的派生值。
   *
   * 来源文案、悬停说明与数值统一在这里算：
   * 远端不可用时数值一律为空（界面显示 "--"），避免把旧值或本机值当成远端结果；
   * 地址过长只在文本上省略中间，完整地址仍放在 title 里。
   */
  const metricsHost = metrics.host ?? metricsTargetHost;
  const metricsShortHost = shortenAddress(metricsHost ?? "远端", 16);
  const metricsSourceText =
    metrics.scope === "remote"
      ? `远程 ${metricsShortHost}`
      : metrics.scope === "local"
        ? "本机"
        : "远端不可用";
  const metricsSourceHint =
    metrics.scope === "unavailable"
      ? `远端指标不可用：${metrics.reason ?? "原因未知"}`
      : metrics.scope === "remote"
        ? `数据来自远端主机 ${metricsHost ?? "远端"}`
        : "数据来自本机";
  const metricsValue = metrics.scope === "unavailable" ? null : metrics.value;
  const metricsSupported = metricsValue?.supported === true;

  /** 监控面板五个指标块的取值：取不到一律是 null / "—"，界面据此显示占位符 */
  const cpuPercent = metricsSupported ? Math.round(metricsValue?.cpuPercent ?? 0) : null;
  const memPercent = metricsSupported ? Math.round(metricsValue?.memPercent ?? 0) : null;
  const memText =
    metricsSupported && metricsValue
      ? `${formatMemory(metricsValue.memUsedMb)} / ${formatMemory(metricsValue.memTotalMb)}`
      : "—";
  const diskPercent = disk ? Math.round(disk.percent) : null;
  const diskText = disk ? `${formatDiskSize(disk.usedBytes)} / ${formatDiskSize(disk.totalBytes)}` : "—";
  const diskHint = disk
    ? `磁盘 ${disk.mount} 已用 ${formatDiskSize(disk.usedBytes)}，可用 ${formatDiskSize(disk.freeBytes)}`
    : "远端磁盘占用不可用（会话没有终端或 df 失败）";
  const gpus = metrics.gpus;
  /** 展开面板时先说清数据从哪来：远端不可用 / 平台不支持 / 本机兜底 */
  const monitorNote =
    metrics.scope === "unavailable"
      ? `远端指标不可用：${metrics.reason ?? "原因未知"}，下列数值显示为占位符`
      : metricsValue !== null && !metricsValue.supported
        ? `当前平台不支持${metrics.scope === "remote" ? "远端" : "本机"}指标采样`
        : metrics.scope === "local" && activeSession?.protocol === "ssh"
          ? "远端会话终端还没就绪，当前显示的是本机数据"
          : null;

  const selectedGroup = groups.find((group) => group.id === selectedGroupId) ?? null;

  const tabs: TabItem[] = openTabIds
    .map((id) => sessions.find((session) => session.id === id))
    .filter((session): session is Session => session !== undefined)
    .map((session) => ({
      id: session.id,
      title: session.name,
      subtitle: session.host ? `${session.username ?? ""}@${session.host}` : "",
      protocol: session.protocol,
    }));

  const terminalTabs = tabs.filter((tab) => isTerminalProtocol(tab.protocol));
  const sftpTabs = tabs.filter((tab) => tab.protocol === "sftp");
  // 终端协议与 SFTP 都有自己的面板 -> 详情页只服务还没实现的协议
  const activeHasPanel =
    activeSession !== null &&
    (isTerminalProtocol(activeSession.protocol) || activeSession.protocol === "sftp");
  // 分屏只服务终端，SFTP 面板不做左右分屏
  const activeHasTerminal = activeSession !== null && isTerminalProtocol(activeSession.protocol);
  const activeSplit = activeTabId !== null && splitTabIds.includes(activeTabId);

  /** 「文件」分页绑定的会话：只有 ssh / sftp 能连，本地终端与无标签时为空 */
  const sftpSession =
    activeSession && isSshLike(activeSession.protocol) ? activeSession : null;
  /** 复用既有认证解析：本次运行已输入的认证优先，其次私钥路径或凭据引用 */
  const sftpAuthState = sftpSession ? resolveSshAuth(sftpSession, sshAuths) : null;
  /** 已登记到内存的认证：引用稳定，直接交给面板 -> 重渲染不会反复重连 */
  const sftpAuth = sftpSession ? (sshAuths[sftpSession.id] ?? null) : null;
  /** 凭据库还锁着时不能直接连 -> 先让用户解锁，和打开会话走同一条路 */
  const sftpLockedVault =
    sftpAuthState?.kind === "ready" &&
    sftpAuthState.auth.kind === "credential" &&
    !vault.unlocked;
  /**
   * 文件分页要不要真的挂载 SftpPanel。
   *
   * 条件：打开过「文件」分页 + 当前会话能连 + 认证已登记 +
   * 允许自动连接或用户手动点了连接。挂载后才建连接，卸载即 sftp_close。
   */
  const sftpMounted =
    sftpVisited &&
    sftpSession !== null &&
    sftpAuth !== null &&
    !sftpLockedVault &&
    (settings.sidebarAutoConnect || sftpManualConnect);

  /**
   * 把会话里存着的认证方式登记进内存表。
   *
   * 参数：无
   * 核心流程：会话 extra 里已有私钥路径或凭据引用时 resolveSshAuth 能直接给出认证，
   * 登记进 sshAuths 后引用稳定（面板的 connect effect 依赖这个引用）；
   * 凭据库锁着时先不登记，等解锁后这次 effect 会再跑一次。
   */
  useEffect(() => {
    if (!sftpSession || sftpAuth) return;
    const resolved = resolveSshAuth(sftpSession, sshAuths);
    if (resolved.kind === "ready") {
      if (resolved.auth.kind === "credential" && !vault.unlocked) return;
      setSshAuths((prev) =>
        prev[sftpSession.id] ? prev : { ...prev, [sftpSession.id]: resolved.auth },
      );
      return;
    }
    if (resolved.kind === "needKeyPassphrase") {
      // 口令存过凭据库引用时自动取回明文登记，省掉「输入私钥口令」这一屏；
      // 取不到（库锁着 / 凭据已删）就保持占位，让用户手输
      void revealStoredPassphrase(resolved.credentialId).then((passphrase) => {
        if (passphrase === null) return;
        setSshAuths((prev) =>
          prev[sftpSession.id]
            ? prev
            : {
                ...prev,
                [sftpSession.id]: {
                  kind: "privateKey",
                  path: resolved.path,
                  passphrase,
                },
              },
        );
      });
    }
  }, [sftpSession, sftpAuth, sshAuths, vault.unlocked]);

  // 设置读完后按 sidebar.defaultTab 展开一次分页 -> 之后用户自己的选择优先
  useEffect(() => {
    if (!settingsReady || defaultTabApplied.current) return;
    defaultTabApplied.current = true;
    setSidebarTab(settings.sidebarDefaultTab);
    if (settings.sidebarDefaultTab === "sftp") setSftpVisited(true);
  }, [settingsReady, settings.sidebarDefaultTab]);

  // 换活动会话时清掉手动连接标记 -> 新会话重新走「自动连接或再点一次」
  useEffect(() => {
    setSftpManualConnect(false);
  }, [sftpSession?.id]);

  /** 当前分页的标题，直接取图标栏文案 -> 两处不会走偏 */
  const sidebarTabLabel = SIDEBAR_RAIL.find((item) => item.key === sidebarTab)?.label ?? "";

  /**
   * 打开工具分页里的弹窗类操作。
   *
   * 参数：kind 为要打开的弹窗类型
   * 核心流程：开凭据库前先清掉上次的错误；动作执行后保持当前分页不变，
   * 不自动跳页，避免用户还得手动切回来。
   */
  const openToolDialog = (kind: ToolDialogKind) => {
    if (kind === "vault") {
      setVaultResumePrompt(false);
      setVaultError(null);
      setDialog({ kind: "vault" });
      return;
    }
    if (kind === "knownHosts") {
      setDialog({ kind: "knownHosts" });
      return;
    }
    if (kind === "tunnel") {
      setDialog({ kind: "tunnel" });
      return;
    }
    setDialog({ kind: "settings" });
  };

  // 菜单按 MobaXterm 的词汇分组；没实现的菜单保留一条 disabled 占位，布局不塌
  const menuGroups: MenuGroup[] = [
    {
      label: "终端",
      entries: [
        {
          kind: "item",
          label: "新建会话…",
          onSelect: () => setDialog({ kind: "session", mode: "create", session: null }),
        },
        { kind: "separator" },
        {
          kind: "item",
          label: activeSplit ? "取消左右分屏" : "左右分屏",
          disabled: !activeHasTerminal,
          onSelect: () => {
            if (activeTabId) toggleSplit(activeTabId);
          },
        },
      ],
    },
    {
      label: "会话",
      entries: [
        {
          kind: "item",
          label: "新建会话…",
          onSelect: () => setDialog({ kind: "session", mode: "create", session: null }),
        },
        {
          kind: "item",
          label: "新建分组…",
          onSelect: () => setDialog({ kind: "group", mode: "create", group: null }),
        },
        { kind: "separator" },
        { kind: "item", label: "导入 MobaXterm 会话…", onSelect: () => void startImport() },
        { kind: "item", label: "导出会话…", onSelect: () => void startExport() },
      ],
    },
    {
      label: "视图",
      entries: [
        {
          kind: "item",
          label: "命令面板…",
          onSelect: () => setPaletteOpen(true),
        },
        { kind: "separator" },
        {
          kind: "item",
          label: "全部展开",
          onSelect: () => setAllCollapsed(false),
        },
        { kind: "item", label: "全部折叠", onSelect: () => setAllCollapsed(true) },
        { kind: "separator" },
        {
          kind: "item",
          label: "文件分页",
          onSelect: () => {
            setSidebarVisible(true);
            selectSidebarTab("sftp");
          },
        },
        { kind: "separator" },
        {
          kind: "item",
          label: sidebarVisible ? "隐藏侧栏" : "显示侧栏",
          onSelect: () => setSidebarVisible((prev) => !prev),
        },
        { kind: "item", label: "远程监控", checked: monitorOpen, onSelect: () => setMonitorOpen((prev) => !prev) },
      ],
    },
    {
      label: "X 服务器",
      entries: [
        { kind: "item", label: "X 服务器转发（尚未实现）", disabled: true, onSelect: () => {} },
      ],
    },
    {
      label: "工具",
      entries: [
        {
          kind: "item",
          label: "凭据库…",
          onSelect: () => {
            setVaultResumePrompt(false);
            setVaultError(null);
            setDialog({ kind: "vault" });
          },
        },
        { kind: "item", label: "隧道…", onSelect: () => setDialog({ kind: "tunnel" }) },
        { kind: "separator" },
        { kind: "item", label: "已知主机…", onSelect: () => setDialog({ kind: "knownHosts" }) },
      ],
    },
    {
      label: "设置",
      entries: [
        {
          kind: "item",
          label: "首选项…",
          onSelect: () => setDialog({ kind: "settings" }),
        },
      ],
    },
    {
      label: "宏",
      entries: [{ kind: "item", label: "宏录制（尚未实现）", disabled: true, onSelect: () => {} }],
    },
    {
      label: "帮助",
      entries: [
        { kind: "item", label: "关于 RustMobaX", onSelect: () => setDialog({ kind: "about" }) },
      ],
    },
  ];

  const toolbarActions: ToolbarAction[] = [
    {
      kind: "button",
      icon: <IconSessions size={22} />,
      label: "新建会话",
      onClick: () => setDialog({ kind: "session", mode: "create", session: null }),
    },
    {
      kind: "button",
      icon: <IconFolder size={22} />,
      label: "新建分组",
      onClick: () => setDialog({ kind: "group", mode: "create", group: null }),
    },
    { kind: "divider" },
    {
      kind: "button",
      icon: <IconImport size={22} />,
      label: "导入",
      title: "导入 MobaXterm 会话",
      onClick: () => void startImport(),
    },
    {
      kind: "button",
      icon: <IconExport size={22} />,
      label: "导出",
      title: "导出会话列表",
      onClick: () => void startExport(),
    },
    { kind: "divider" },
    {
      kind: "button",
      icon: <IconSplit size={22} />,
      label: activeSplit ? "取消分屏" : "分屏",
      title: "左右分屏，再次点击还原",
      disabled: !activeHasTerminal,
      active: activeSplit,
      onClick: () => {
        if (activeTabId) toggleSplit(activeTabId);
      },
    },
    {
      kind: "button",
      icon: <IconKey size={22} />,
      label: "凭据库",
      title: "打开凭据库",
      onClick: () => {
        setVaultResumePrompt(false);
        setVaultError(null);
        setDialog({ kind: "vault" });
      },
    },
    {
      kind: "button",
      icon: <IconTunnel size={22} />,
      label: "隧道",
      title: "端口转发隧道",
      onClick: () => setDialog({ kind: "tunnel" }),
    },
    { kind: "divider" },
    {
      kind: "button",
      icon: <IconExpandAll size={22} />,
      label: "全部展开",
      onClick: () => setAllCollapsed(false),
    },
    {
      kind: "button",
      icon: <IconCollapseAll size={22} />,
      label: "全部折叠",
      onClick: () => setAllCollapsed(true),
    },
    { kind: "divider" },
    {
      kind: "button",
      icon: <IconInfo size={22} />,
      label: "关于",
      onClick: () => setDialog({ kind: "about" }),
    },
  ];

  /**
   * 渲染搜索结果。
   *
   * 参数：无
   * 核心流程：搜索时用扁平列表，避免命中项被折叠的分组藏起来。
   */
  const renderSearchResults = () => {
    if (sessions.length === 0) {
      return <div className="tree-empty">没有匹配「{keyword}」的会话</div>;
    }
    return (
      <div className="session-tree">
        {sessions.map((session) => (
          <div
            key={session.id}
            className={`tree-row session-item ${
              activeTabId === session.id ? "is-active" : ""
            }`}
            onClick={() => openSession(session)}
          >
            <span className={`session-icon protocol-${session.protocol}`}>
              {session.protocol === "sftp" ? (
                <IconFolder size={15} />
              ) : session.protocol === "ssh" ? (
                <IconServer size={15} />
              ) : (
                <IconTerminal size={15} />
              )}
            </span>
            <span className="session-name">{session.name}</span>
            <span className="session-target">
              {session.host ?? "local"}
              {session.groupId ? ` · ${groupPath(groups, session.groupId).join("/")}` : ""}
            </span>
          </div>
        ))}
      </div>
    );
  };

  /**
   * 渲染「文件」分页正文。
   *
   * 参数：无
   * 核心流程：按「有可连会话 -> 有认证 -> 允许连接」逐级给出提示与入口，
   * 全都满足才挂载 SftpPanel（挂载即建连，卸载时组件自己发 sftp_close）；
   * 面板用会话 id 作 key -> 换活动会话时旧面板卸载回收旧连接，新会话重新懒加载。
   */
  const renderSftpPage = () => {
    if (!sftpSession) {
      return (
        <div className="sidebar-empty">
          <p className="sidebar-empty-title">还没有可用的会话</p>
          <p className="sidebar-empty-text">先在「会话」分页里打开一个 SSH 会话。</p>
        </div>
      );
    }
    if (!sftpAuthState) return null;
    if (sftpAuthState.kind === "needPassword") {
      return (
        <div className="sidebar-empty">
          <p className="sidebar-empty-title">{sftpSession.name}</p>
          <p className="sidebar-empty-text">这条连接还没有可用的认证方式，密码只保存在内存中。</p>
          <button
            type="button"
            className="sidebar-empty-action"
            onClick={() => setPendingSsh(sftpSession)}
          >
            输入密码
          </button>
        </div>
      );
    }
    if (sftpAuthState.kind === "needKeyPassphrase") {
      const keyPath = sftpAuthState.path;
      return (
        <div className="sidebar-empty">
          <p className="sidebar-empty-title">{sftpSession.name}</p>
          <p className="sidebar-empty-text">私钥带口令，本次运行还没收到口令。</p>
          <button
            type="button"
            className="sidebar-empty-action"
            onClick={() => setPendingKeyPass({ session: sftpSession, path: keyPath })}
          >
            输入私钥口令
          </button>
        </div>
      );
    }
    if (sftpLockedVault) {
      return (
        <div className="sidebar-empty">
          <p className="sidebar-empty-title">{sftpSession.name}</p>
          <p className="sidebar-empty-text">这条会话用凭据库里的凭据，先解锁凭据库。</p>
          <button
            type="button"
            className="sidebar-empty-action"
            onClick={() => {
              setVaultResumePrompt(false);
              setPendingVaultOpen(sftpSession);
              setVaultError(null);
              setDialog({ kind: "vault" });
            }}
          >
            解锁凭据库
          </button>
        </div>
      );
    }
    if (!sftpAuth) {
      // 认证正在从会话配置里登记过来，这一帧通常只有几十毫秒
      return (
        <div className="sidebar-empty">
          <p className="sidebar-empty-text">正在准备连接…</p>
        </div>
      );
    }
    if (!sftpMounted) {
      return (
        <div className="sidebar-empty">
          <p className="sidebar-empty-title">{sftpSession.name}</p>
          <p className="sidebar-empty-text">已关闭自动连接，手动连一次即可浏览远端目录。</p>
          <button
            type="button"
            className="sidebar-empty-action"
            onClick={() => setSftpManualConnect(true)}
          >
            连接
          </button>
        </div>
      );
    }
    return (
      <SftpPanel
        key={sftpSession.id}
        session={sftpSession}
        sshAuth={sftpAuth}
        acceptUnknownHostKey={sshTrusted[sftpSession.id] === true}
        onHostKeyPrompt={handleHostKeyPrompt}
        onHostKeyMismatch={handleHostKeyMismatch}
        cwd={cwdByTarget[targetKey(sftpSession)] ?? null}
        followCwd={followCwd}
        onFollowCwdChange={(next) => applySetting("sftpFollowCwd", next)}
        terminalId={terminalIds[sftpSession.id] ?? null}
      />
    );
  };

  /**
   * 全局快捷键分发。
   *
   * 参数：无
   * 核心流程：捕获阶段监听 keydown，命中已配置组合就执行动作并阻止默认行为；
   * 真输入框/下拉/可编辑区聚焦时不拦截，终端内的搜索与粘贴由终端自己处理，
   * 这里只看 defaultPrevented 与焦点位置，避免和终端抢键。
   *
   * 注意 xterm 用隐藏的 textarea（.xterm-helper-textarea）承接键盘输入，所以终端聚焦时
   * event.target 就是 TEXTAREA；它不是「用户正在打字的输入框」，照旧返回会让终端里的
   * 全局快捷键整体失效，因此用 closest(".xterm") 把它和真输入框区分开。
   */
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing) return;
      const target = event.target as HTMLElement | null;
      if (target) {
        const tag = target.tagName;
        /** 焦点在终端容器内（.xterm 由 term.open 生成）-> 那是 xterm 的隐藏 textarea */
        const inTerminal = target.closest?.(".xterm") != null;
        const typingInField =
          tag === "INPUT" ||
          tag === "SELECT" ||
          (tag === "TEXTAREA" && !inTerminal);
        if (typingInField || target.isContentEditable) {
          return;
        }
      }
      const map = settings.shortcuts;
      const hit = (action: ShortcutActionId) => matches(event, map[action]);

      // 终端内的搜索与粘贴交给 TerminalPanel，避免一次按键触发两次
      if (hit("search")) return;

      // 命中并执行动作后必须 stopPropagation：xterm 的 _keyDown 只问自己的
      // customKeyEventHandler，不检查 defaultPrevented，事件若继续传播到它的隐藏
      // textarea 仍会被编码后发给 shell（Ctrl+N -> 0x0e），于是「弹窗开了终端里还多一个
      // 控制字符」。在 window 捕获阶段掐断传播，事件就不会走到目标节点。
      if (hit("newSession")) {
        event.preventDefault();
        event.stopPropagation();
        setDialog({ kind: "session", mode: "create", session: null });
      } else if (hit("newGroup")) {
        event.preventDefault();
        event.stopPropagation();
        setDialog({ kind: "group", mode: "create", group: null });
      } else if (hit("import")) {
        event.preventDefault();
        event.stopPropagation();
        void startImport();
      } else if (hit("closeTab")) {
        if (activeTabId) {
          event.preventDefault();
          event.stopPropagation();
          closeTab(activeTabId);
        }
      } else if (hit("split")) {
        if (activeHasTerminal && activeTabId) {
          event.preventDefault();
          event.stopPropagation();
          toggleSplit(activeTabId);
        }
      } else if (hit("toggleSidebar")) {
        event.preventDefault();
        event.stopPropagation();
        setSidebarVisible((prev) => !prev);
      } else if (hit("settings")) {
        event.preventDefault();
        event.stopPropagation();
        setDialog({ kind: "settings" });
      } else if (hit("commandPalette")) {
        event.preventDefault();
        event.stopPropagation();
        setPaletteOpen(true);
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [settings.shortcuts, activeTabId, activeHasTerminal, startImport, closeTab, toggleSplit]);

  /**
   * 命令面板的命令清单。
   *
   * 参数：无（由当前标签、会话列表、隧道缓存与键位拼出）
   * 核心流程：固定动作排前面，动态项（每个标签 / 未打开的会话 / 每条隧道）随后；
   * id 用不同前缀（action: / tab: / session: / tunnel: / nav:）保证全局唯一，
   * 同一会话既在标签里又在会话列表里也不会撞 id；键位提示统一走 formatCombo。
   */
  const paletteCommands: Command[] = useMemo(() => {
    const shortcuts = settings.shortcuts;
    const list: Command[] = [];

    // 会话与分组：常用的增删改入口
    list.push({
      id: "action:newSession",
      title: "新会话",
      titleEn: "New session",
      group: "会话",
      hint: formatCombo(shortcuts.newSession),
      keywords: ["new", "session", "create", "xinjian"],
      run: () => setDialog({ kind: "session", mode: "create", session: null }),
    });
    list.push({
      id: "action:newGroup",
      title: "新建分组",
      titleEn: "New group",
      group: "会话",
      hint: formatCombo(shortcuts.newGroup),
      keywords: ["group", "folder", "fenzu"],
      run: () => setDialog({ kind: "group", mode: "create", group: null }),
    });
    list.push({
      id: "action:import",
      title: "导入会话",
      titleEn: "Import sessions",
      group: "会话",
      hint: formatCombo(shortcuts.import),
      keywords: ["import", "mobaxterm", "daoru"],
      run: () => void startImport(),
    });

    // 跳转：四条固定入口，先去侧栏再换分页
    list.push({
      id: "nav:sessions",
      title: "去到会话",
      titleEn: "Go to Sessions",
      group: "跳转",
      keywords: ["sessions", "sidebar"],
      run: () => {
        setSidebarVisible(true);
        selectSidebarTab("sessions");
      },
    });
    list.push({
      id: "nav:tools",
      title: "去到工具",
      titleEn: "Go to Tools",
      group: "跳转",
      keywords: ["tools", "sidebar"],
      run: () => {
        setSidebarVisible(true);
        selectSidebarTab("tools");
      },
    });
    list.push({
      id: "nav:macros",
      title: "去到宏",
      titleEn: "Go to Macros",
      group: "跳转",
      keywords: ["macros", "sidebar"],
      run: () => {
        setSidebarVisible(true);
        selectSidebarTab("macros");
      },
    });
    list.push({
      id: "nav:sftp",
      title: "去到文件",
      titleEn: "Go to Files",
      group: "跳转",
      keywords: ["files", "sftp", "sidebar"],
      // selectSidebarTab 内部已对 sftp 置 sftpVisited，这里不重复
      run: () => {
        setSidebarVisible(true);
        selectSidebarTab("sftp");
      },
    });

    // 动态：已打开的标签
    for (const tab of tabs) {
      list.push({
        id: `tab:${tab.id}`,
        title: tab.title,
        titleEn: "Switch to tab",
        group: "跳转",
        hint: tab.subtitle,
        keywords: ["tab", "switch"],
        run: () => setActiveTabId(tab.id),
      });
    }

    // 动态：还没打开的会话
    for (const session of sessions) {
      if (openTabIds.includes(session.id)) continue;
      list.push({
        id: `session:${session.id}`,
        title: session.name,
        titleEn: "Open session",
        group: "会话",
        hint: session.host ? `${session.username ?? ""}@${session.host}` : session.protocol,
        keywords: ["open", "connect", session.protocol],
        run: () => void openSession(session),
      });
    }

    // 标签操作
    list.push({
      id: "action:closeTab",
      title: "关闭当前标签",
      titleEn: "Close current tab",
      group: "标签",
      hint: formatCombo(shortcuts.closeTab),
      keywords: ["close", "tab"],
      run: () => {
        if (activeTabId) closeTab(activeTabId);
      },
    });
    list.push({
      id: "action:split",
      title: "分屏当前标签",
      titleEn: "Split current tab",
      group: "标签",
      hint: formatCombo(shortcuts.split),
      keywords: ["split", "pane"],
      run: () => {
        if (activeTabId && activeHasTerminal) toggleSplit(activeTabId);
      },
    });

    // 隧道
    list.push({
      id: "action:tunnelList",
      title: "隧道列表",
      titleEn: "Tunnel list",
      group: "隧道",
      keywords: ["tunnel", "forward", "suidao"],
      run: () => setDialog({ kind: "tunnel" }),
    });
    for (const config of paletteTunnels) {
      list.push({
        id: `tunnel:${config.id}`,
        title: `启动隧道 ${config.name || config.id}`,
        titleEn: "Start tunnel",
        group: "隧道",
        hint: `${config.listenHost || "127.0.0.1"}:${config.listenPort || "auto"}`,
        keywords: ["tunnel", "start", config.kind],
        run: () => void startTunnel(config.id),
      });
    }

    // 视图与系统
    list.push({
      id: "action:toggleSidebar",
      title: sidebarVisible ? "隐藏侧栏" : "显示侧栏",
      titleEn: "Toggle sidebar",
      group: "视图",
      hint: formatCombo(shortcuts.toggleSidebar),
      keywords: ["sidebar", "toggle"],
      run: () => setSidebarVisible((prev) => !prev),
    });
    list.push({
      id: "action:settings",
      title: "打开设置",
      titleEn: "Open settings",
      group: "系统",
      hint: formatCombo(shortcuts.settings),
      keywords: ["settings", "preferences", "shezhi"],
      run: () => setDialog({ kind: "settings" }),
    });
    list.push({
      id: "action:vault",
      title: "打开凭据库",
      titleEn: "Open vault",
      group: "系统",
      keywords: ["vault", "credential", "pingju"],
      run: () => {
        setVaultResumePrompt(false);
        setVaultError(null);
        setDialog({ kind: "vault" });
      },
    });
    list.push({
      id: "action:knownHosts",
      title: "已知主机",
      titleEn: "Known hosts",
      group: "系统",
      keywords: ["known", "host", "hostkey", "zhiji"],
      run: () => setDialog({ kind: "knownHosts" }),
    });
    list.push({
      id: "action:about",
      title: "关于 RustMobaX",
      titleEn: "About RustMobaX",
      group: "系统",
      keywords: ["about", "version", "guanyu"],
      run: () => setDialog({ kind: "about" }),
    });

    return list;
  }, [
    settings.shortcuts,
    tabs,
    sessions,
    openTabIds,
    paletteTunnels,
    activeTabId,
    activeHasTerminal,
    sidebarVisible,
    startTunnel,
    startImport,
    openSession,
    closeTab,
    toggleSplit,
    selectSidebarTab,
  ]);

  const statusText =
    notice?.text ??
    (activeSession
      ? `${activeSession.protocol.toUpperCase()} · ${activeSession.host ?? "本地终端"}`
      : "就绪");

  return (
    <div className="app-shell">
      <MenuBar groups={menuGroups} />
      <Toolbar actions={toolbarActions} />

      <div
        className="workspace"
        style={{ "--sidebar-width": `${sidebarWidth}px` } as CSSProperties}
      >
        {/* 收起侧栏只做 CSS 隐藏：组件仍挂载，「文件」分页的 SFTP 连接不会因为收起而断开 */}
        <aside className={`sidebar ${sidebarVisible ? "" : "is-hidden"}`}>
          <SidebarRail active={sidebarTab} onSelect={selectSidebarTab} />
          <div className="sidebar-main">
            <div className="sidebar-page-head">
              <span className="sidebar-page-title">{sidebarTabLabel}</span>
              {sidebarTab === "sessions" && (
                <span className="sidebar-count">{sessions.length}</span>
              )}
            </div>

            <section
              className="sidebar-page"
              role="tabpanel"
              id="sidebar-page-sessions"
              aria-labelledby="sidebar-tab-sessions"
              hidden={sidebarTab !== "sessions"}
            >
              <div className="quick-connect">
                <span className="quick-connect-icon">
                  <IconTerminal size={14} tone="mono" />
                </span>
                <input
                  value={quickConnect}
                  placeholder="Quick connect: user@host:port"
                  onChange={(event) => setQuickConnect(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") handleQuickConnect(quickConnect);
                  }}
                />
              </div>

              <div className="tree-search">
                <span className="tree-search-icon">
                  <IconSearch size={13} />
                </span>
                <input
                  value={keyword}
                  placeholder="搜索名称 / 主机 / 用户名 / 分组"
                  onChange={(event) => setKeyword(event.target.value)}
                />
              </div>

              <div className="tree-scroll">
                {searching ? (
                  renderSearchResults()
                ) : (
                  <SessionTree
                    groups={groups}
                    sessions={sessions}
                    collapsed={collapsed}
                    selectedGroupId={selectedGroupId}
                    selectedSessionId={activeTabId}
                    onToggleGroup={toggleGroup}
                    onSelectGroup={setSelectedGroupId}
                    onOpenSession={openSession}
                    onRenameGroup={(group) => setDialog({ kind: "group", mode: "rename", group })}
                    onDeleteGroup={askDeleteGroup}
                    onEditSession={(session) =>
                      setDialog({ kind: "session", mode: "edit", session })
                    }
                    onCopySession={(session) => void copySession(session)}
                    onDeleteSession={askDeleteSession}
                    onMoveSession={(sessionId, groupId) => void moveSessionTo(sessionId, groupId)}
                    onMoveGroup={(groupId, parentId) => void moveGroupTo(groupId, parentId)}
                  />
                )}
              </div>
            </section>

            <section
              className="sidebar-page"
              role="tabpanel"
              id="sidebar-page-tools"
              aria-labelledby="sidebar-tab-tools"
              hidden={sidebarTab !== "tools"}
            >
              <ToolsPanel onOpenDialog={openToolDialog} onOpenSftp={() => selectSidebarTab("sftp")} />
            </section>

            <section
              className="sidebar-page"
              role="tabpanel"
              id="sidebar-page-macros"
              aria-labelledby="sidebar-tab-macros"
              hidden={sidebarTab !== "macros"}
            >
              <MacrosPanel />
            </section>

            <section
              className="sidebar-page sidebar-page-flush"
              role="tabpanel"
              id="sidebar-page-sftp"
              aria-labelledby="sidebar-tab-sftp"
              hidden={sidebarTab !== "sftp"}
            >
              {renderSftpPage()}
            </section>
          </div>
        </aside>
        {sidebarVisible && (
          <SidebarResizer
            width={sidebarWidth}
            min={SFTP_WIDTH_MIN}
            max={SFTP_WIDTH_MAX}
            onDrag={previewSidebarWidth}
            onCommit={commitSidebarWidth}
          />
        )}

        <main className="content">
          <TabBar
            tabs={tabs}
            activeId={activeTabId}
            onSelect={setActiveTabId}
            onClose={closeTab}
            sidebarVisible={sidebarVisible}
            onToggleSidebar={() => setSidebarVisible((prev) => !prev)}
          />

          <div className="tab-body">
            {/* 终端常驻 -> 切换标签不会重建会话 */}
            {terminalTabs.map((tab) => {
              const tabSession = sessions.find((item) => item.id === tab.id);
              if (!tabSession) return null;
              const split = splitTabIds.includes(tab.id);
              // 两块终端各自独立建连接；外面再套停靠容器时，主区变窄会触发
              // xterm 的 ResizeObserver -> 两块终端都重新 fit
              const terminalSplit = (
                <div className="terminal-split">
                  <div className="terminal-pane">
                    <TerminalPanel
                      session={tabSession}
                      theme={activeTheme}
                      fontSize={settings.terminalFontSize}
                      fontFamily={terminalFontFamily}
                      onFontSizeChange={(next) => applySetting("terminalFontSize", next)}
                      sshAuth={sshAuths[tabSession.id]}
                      acceptUnknownHostKey={sshTrusted[tabSession.id] === true}
                      onHostKeyPrompt={handleHostKeyPrompt}
                      onHostKeyMismatch={handleHostKeyMismatch}
                      onCwdChange={handleCwdChange}
                      onTerminalId={handleTerminalId}
                      banner={settings.terminalBanner}
                      followCwd={settings.sftpFollowCwd}
                    />
                  </div>
                  {split && (
                    // 第二块面板独立建连接，与左侧互不影响
                    <div className="terminal-pane">
                      <TerminalPanel
                        key={`${tab.id}-split`}
                        session={tabSession}
                        theme={activeTheme}
                        fontSize={settings.terminalFontSize}
                        fontFamily={terminalFontFamily}
                        onFontSizeChange={(next) => applySetting("terminalFontSize", next)}
                        sshAuth={sshAuths[tabSession.id]}
                        acceptUnknownHostKey={sshTrusted[tabSession.id] === true}
                        onHostKeyPrompt={handleHostKeyPrompt}
                        onHostKeyMismatch={handleHostKeyMismatch}
                        onCwdChange={handleCwdChange}
                        onTerminalId={handleTerminalId}
                        banner={settings.terminalBanner}
                        followCwd={settings.sftpFollowCwd}
                        onClosePane={() => toggleSplit(tab.id)}
                      />
                    </div>
                  )}
                </div>
              );
              // 文件面板已经搬到左侧栏的「文件」分页 -> 标签里只放终端本体
              return (
                <div
                  key={tab.id}
                  className={`tab-pane ${activeTabId === tab.id ? "" : "is-hidden"}`}
                >
                  {terminalSplit}
                </div>
              );
            })}

            {sftpTabs.map((tab) => {
              const tabSession = sessions.find((item) => item.id === tab.id);
              if (!tabSession) return null;
              return (
                <div
                  key={tab.id}
                  className={`tab-pane ${activeTabId === tab.id ? "" : "is-hidden"}`}
                >
                  {/* SFTP 会话没有终端 -> 主区给出入口，文件列表在左侧栏「文件」分页 */}
                  <section className="welcome">
                    <div className="welcome-logo">
                      <IconFolder size={44} />
                    </div>
                    <h2>{tabSession.name}</h2>
                    <p>
                      {tabSession.username ?? ""}@{tabSession.host ?? ""}
                      {tabSession.port ? `:${tabSession.port}` : ""}
                    </p>
                    <p>文件列表已移到左侧栏的「文件」分页，双击目录进入、双击文件用内置编辑器打开。</p>
                    <div className="welcome-actions">
                      <button
                        type="button"
                        className="primary"
                        onClick={() => {
                          setSidebarVisible(true);
                          selectSidebarTab("sftp");
                        }}
                      >
                        打开文件分页
                      </button>
                    </div>
                  </section>
                </div>
              );
            })}

            {activeSession && !activeHasPanel ? (
              <section className="detail-page">
                <section className="detail">
                <header className="detail-header">
                  <div>
                    <h2>{activeSession.name}</h2>
                    <p className="detail-sub">
                      {activeSession.protocol.toUpperCase()}
                      {activeSession.host
                        ? ` · ${activeSession.username ?? ""}@${activeSession.host}`
                        : ""}
                      {activeSession.port ? `:${activeSession.port}` : ""}
                    </p>
                  </div>
                  <div className="detail-actions">
                    <button
                      type="button"
                      className="primary"
                      disabled
                      title="终端连接将在 E2 实现"
                    >
                      连接
                    </button>
                    <button
                      type="button"
                      onClick={() =>
                        setDialog({ kind: "session", mode: "edit", session: activeSession })
                      }
                    >
                      编辑
                    </button>
                    <button type="button" onClick={() => void copySession(activeSession)}>
                      复制
                    </button>
                    <button
                      type="button"
                      className="danger"
                      onClick={() => askDeleteSession(activeSession)}
                    >
                      删除
                    </button>
                  </div>
                </header>

                <dl className="detail-grid">
                  <dt>协议</dt>
                  <dd>{activeSession.protocol}</dd>
                  <dt>认证方式</dt>
                  <dd>{activeSession.authKind}</dd>
                  <dt>主机</dt>
                  <dd>{activeSession.host ?? "-"}</dd>
                  <dt>端口</dt>
                  <dd>{activeSession.port ?? "-"}</dd>
                  <dt>用户名</dt>
                  <dd>{activeSession.username ?? "-"}</dd>
                  <dt>分组</dt>
                  <dd>
                    {activeSession.groupId
                      ? groupPath(groups, activeSession.groupId).join(" / ")
                      : "未分组"}
                  </dd>
                  <dt>更新时间</dt>
                  <dd>{new Date(activeSession.updatedAt).toLocaleString("zh-CN")}</dd>
                </dl>

                {isRemoteDesktop(activeSession.protocol) && (
                  <section className="remote-desktop">
                    <h3>用系统客户端打开</h3>
                    <p className="remote-desktop-note">
                      RDP / VNC 本期不自研渲染，连接交给本机已安装的客户端；密码由客户端单独询问。
                    </p>
                    {remoteClients === null ? (
                      <p className="remote-desktop-note">正在检测本机客户端…</p>
                    ) : remoteClients.length === 0 ? (
                      <div>
                        <p className="remote-desktop-note is-strong">未检测到远程桌面客户端</p>
                        <p className="remote-desktop-note">{remoteInstallHint.intro}</p>
                        <ul className="remote-desktop-hints">
                          {remoteInstallHint.commands.map((command) => (
                            <li key={command}>
                              <code>{command}</code>
                            </li>
                          ))}
                        </ul>
                      </div>
                    ) : (
                      <div className="remote-desktop-actions">
                        {remoteClients.map((client) => (
                          <button
                            key={client.kind}
                            type="button"
                            className="primary"
                            title={client.path}
                            onClick={() => openRemoteDesktop(client, activeSession)}
                          >
                            用 {client.name} 打开
                          </button>
                        ))}
                      </div>
                    )}
                    {remoteError && <p className="remote-desktop-error">{remoteError}</p>}
                  </section>
                )}

                <p className="terminal-placeholder">
                  终端会话、SFTP 面板与端口转发在后续里程碑接入。
                </p>
                </section>
              </section>
            ) : !activeSession && selectedGroup ? (
              <section className="detail-page">
                <section className="detail">
                <header className="detail-header">
                  <div>
                    <h2>{selectedGroup.name}</h2>
                    <p className="detail-sub">
                      分组 · {groupPath(groups, selectedGroup.id).join(" / ")}
                    </p>
                  </div>
                  <div className="detail-actions">
                    <button
                      type="button"
                      onClick={() => setDialog({ kind: "group", mode: "create", group: null })}
                    >
                      新建子分组
                    </button>
                    <button
                      type="button"
                      onClick={() =>
                        setDialog({ kind: "group", mode: "rename", group: selectedGroup })
                      }
                    >
                      重命名
                    </button>
                    <button
                      type="button"
                      className="danger"
                      onClick={() => askDeleteGroup(selectedGroup)}
                    >
                      删除
                    </button>
                  </div>
                </header>
                <p className="detail-empty">
                  该分组下有 {sessions.filter((s) => s.groupId === selectedGroup.id).length}{" "}
                  个会话、{groups.filter((g) => g.parentId === selectedGroup.id).length} 个子分组。
                </p>
                </section>
              </section>
            ) : !activeSession ? (
              <section className="welcome">
                <div className="welcome-logo">
                  <IconTerminal size={44} />
                </div>
                <h2>RustMobaX</h2>
                <p>从左侧会话树双击，或用 Quick connect 输入 user@host:port</p>
                <div className="welcome-actions">
                  <button
                    type="button"
                    className="primary"
                    onClick={() => setDialog({ kind: "session", mode: "create", session: null })}
                  >
                    新建会话
                  </button>
                  <button type="button" onClick={() => void startImport()}>
                    导入 MobaXterm 会话
                  </button>
                </div>
              </section>
            ) : null}
          </div>

          {/* 远程监控挂在会话主区底部：侧栏切到文件/工具/宏分页时它不会跟着消失。
              判据是「激活会话 + 它真的有活跃终端」：关掉连接但标签还留着时 terminalIds
              里已经没有这条会话，面板随之消失，不再展示过期或本机数据；
              本地终端同样有 terminalId -> 照常显示；SFTP / rdp / vnc 没有终端就不占高度 */}
          {activeTerminalId !== null && (
            <section className="monitor-panel" aria-label="远程监控">
              <button
                type="button"
                className="monitor-head"
                aria-expanded={monitorOpen}
                onClick={() => setMonitorOpen((prev) => !prev)}
                title="展开 / 收起远程监控（状态栏始终显示精简指标）"
              >
                <span className={`monitor-caret ${monitorOpen ? "is-open" : ""}`}>
                  <IconChevronRight size={12} />
                </span>
                <IconActivity size={12} />
                <span className="monitor-title">远程监控</span>
                {/* 来源标记常驻：文字直说数据来自哪台机器，不靠颜色区分 */}
                <span className={`monitor-source is-${metrics.scope}`} title={metricsSourceHint}>
                  {metricsSourceText}
                </span>
              </button>
              {monitorOpen && (
                <>
                  {monitorNote !== null && (
                    <p className="monitor-note" title={metricsSourceHint}>
                      {monitorNote}
                    </p>
                  )}
                  <div className="monitor-strip">
                    <MonitorCard
                      title="CPU"
                      value={cpuPercent === null ? "—" : `${cpuPercent}%`}
                      percent={cpuPercent}
                      hint={`CPU 使用率 · ${metricsSourceText}`}
                    />
                    <MonitorCard
                      title="内存"
                      value={memText}
                      detail={memPercent === null ? undefined : `${memPercent}%`}
                      percent={memPercent}
                      hint={`内存占用 ${memText} · ${metricsSourceText}`}
                    />
                    <MonitorCard
                      title="磁盘"
                      value={diskText}
                      detail={diskPercent === null ? undefined : `${diskPercent}%`}
                      percent={diskPercent}
                      hint={diskHint}
                    />
                    <MonitorCard
                      title="网络"
                      value={metricsSupported ? `↓ ${formatRate(metricsValue?.rxKbps ?? 0)}` : "↓ —"}
                      detail={metricsSupported ? `↑ ${formatRate(metricsValue?.txKbps ?? 0)}` : "↑ —"}
                      hint={`网络收发速率 · ${metricsSourceHint}`}
                    />
                    {/* 没有 GPU 时保留这块位置并写明「未检测到」，让人知道是没检测到而不是坏了 */}
                    {gpus.length === 0 ? (
                      <div
                        className="monitor-card is-empty"
                        title={
                          metrics.scope === "remote"
                            ? "远端没有采集到 GPU：无独立显卡、驱动不提供查询或采样失败"
                            : "GPU 只在远端会话上采样，本机口径不采集显卡信息"
                        }
                      >
                        <span className="monitor-card-title">GPU</span>
                        <span className="monitor-card-empty">未检测到 GPU</span>
                      </div>
                    ) : (
                      // measurable 为 false 说明只认出了型号（驱动没装），只报型号与原因，
                      // 绝不能把 0 当成「利用率 0%」画出来骗人
                      gpus.map((gpu, index) =>
                        gpu.measurable ? (
                          <MonitorCard
                            key={`${gpu.name}-${index}`}
                            title={gpu.name}
                            value={`${Math.round(gpu.utilPercent)}%`}
                            detail={`显存 ${formatMemory(gpu.memUsedMb)} / ${formatMemory(gpu.memTotalMb)}${
                              gpu.tempC === null ? "" : ` · ${Math.round(gpu.tempC)}°C`
                            }`}
                            percent={gpu.utilPercent}
                            hint={`GPU 利用率与显存 · ${metricsSourceText}`}
                          />
                        ) : (
                          <div
                            key={`${gpu.name}-${index}`}
                            className="monitor-card is-empty"
                            title={`仅凭 lspci 认出型号，驱动没有提供利用率/显存/温度 · ${metricsSourceText}`}
                          >
                            {/* 名称在卡片里最终还会被省略号截断（后端已去掉厂商前缀和 (rev xx)），
                                所以这一行自己带 title：鼠标停在名字上先想知道「这是哪张卡」，
                                型号放最前面，原因跟在后面补一句；卡片空白处的 title 仍只讲原因与来源，
                                两处各答一个问题，不用来回找。 */}
                            <span
                              className="monitor-card-title"
                              title={`${gpu.name}（仅凭 lspci 认出型号，驱动没有提供利用率/显存/温度）`}
                            >
                              {gpu.name}
                            </span>
                            <span className="monitor-card-note">仅识别型号，无法采集指标</span>
                          </div>
                        ),
                      )
                    )}
                  </div>
                </>
              )}
            </section>
          )}
        </main>
      </div>

      <footer className={`statusbar ${notice?.tone === "error" ? "is-error" : ""}`}>
        <span className="status-text">{statusText}</span>
        <span className="status-meta">
          {sessions.length} 会话 · {groups.length} 分组 · {tabs.length} 标签
        </span>
        {notice && (
          <button
            type="button"
            className="status-close"
            aria-label="清除状态提示"
            onClick={() => setNotice(null)}
          >
            ×
          </button>
        )}
        <span className="status-metrics" role="group" aria-label="系统指标">
          {/* 来源标记放在三个芯片前面：文字直说数据来自哪台机器，不只靠颜色 */}
          <span
            className={`metric-source is-${metrics.scope}`}
            title={metricsSourceHint}
            aria-label={metricsSourceHint}
          >
            {metricsSourceText}
          </span>
          <span className="metric-chip" title={`CPU 使用率 · ${metricsSourceText}`}>
            <span className="metric-label">CPU</span>
            <span className="metric-value">
              {metricsSupported ? `${Math.round(metricsValue?.cpuPercent ?? 0)}%` : "--"}
            </span>
          </span>
          <span className="metric-chip" title={`内存使用率 · ${metricsSourceText}`}>
            <span className="metric-label">内存</span>
            <span className="metric-value">
              {metricsSupported ? `${Math.round(metricsValue?.memPercent ?? 0)}%` : "--"}
            </span>
          </span>
          <span
            className="metric-chip"
            title={
              metricsSupported
                ? `${metricsSourceText} · 上行 ${formatRate(metricsValue?.txKbps ?? 0)}，下行 ${formatRate(metricsValue?.rxKbps ?? 0)}`
                : `网络收发速率 · ${metricsSourceHint}`
            }
          >
            <span className="metric-label">网络</span>
            <span className="metric-value">
              {metricsSupported
                ? `↑ ${formatRate(metricsValue?.txKbps ?? 0)} ↓ ${formatRate(metricsValue?.rxKbps ?? 0)}`
                : "↑ -- ↓ --"}
            </span>
          </span>
        </span>
      </footer>

      {dialog.kind === "session" && (
        <SessionDialog
          mode={dialog.mode}
          initial={dialog.session}
          draft={dialog.draft ?? null}
          groups={groups}
          credentials={credentials}
          defaultGroupId={selectedGroupId}
          busy={busy}
          onSubmit={(input) => void submitSession(input)}
          onCancel={() => setDialog({ kind: "none" })}
        />
      )}

      {dialog.kind === "group" && (
        <GroupPromptDialog
          title={dialog.mode === "create" ? "新建分组" : "重命名分组"}
          label="分组名称"
          initialValue={dialog.group?.name ?? ""}
          busy={busy}
          onSubmit={(name) => void submitGroup(name)}
          onCancel={() => setDialog({ kind: "none" })}
        />
      )}

      {dialog.kind === "import" && (
        <ImportPreviewDialog
          preview={dialog.preview}
          busy={busy}
          onConfirm={() => void confirmImport(dialog.preview)}
          onCancel={() => setDialog({ kind: "none" })}
        />
      )}

      {dialog.kind === "knownHosts" && (
        <KnownHostsDialog
          onNotify={(text) => setNotice({ tone: "info", text })}
          onClose={() => setDialog({ kind: "none" })}
        />
      )}

      {(dialog.kind === "tunnel" || dialog.kind === "tunnelForm") && (
        <TunnelDialog
          sessions={sessions.filter((session) => isSshLike(session.protocol))}
          revision={tunnelRevision}
          onConfirmHostKey={requestHostKeyTrust}
          onOpenForm={(config) => setDialog({ kind: "tunnelForm", config })}
          onNotify={(text) => setNotice({ tone: "info", text })}
          needsVaultBeforeStart={(ids) => deferTunnelStartsForVault(ids, { fromList: true })}
          onClose={() => setDialog({ kind: "none" })}
        />
      )}

      {dialog.kind === "tunnelForm" && (
        <TunnelFormDialog
          config={dialog.config}
          sessions={sessions.filter((session) => isSshLike(session.protocol))}
          onSaved={() => {
            setTunnelRevision((value) => value + 1);
            setDialog({ kind: "tunnel" });
          }}
          onCancel={() => setDialog({ kind: "tunnel" })}
        />
      )}

      {dialog.kind === "settings" && (
        <SettingsDialog
          settings={settings}
          customThemes={customThemes}
          systemDark={systemDark}
          onChange={applySetting}
          onReset={resetSettings}
          onReloadThemes={loadCustomThemes}
          onNotice={(text) => setNotice({ tone: "info", text })}
          onClose={() => setDialog({ kind: "none" })}
        />
      )}

      {dialog.kind === "about" && <AboutDialog onClose={() => setDialog({ kind: "none" })} />}

      {/* 命令面板挂在弹窗区最后：它自己控制显隐，与其它弹窗互不干扰 */}
      <CommandPalette
        open={paletteOpen}
        commands={paletteCommands}
        onClose={() => setPaletteOpen(false)}
      />

      {pendingSsh && (
        <SecretPromptDialog
          title="SSH 密码"
          label={`${pendingSsh.username ?? ""}@${pendingSsh.host ?? ""} 的密码`}
          hint="密码只保存在内存中，关闭应用后失效；勾选存入凭据库可加密保存复用。"
          busy={busy}
          allowSaveToVault={vault.unlocked}
          vaultInitialized={vault.initialized}
          onRequestVault={requestVaultForPrompt}
          defaultVaultLabel={`${pendingSsh.username ?? ""}@${pendingSsh.host ?? ""} 密码`}
          onSubmitWithSave={(secret, save, label) =>
            void acceptSecret(
              pendingSsh,
              secret,
              { kind: "password", password: secret },
              save ? { kind: "password", label } : null,
            )
          }
          onCancel={() => setPendingSsh(null)}
        />
      )}

      {pendingKeyPass && (
        <SecretPromptDialog
          title="私钥口令"
          label={`${pendingKeyPass.session.username ?? ""}@${pendingKeyPass.session.host ?? ""} 的私钥口令`}
          hint="口令只保存在内存中，关闭应用后失效；勾选存入凭据库可加密保存复用。"
          busy={busy}
          allowSaveToVault={vault.unlocked}
          vaultInitialized={vault.initialized}
          onRequestVault={requestVaultForPrompt}
          defaultVaultLabel={`${pendingKeyPass.session.username ?? ""}@${pendingKeyPass.session.host ?? ""} 私钥口令`}
          onSubmitWithSave={(secret, save, label) =>
            void acceptSecret(
              pendingKeyPass.session,
              secret,
              { kind: "privateKey", path: pendingKeyPass.path, passphrase: secret },
              save ? { kind: "key_passphrase", label } : null,
            )
          }
          onCancel={() => setPendingKeyPass(null)}
        />
      )}

      {dialog.kind === "vault" && (
        <VaultDialog
          status={vault}
          credentials={credentials}
          busy={busy}
          error={vaultError}
          onSetup={(password) =>
            void runVaultAction(() => vaultSetup(password), resumePendingSession)
          }
          onUnlock={(password) =>
            void runVaultAction(() => vaultUnlock(password), resumePendingSession)
          }
          onLock={() => void runVaultAction(() => vaultLock())}
          onSave={(label, secret) =>
            void runVaultAction(async () => {
              await credentialSave("password", label, secret);
              setNotice({ tone: "info", text: `已保存凭据「${label}」` });
            })
          }
          onDelete={(id) => void runVaultAction(() => credentialDelete(id).then(() => undefined))}
          autoStartCount={autoStartVaultHint ?? undefined}
          onClose={() => {
            // 启动探测主动弹的那次被关掉 -> 本次会话不再自动弹；只影响「弹窗」，
            // 用户之后自己解锁仍会触发自动启动（见阶段 declined 的处理）
            if (autoStartPhase.current === "awaitingVault") {
              autoStartPhase.current = "declined";
            }
            // 为用户主动启动弹的解锁框被关掉 -> 静默丢弃这批启动：用户已经用
            // 「关闭」表达不想输密码，再甩一句错误只会添乱；顺手清掉「来自列表」
            // 的标记，免得事后再突然弹一个隧道列表出来
            pendingTunnelStarts.current = [];
            pendingTunnelStartsFromList.current = false;
            setAutoStartVaultHint(null);
            setPendingVaultOpen(null);
            setVaultResumePrompt(false);
            setDialog({ kind: "none" });
          }}
        />
      )}

      {hostKeyPrompt && (
        <HostKeyDialog
          info={hostKeyPrompt.info}
          mode={hostKeyPrompt.mode}
          busy={busy}
          onTrust={() => void trustAndReconnect()}
          onForget={() => void forgetAndClose()}
          onCancel={cancelHostKeyPrompt}
        />
      )}

      {confirmState && (
        <ConfirmDialog
          message={confirmState.message}
          onConfirm={() => {
            const action = confirmState.run;
            setConfirmState(null);
            action();
          }}
          onCancel={() => setConfirmState(null)}
        />
      )}
    </div>
  );
}

export default App;
