import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { errorDetail, errorKind, errorMessage } from "../api/session";
import type { Session } from "../api/session";
import type { HostKeyInfo, SshAuthRequest } from "../api/terminal";
import {
  SFTP_WRITE_LIMIT_BYTES,
  formatBytes,
  listLocalDir,
  remoteDisk,
  sftpCancelTransfer,
  sftpClose,
  sftpConnect,
  sftpDownloadRemoteFile,
  sftpList,
  sftpMkdir,
  sftpReadText,
  sftpRemove,
  sftpRename,
  sftpUploadLocalFile,
  sftpWriteText,
} from "../api/sftp";
import type {
  LocalEntry,
  RemoteDisk,
  SftpEntry,
  TransferEvent,
  TransferHandle,
} from "../api/sftp";
import { runPool } from "../lib/pool";
import { useEscapeClose } from "../lib/useEscapeClose";
import { ConfirmDialog } from "./ConfirmDialog";
import { IconClose } from "./icons";

type Props = {
  session: Session;
  /** SSH 认证方式，只在内存里传递。 */
  sshAuth?: SshAuthRequest | null;
  /** 用户已确认过该主机指纹。 */
  acceptUnknownHostKey?: boolean;
  /** 首次连接遇到未知主机 -> 交给上层弹确认框。 */
  onHostKeyPrompt?: (sessionId: string, info: HostKeyInfo) => void;
  /** 指纹与记录不一致 -> 交给上层做硬阻断提示。 */
  onHostKeyMismatch?: (sessionId: string, info: HostKeyInfo) => void;
  /** 同会话终端上报的工作目录（OSC 7），有值时面板跟着跳过去。 */
  cwd?: string | null;
  /** 是否跟随外部 cwd 变化；关掉时忽略终端上报，默认开。 */
  followCwd?: boolean;
  /**
   * 切换「跟随终端当前工作目录」开关。
   *
   * 设置值本身由上层持有（写库统一走 applySetting），面板只负责把这个开关
   * 放到文件分页的工具栏上；不传时开关只读，避免出现改了却不生效的假开关。
   */
  onFollowCwdChange?: (next: boolean) => void;
  /**
   * 同会话已连接的终端标识，供远端磁盘占用查询使用。
   * 终端没连上时为 null，此时磁盘那行整行隐藏。
   */
  terminalId?: string | null;
};

/** 传输方向：决定调哪条后端命令，以及进度行里的措辞 */
type TransferDirection = "upload" | "download";

/** 单个文件的传输任务，上传与下载共用同一张表 */
type FileTask = {
  /** 显示用的文件名 */
  name: string;
  /** 远端路径 */
  remotePath: string;
  /** 本机路径 */
  localPath: string;
  /** 预期字节数，0 表示前端也没拿到大小 -> 进度条走不确定态 */
  size: number;
};

/** 进度行的阶段：running 进行中，cancelling 已点取消在等后端回话 */
type TransferPhase = "running" | "cancelling" | "cancelled" | "error";

/**
 * 进度行状态。
 *
 * 字节一律按「本批已累计」记，不做单文件百分比来回覆盖，
 * 因此并发 3 个文件时数字只会单调上涨，不会因谁先回调而跳动。
 */
type TransferState = {
  /** 最近上报进度的文件名 */
  name: string;
  phase: TransferPhase;
  /** 本批已完成文件数 */
  done: number;
  /** 本批文件总数，1 表示单文件（此时不显示「第 i/n 个」） */
  total: number;
  /** 已累计字节（含续传起点） */
  bytes: number;
  /** 本批预期总字节，0 表示未知 -> 走不确定态条纹 */
  totalBytes: number;
  /** 失败原因或取消提示，running 时为 null */
  message: string | null;
  /** 失败/取消后带上的重试上下文，null 表示这条进度行不能重试 */
  retry: TransferRetry | null;
};

/** 失败/取消后留给「重试」「继续传输」的上下文，存成纯数据便于直接放 state */
type TransferRetry = {
  direction: TransferDirection;
  id: string;
  /** 还没传完的文件，保持原顺序（失败那个排在第一位） */
  tasks: FileTask[];
  /** 需要接着 .rmxpart 续传的远端路径 */
  resume: string[];
  /** 原批的账，重试时接着累加，数字不回退 */
  done: number;
  total: number;
  totalBytes: number;
  written: [string, number][];
};

/**
 * 一次传输作业的可变账本。
 *
 * 进度回调、并发池、取消按钮都读写同一个对象，
 * 这样新起的传输不会读到上一次渲染遗留的 state。
 */
type TransferLedger = {
  direction: TransferDirection;
  id: string;
  /** 本批文件总数 */
  total: number;
  /** 本批预期总字节，0 表示未知 */
  totalBytes: number;
  /** 每个文件已写字节，key 为远端路径；累计显示靠它累加 */
  written: Map<string, number>;
  /** 已完成文件数（重试时接着累加） */
  done: number;
  /** 最近一个上报进度的文件名 */
  name: string;
  /** 本次是否已经请求取消 */
  cancelling: boolean;
  /** 在途任务的取消令牌，取消时逐个点掉 */
  tokens: Set<string>;
  /** 上次刷 UI 的时间戳，用来给高频进度事件做节流 */
  lastPaint: number;
};

/** 一批文件跑完的结果 */
type BatchResult = {
  ledger: TransferLedger;
  /** 还没传完的文件（失败或取消后剩的），全部成功时为空 */
  remaining: FileTask[];
  /** 第一个失败的文件与原因 */
  failure: { task: FileTask; message: string } | null;
  /** 是否被用户取消 */
  cancelled: boolean;
};

type Status = "connecting" | "ready" | "error";

/** 待提交的单输入框弹窗状态 */
type PromptState = {
  kind: "mkdir" | "rename";
  title: string;
  label: string;
  initial: string;
  /** rename 时的目标条目，mkdir 时为 null */
  target: SftpEntry | null;
};

/** 内置编辑器的打开状态 */
type EditorState = {
  path: string;
  content: string;
  /** 打开时或上次保存时的内容，用于判断是否有未保存修改 */
  saved: string;
};

/**
 * 把字节数转成人类可读的大小。
 *
 * 参数：bytes 为字节数
 * 核心流程：实现已挪到 api/sftp.ts 的 formatBytes，目录列表、磁盘条、
 * 传输进度共用同一口径；这里保留原名方便阅读调用处。
 */
const formatSize = formatBytes;

/**
 * 格式化远端修改时间。
 *
 * 参数：seconds 为 Unix 秒
 * 核心流程：0 表示服务端没给时间，显示占位符；其余按本地时区显示。
 */
function formatTime(seconds: number): string {
  if (!seconds) return "-";
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false });
}

/**
 * 默认目录的路径取值。
 *
 * 空串只在部分实现里等于家目录，OpenSSH 会直接报 No such file，
 * 所以统一用 POSIX 的当前目录 "."（登录后就是家目录）。
 */
const DEFAULT_DIR = ".";

/** 容器窄于这个宽度时切紧凑行布局（px）：隐藏大小/时间列，操作收进「⋯」 */
const NARROW_MAX_WIDTH = 360;

/**
 * 取 POSIX 路径的父目录。
 *
 * 参数：path 为当前路径
 * 核心流程：去掉末尾斜杠后截断最后一段；已经是根时回根，
 * 已经在默认目录（"." 或空）时保持默认目录，不产生 "./.." 这类路径。
 */
function parentPath(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  if (trimmed === "" || trimmed === DEFAULT_DIR) return DEFAULT_DIR;
  const index = trimmed.lastIndexOf("/");
  if (index < 0) return DEFAULT_DIR;
  if (index === 0) return "/";
  return trimmed.slice(0, index);
}

/**
 * 拼接远端路径。
 *
 * 参数：dir 为所在目录（默认目录记为 "."），name 为条目名
 * 核心流程：纯字符串拼接，避免出现重复斜杠与 "./" 前缀。
 */
function joinPath(dir: string, name: string): string {
  if (dir === "" || dir === DEFAULT_DIR) return name;
  if (dir === "/") return `/${name}`;
  return `${dir.replace(/\/+$/, "")}/${name}`;
}

/**
 * 取本地路径的文件名。
 *
 * 参数：path 为本地绝对路径
 * 核心流程：Windows 与 POSIX 的分隔符都切一刀，取最后一段。
 */
function localBaseName(path: string): string {
  const segments = path.split(/[\\/]/);
  return segments[segments.length - 1] || path;
}

/**
 * 取文件名的扩展名（含点）。
 *
 * 参数：name 为文件名或路径
 * 核心流程：只看最后一段；没有点、点在开头（`.bashrc` 这类隐藏文件）或点在
 * 末尾都算「没有扩展名」，返回空串，避免拼出 `Makefile.Makefile` 这种名字。
 */
function fileExtension(name: string): string {
  const base = localBaseName(name);
  const dot = base.lastIndexOf(".");
  if (dot <= 0 || dot === base.length - 1) return "";
  return base.slice(dot);
}

/**
 * 让保存路径带上远端文件的扩展名。
 *
 * 参数：target 为保存对话框返回的路径，remoteName 为远端文件名
 * 核心流程：远端没有扩展名就原样返回；保存路径本身已经以同一扩展名结尾
 * （大小写不敏感，`.BIN` 与 `.bin` 视为一致）也原样返回，
 * 只有确实缺扩展名时才补一次，因此用户输入的 `dl-big60.bin` 不会被改成
 * `dl-big60.bin.bin`。
 */
function withRemoteExtension(target: string, remoteName: string): string {
  const ext = fileExtension(remoteName);
  if (ext === "") return target;
  return target.toLowerCase().endsWith(ext.toLowerCase()) ? target : `${target}${ext}`;
}

/**
 * 判断一个本地路径是目录还是文件。
 *
 * 参数：path 为本机绝对路径
 * 核心流程：能列出子项就是目录，返回子项列表；列不出来说明是文件
 * （拖放事件只给路径不给类型，只能这样探一次，目录只探一次不重复读）。
 */
async function probeLocalDir(path: string): Promise<LocalEntry[] | null> {
  try {
    return await listLocalDir(path);
  } catch {
    return null;
  }
}

/** 目录传输时同时在传的文件数：3 个足够填满带宽，也不会把 SFTP 通道挤爆 */
const TRANSFER_CONCURRENCY = 3;

/** 失败或取消后进度行保留的时长（毫秒），期间可以点「重试」「继续传输」 */
const PROGRESS_HOLD_MS = 20_000;

/** 进度事件刷新 UI 的最小间隔（毫秒）：后端回调很密，节流后数字仍然连续可读 */
const PROGRESS_PAINT_MS = 80;

/**
 * 从父目录列表里读本机文件大小。
 *
 * 参数：path 为本机文件绝对路径
 * 核心流程：按文件名反推父目录再列一次，找到同名条目取 size；
 * 拿不到就返回 0，调用方把它当成「大小未知」，进度条退化成不确定态，
 * 绝不编一个数字出来。
 */
async function localFileSize(path: string): Promise<number> {
  const name = localBaseName(path);
  const parent = path.slice(0, Math.max(0, path.length - name.length)).replace(/[\\/]+$/, "");
  try {
    const list = await listLocalDir(parent === "" ? "/" : parent);
    const hit = list.find((item) => item.name === name && !item.isDir);
    return hit && Number.isFinite(hit.size) && hit.size > 0 ? hit.size : 0;
  } catch {
    return 0;
  }
}

/**
 * 探一个本机路径：是目录还是文件，文件的话顺带拿到大小。
 *
 * 参数：path 为本机绝对路径
 * 核心流程：能列出子项就是目录；列不出来按文件处理，再补一次父目录列表取大小。
 * 拖放事件只给路径不给类型，整目录上传也靠它递归。
 */
async function probeLocalPath(
  path: string,
): Promise<{ isDir: true; children: LocalEntry[] } | { isDir: false; size: number }> {
  const children = await probeLocalDir(path);
  if (children !== null) return { isDir: true, children };
  return { isDir: false, size: await localFileSize(path) };
}

/**
 * 把账本里的已写字节加总。
 *
 * 参数：written 为「远端路径 -> 已写字节」表
 * 核心流程：逐个累加，缺失的按 0 算；并发时每个文件各记一份，加起来就是本批总数。
 */
function sumWritten(written: Map<string, number>): number {
  let total = 0;
  for (const value of written.values()) total += value;
  return total;
}

/**
 * 远端标识过长时中间省略。
 *
 * 参数：text 为完整文案，limit 为大致能放下的字符数
 * 核心流程：短文本原样返回；长文本按「头短尾长」比例保留，中间插省略号，
 * 优先保住最能定位的尾段（目录名），完整值仍放在 title 里。
 */
function middleEllipsis(text: string, limit: number): string {
  if (text.length <= limit) return text;
  const tail = Math.max(8, Math.floor(limit * 0.62));
  const head = Math.max(6, limit - tail - 1);
  return `${text.slice(0, head)}…${text.slice(text.length - tail)}`;
}

/** 磁盘占用的三档水位，文字与颜色同时表达，不只靠颜色 */
type DiskLevel = "normal" | "high" | "critical";

/**
 * 按使用率判定水位。
 *
 * 参数：percent 为使用率（0 到 100）
 * 核心流程：低于 75% 正常，75% 到 90% 偏高，超过 90% 告急。
 */
function diskLevel(percent: number): DiskLevel {
  if (percent > 90) return "critical";
  if (percent >= 75) return "high";
  return "normal";
}

/** 水位文案：跟百分比并排显示，色觉不敏感的用户也能读懂 */
function diskLevelText(level: DiskLevel): string {
  if (level === "critical") return "告急";
  if (level === "high") return "偏高";
  return "正常";
}

/**
 * 计算字符串的 UTF-8 字节数。
 *
 * 参数：text 为待测文本
 * 核心流程：按 UTF-8 编码后取长度，用于卡住 2 MiB 的写入上限。
 */
function utf8Length(text: string): number {
  return new TextEncoder().encode(text).length;
}

/**
 * 面板内的单输入框弹窗（新建目录 / 重命名）。
 *
 * 参数：title/label 为文案，initial 为初始值，busy 为提交中标记，
 *       onSubmit 为提交回调，onCancel 为取消回调
 * 核心流程：本地维护输入 -> 空值与斜杠拦下 -> 交给面板去发远端命令。
 */
function NameDialog(props: {
  title: string;
  label: string;
  initial: string;
  busy: boolean;
  onSubmit: (value: string) => void;
  onCancel: () => void;
}) {
  const { title, label, initial, busy, onSubmit, onCancel } = props;
  const [value, setValue] = useState(initial);
  const [error, setError] = useState<string | null>(null);

  // Esc = 取消；提交中「取消」按钮禁用，Esc 也一并失效
  useEscapeClose(onCancel, !busy);

  /**
   * 提交输入值。
   *
   * 参数：无
   * 核心流程：去空白后为空或带 / 则提示，否则上交面板。
   */
  const submit = () => {
    const trimmed = value.trim();
    if (trimmed === "") {
      setError("名称不能为空");
      return;
    }
    if (trimmed.includes("/")) {
      setError("名称里不能带斜杠");
      return;
    }
    onSubmit(trimmed);
  };

  return (
    <div className="modal-backdrop" role="presentation">
      <div className="modal modal-small" role="dialog" aria-label={title}>
        <h2>{title}</h2>
        <label className="field">
          <span>{label}</span>
          <input
            value={value}
            autoFocus
            onChange={(event) => {
              setValue(event.target.value);
              setError(null);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") submit();
            }}
          />
        </label>
        {error && <p className="form-error">{error}</p>}
        <div className="modal-actions">
          <button type="button" onClick={onCancel} disabled={busy}>
            取消
          </button>
          <button type="button" className="primary" onClick={submit} disabled={busy}>
            {busy ? "提交中…" : "确定"}
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * SFTP 文件面板。
 *
 * 参数：见 Props
 * 核心流程：挂载时自建一条 SFTP 连接并列目录 -> 目录内导航靠 sftp_list ->
 * 单个文件的上传下载走流式命令（后端分块拷贝，不限大小、内容不过前端） ->
 * 卸载或关标签时 sftp_close 回收连接；
 * 面板常驻在左侧栏的「文件」分页里，切分页只是 CSS 隐藏，不会重连；
 * 容器变窄（侧栏被拖窄）时切成紧凑行：只留名称列，操作收进「⋯」展开行。
 */
export function SftpPanel({
  session,
  sshAuth,
  acceptUnknownHostKey = false,
  onHostKeyPrompt,
  onHostKeyMismatch,
  cwd,
  followCwd = true,
  onFollowCwdChange,
  terminalId = null,
}: Props) {
  const panelRef = useRef<HTMLElement | null>(null);
  const [clientId, setClientId] = useState<string | null>(null);
  /** 容器是否窄到要切紧凑布局 -> 由 ResizeObserver 量出来，不靠媒体查询 */
  const [narrow, setNarrow] = useState(false);
  /** 哪一行的「⋯」已展开（存远端路径），null 表示都没开 */
  const [actionRow, setActionRow] = useState<string | null>(null);
  const [status, setStatus] = useState<Status>("connecting");
  const [connectError, setConnectError] = useState<string | null>(null);
  /** 当前目录，DEFAULT_DIR（"."）表示服务端默认目录（家目录） */
  const [path, setPath] = useState(DEFAULT_DIR);
  /** 路径输入框的草稿值，默认态留空好让占位文案显示出来 */
  const [pathInput, setPathInput] = useState("");
  const [entries, setEntries] = useState<SftpEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [listError, setListError] = useState<string | null>(null);
  /** 操作回执，成功与失败都往这里写 */
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** busy 的同步副本：拖放回调与并发池不经过渲染，靠它挡住第二宗传输 */
  const busyRef = useRef(false);
  /** 传输进度行，null 表示当前没有传输（成功后立刻清空，失败保留 20 秒） */
  const [transfer, setTransfer] = useState<TransferState | null>(null);
  /** 失败/取消后留下的重试上下文由进度行自己带着，这里只记在途令牌 */
  /** 当前在跑的账本，取消按钮靠它拿在途令牌 */
  const ledgerRef = useRef<TransferLedger | null>(null);
  /** 进度行 20 秒后自动消失的定时器 */
  const progressTimerRef = useRef<number | null>(null);
  /** 当前远端路径所在分区的磁盘占用，null 表示拿不到（整行隐藏） */
  const [disk, setDisk] = useState<RemoteDisk | null>(null);
  /** 上次成功取到磁盘数据的时间戳，用来判断是否超过 10 秒需要重取 */
  const diskFetchedAt = useRef(0);
  /** 手动触发一次重取（传完文件、删完文件后容量变了，不能等 10 秒） */
  const [diskTick, setDiskTick] = useState(0);
  /** 拖入本机路径时的覆盖层（值为拖入的路径数，0 表示没在拖） */
  const [dropCount, setDropCount] = useState(0);
  /** 拖放降级的回执：原生事件不可用时的原因，非 null 表示已退回 HTML5 */
  const [dropFallback, setDropFallback] = useState<string | null>(null);
  /** 上传方式下拉是否展开 */
  const [uploadMenu, setUploadMenu] = useState(false);
  /** 上传按钮与下拉包在一个容器里，用来判断点击是否落在内部 */
  const uploadMenuRef = useRef<HTMLDivElement | null>(null);
  const [prompt, setPrompt] = useState<PromptState | null>(null);
  const [removing, setRemoving] = useState<SftpEntry | null>(null);
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [editorError, setEditorError] = useState<string | null>(null);
  const [editorBusy, setEditorBusy] = useState(false);

  /**
   * 关闭内置编辑器。
   *
   * 参数：无
   * 核心流程：清空编辑器状态与上一次的保存错误；未保存的改动直接丢弃，
   * 与标题栏 × / 底部「关闭（不保存）」保持同一种语义。
   */
  const closeEditor = () => {
    setEditor(null);
    setEditorError(null);
  };

  // Esc = 关闭编辑器（等价于 × /「关闭（不保存）」，也不做二次确认）；
  // 编辑器没开或正在写入时不入栈，避免和保存流程抢时间
  useEscapeClose(closeEditor, editor !== null && !editorBusy);

  /**
   * 量一次自身宽度决定是否用紧凑布局。
   *
   * 参数：无
   * 核心流程：监听面板自身的宽度变化（侧栏拖动会改变它），
   * 窄于阈值时只留名称列与「⋯」，避免出现横向滚动条压住内容；
   * 环境不支持 ResizeObserver 时保持宽布局（窗口还有足够空间）。
   */
  useEffect(() => {
    const node = panelRef.current;
    if (!node || typeof ResizeObserver === "undefined") return undefined;
    const observer = new ResizeObserver((records) => {
      const width = records[0]?.contentRect.width ?? node.clientWidth;
      setNarrow(width < NARROW_MAX_WIDTH);
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, []);

  // 布局切换或目录变化时收起展开的操作行 -> 不给新列表留下错位的展开态
  useEffect(() => {
    setActionRow(null);
  }, [narrow, path]);

  // 卸载时把进度行的自动消失定时器收掉，关标签后不再往已卸载的组件写 state
  useEffect(() => {
    return () => {
      if (progressTimerRef.current !== null) {
        window.clearTimeout(progressTimerRef.current);
        progressTimerRef.current = null;
      }
    };
  }, []);

  /**
   * 读取一个远端目录。
   *
   * 参数：id 为会话标识，target 为目录路径
   * 核心流程：置 loading -> 取列表 -> 成功才更新当前目录与输入框；
   * 失败时保留原目录并展示原因，避免界面漂到一个读不了的位置。
   * 默认目录不把 "." 写进输入框，让占位文案接管显示。
   */
  const loadDir = useCallback(async (id: string, target: string) => {
    setLoading(true);
    setListError(null);
    try {
      const list = await sftpList(id, target);
      setEntries(list);
      setPath(target);
      setPathInput(target === DEFAULT_DIR ? "" : target);
    } catch (err) {
      setEntries([]);
      setListError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, []);

  // 建立本面板自己的 SFTP 连接 -> 认证或指纹变化才重连
  useEffect(() => {
    let disposed = false;
    let opened: string | null = null;
    setStatus("connecting");
    setConnectError(null);
    setEntries([]);

    void sftpConnect({
      host: session.host ?? "",
      port: session.port ?? 22,
      username: session.username ?? "",
      auth: sshAuth ?? { kind: "password", password: "" },
      acceptUnknownHostKey,
    })
      .then(async (id) => {
        if (disposed) {
          // 面板已经卸载 -> 立刻回收这条连接
          void sftpClose(id);
          return;
        }
        opened = id;
        setClientId(id);
        setStatus("ready");
        await loadDir(id, DEFAULT_DIR);
      })
      .catch((reason) => {
        const kind = errorKind(reason);
        const detail = errorDetail<HostKeyInfo>(reason);
        setStatus("error");
        if (kind === "hostKeyUnknown" && detail) {
          setConnectError("需要确认主机指纹后才能连接");
          onHostKeyPrompt?.(session.id, detail);
          return;
        }
        if (kind === "hostKeyMismatch" && detail) {
          setConnectError("主机密钥已变更，连接已阻断");
          onHostKeyMismatch?.(session.id, detail);
          return;
        }
        setConnectError(errorMessage(reason));
      });

    return () => {
      disposed = true;
      setClientId(null);
      if (opened) void sftpClose(opened);
    };
  }, [
    session.id,
    session.host,
    session.port,
    session.username,
    sshAuth,
    acceptUnknownHostKey,
    onHostKeyPrompt,
    onHostKeyMismatch,
    loadDir,
  ]);

  // 终端报了新的工作目录 -> 面板跟着跳过去（收不到就一直手动导航）；
  // 用户关掉「跟随终端目录」后这里直接忽略外部 cwd
  useEffect(() => {
    if (!followCwd || !clientId || !cwd) return;
    void loadDir(clientId, cwd);
  }, [cwd, clientId, followCwd, loadDir]);

  /**
   * 取当前远端路径所在分区的磁盘占用。
   *
   * 参数：无（读 terminalId 与 path）
   * 核心流程：目录一变就重取（不同目录可能落在不同分区）；
   * 终端没连上（terminalId 为空）或命令失败时把数据清空，整行隐藏，
   * 不显示上一次的数字、也不猜一个数字出来。
   */
  useEffect(() => {
    if (status !== "ready" || !terminalId) {
      setDisk(null);
      diskFetchedAt.current = 0;
      return undefined;
    }
    let disposed = false;
    void (async () => {
      try {
        const value = await remoteDisk(terminalId, path);
        if (disposed) return;
        if (value.totalBytes > 0) {
          setDisk(value);
          diskFetchedAt.current = Date.now();
        } else {
          setDisk(null);
        }
      } catch {
        if (disposed) return;
        setDisk(null);
        diskFetchedAt.current = 0;
      }
    })();
    return () => {
      disposed = true;
    };
  }, [status, terminalId, path, diskTick]);

  /**
   * 磁盘数据超过 10 秒就重取一次。
   *
   * 参数：无
   * 核心流程：10 秒一跳，只有确实过期才发命令；
   * 面板被切到别的分页（CSS 隐藏）或窗口不在前台时跳过，不白跑一次 df；
   * 失败同样清空数据让整行隐藏，避免看着过期数字以为容量没变。
   */
  useEffect(() => {
    if (status !== "ready" || !terminalId) return undefined;
    const timer = window.setInterval(() => {
      if (document.visibilityState !== "visible") return;
      const node = panelRef.current;
      if (!node || node.getClientRects().length === 0) return;
      if (Date.now() - diskFetchedAt.current < 10000) return;
      void remoteDisk(terminalId, path)
        .then((value) => {
          if (value.totalBytes > 0) {
            setDisk(value);
            diskFetchedAt.current = Date.now();
          }
        })
        .catch(() => setDisk(null));
    }, 10000);
    return () => window.clearInterval(timer);
  }, [status, terminalId, path]);

  // 上传下拉展开时：点空白处或按 Esc 收起，避免菜单一直挂在那里
  useEffect(() => {
    if (!uploadMenu) return undefined;
    const onPointerDown = (event: PointerEvent) => {
      if (uploadMenuRef.current?.contains(event.target as Node)) return;
      setUploadMenu(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setUploadMenu(false);
    };
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [uploadMenu]);

  /**
   * 包装一次远端写操作。
   *
   * 参数：action 为远端动作，successText 为成功回执，refresh 为是否需要重列
   * 核心流程：置忙清回执 -> 执行 -> 写回执 -> 需要时重列当前目录；失败只提示。
   */
  const runFileAction = async (
    action: () => Promise<void>,
    successText: string,
    refresh = true,
  ) => {
    setBusy(true);
    setNotice(null);
    try {
      await action();
      setNotice(successText);
      // 删除/建目录会改变占用，顺手让磁盘条重取一次
      setDiskTick((value) => value + 1);
      if (refresh && clientId) await loadDir(clientId, path);
    } catch (err) {
      setNotice(`操作失败：${errorMessage(err)}`);
    } finally {
      setBusy(false);
    }
  };

  /**
   * 回到上级目录。
   *
   * 参数：无
   * 核心流程：默认目录没有上级，因此到默认目录时按钮置灰。
   */
  const goParent = () => {
    if (clientId) void loadDir(clientId, parentPath(path));
  };

  /**
   * 按路径栏输入跳转。
   *
   * 参数：无（读 pathInput）
   * 核心流程：去掉首尾空白后直接列目录，空输入视为回到默认目录，
   * 失败由列表区报原因。
   */
  const jumpTo = () => {
    if (!clientId) return;
    const target = pathInput.trim();
    void loadDir(clientId, target === "" ? DEFAULT_DIR : target);
  };

  /**
   * 回到服务端默认目录。
   *
   * 参数：无
   * 核心流程：固定用 DEFAULT_DIR 重列，不能把默认目录格式化成 "/"，
   * 否则会去开根目录；成功后输入框回到空默认态，清掉残留的旧文本。
   */
  const backToDefault = () => {
    if (clientId) void loadDir(clientId, DEFAULT_DIR);
  };

  /**
   * 双击或点「打开」时进入目录 / 打开文件。
   *
   * 参数：entry 为列表条目
   * 核心流程：目录直接切过去；文件走内置编辑器读文本。
   */
  const openEntry = async (entry: SftpEntry) => {
    if (!clientId) return;
    if (entry.isDir) {
      void loadDir(clientId, entry.path);
      return;
    }
    setBusy(true);
    setNotice(null);
    try {
      const content = await sftpReadText(clientId, entry.path);
      setEditorError(null);
      setEditor({ path: entry.path, content, saved: content });
    } catch (err) {
      setNotice(`无法按文本打开 ${entry.name}：${errorMessage(err)}`);
    } finally {
      setBusy(false);
    }
  };

  /**
   * 提交新建目录 / 重命名。
   *
   * 参数：value 为输入的名称
   * 核心流程：按弹窗类型拼目标路径，成功后收起弹窗并重列目录。
   */
  const submitPrompt = async (value: string) => {
    if (!clientId || !prompt) return;
    const target = prompt.target;
    const to = joinPath(path, value);
    const action =
      prompt.kind === "mkdir"
        ? () => sftpMkdir(clientId, to)
        : () => sftpRename(clientId, target?.path ?? "", to);
    const successText = prompt.kind === "mkdir" ? `已新建目录 ${value}` : `已重命名为 ${value}`;
    setPrompt(null);
    await runFileAction(action, successText);
  };

  /**
   * 打开系统选择器并拿到路径。
   *
   * 参数：directory 为 true 表示选目录
   * 核心流程：统一加 5 秒兜底提示（xrdp 下 portal 可能挂起），
   * 用户取消返回 null，调用方直接结束本次操作。
   */
  const pickPath = async (directory: boolean): Promise<string | null> => {
    const timer = window.setTimeout(() => {
      setNotice("文件选择器未响应，可关掉重试或改用编辑远端已有文件");
    }, 5000);
    try {
      const result = await openDialog({ multiple: false, directory });
      window.clearTimeout(timer);
      return typeof result === "string" ? result : null;
    } catch (err) {
      window.clearTimeout(timer);
      setNotice(`无法打开文件选择器：${String(err)}`);
      return null;
    } finally {
      window.clearTimeout(timer);
    }
  };

  /** 扫描阶段（还没派发任何文件）是否被取消，取消按钮在准备阶段也要立刻有反应 */
  const scanCancelledRef = useRef(false);

  /** 清掉进度行的自动消失定时器 */
  const clearProgressTimer = useCallback(() => {
    if (progressTimerRef.current !== null) {
      window.clearTimeout(progressTimerRef.current);
      progressTimerRef.current = null;
    }
  }, []);

  /** 开始一宗新传输前，先把上一宗留下的定时器清掉 */
  const beginProgress = useCallback(() => {
    clearProgressTimer();
    scanCancelledRef.current = false;
    // 上一个账本已经过期：清掉，免得扫描阶段的「取消」打到旧令牌上
    ledgerRef.current = null;
  }, [clearProgressTimer]);

  /**
   * 把进度行留 20 秒再收起来。
   *
   * 参数：无
   * 核心流程：失败与取消都不会立刻消失，用户来得及看清原因并点「重试」；
   * 期间再点任何一次传输都会提前清掉。
   */
  const holdProgress = useCallback(() => {
    clearProgressTimer();
    progressTimerRef.current = window.setTimeout(() => {
      progressTimerRef.current = null;
      setTransfer(null);
    }, PROGRESS_HOLD_MS);
  }, [clearProgressTimer]);

  /**
   * 把账本刷成进度行状态。
   *
   * 参数：ledger 为当前账本，phase 为阶段，message 为附加说明
   * 核心流程：字节一律用账本里所有文件的已写字节累加，
   * 因此并发时数字只涨不跳；节流交给调用方按 lastPaint 判断。
   */
  const paintLedger = useCallback(
    (ledger: TransferLedger, phase: TransferPhase, message: string | null = null) => {
      ledger.lastPaint = Date.now();
      setTransfer({
        name: ledger.name,
        phase,
        done: ledger.done,
        total: ledger.total,
        bytes: sumWritten(ledger.written),
        totalBytes: ledger.totalBytes,
        message,
        retry: null,
      });
    },
    [],
  );

  /**
   * 用并发池把一批文件传完。
   *
   * 参数：direction 为方向，id 为会话标识，tasks 为按序派发的文件表，
   * seed 为重试时接着用的旧账（完成数、已写字节、要续传的路径）
   * 核心流程：建账本 -> 挂到 ledgerRef（取消按钮靠它拿在途令牌）->
   * 交给 runPool 并发跑（上限 3）；单个文件起一个 TransferHandle，
   * 进度事件按「续传起点 + 本次已传」写回账本；任何文件失败都由池记住并
   * 停止派发，等在途结束后把「剩下没传完的文件」交回调用方。
   */
  const runBatch = useCallback(
    async (
      direction: TransferDirection,
      id: string,
      tasks: FileTask[],
      seed?: {
        done: number;
        total: number;
        totalBytes: number;
        written: [string, number][];
        resume: string[];
      },
    ): Promise<BatchResult> => {
      /** 需要续传的远端路径：这些文件旁边已经留下了 .rmxpart */
      const resumePaths = new Set(seed?.resume ?? []);
      const ledger: TransferLedger = {
        direction,
        id,
        total: seed?.total ?? tasks.length,
        totalBytes:
          seed?.totalBytes ?? tasks.reduce((sum, task) => sum + Math.max(0, task.size), 0),
        written: new Map(seed?.written ?? []),
        done: seed?.done ?? 0,
        name: tasks[0]?.name ?? "",
        cancelling: false,
        tokens: new Set<string>(),
        lastPaint: 0,
      };
      ledgerRef.current = ledger;
      paintLedger(ledger, "running");
      /** 已完成任务的远端路径，用来算「还剩哪些没传」 */
      const completed = new Set<string>();
      // 用对象装着第一个失败，避免闭包里给 let 赋值时类型被收窄成 null
      const failed: { value: { task: FileTask; message: string } | null } = { value: null };

      /** 传一个文件：起句柄、把进度写回账本、结束时记完成或失败 */
      const transferOne = async (task: FileTask) => {
        /** 上一次尝试留下的字节数（续传起点），只用来兜住数字不回退 */
        const startAt = ledger.written.get(task.remotePath) ?? 0;
        const onEvent = (event: TransferEvent) => {
          if (event.kind === "error") return;
          // 后端事件里的 bytes 是「本次起点 + 本次已写」的累计值，续传时已含
          // .rmxpart 里那截，直接落账即可；再叠加 startAt 会把同一段字节数两遍
          // （60 MiB 的文件续传完显示成 78.3 MiB）。startAt 只用于「没续传、
          // 从头重传」时兜底，保证进度数字不往回跳。
          const bytes = Math.max(0, event.bytes);
          ledger.written.set(task.remotePath, Math.max(startAt, bytes));
          ledger.name = task.name;
          // 后端回调很密，按最小间隔节流；收尾处还会再刷一次，最后一帧不会丢
          if (Date.now() - ledger.lastPaint < PROGRESS_PAINT_MS) return;
          paintLedger(ledger, ledger.cancelling ? "cancelling" : "running");
        };
        const options = { resume: resumePaths.has(task.remotePath), onEvent };
        const handle: TransferHandle =
          direction === "upload"
            ? sftpUploadLocalFile(ledger.id, task.remotePath, task.localPath, options)
            : sftpDownloadRemoteFile(ledger.id, task.remotePath, task.localPath, options);
        ledger.tokens.add(handle.cancelToken);
        ledger.name = task.name;
        paintLedger(ledger, ledger.cancelling ? "cancelling" : "running");
        // 取消恰好发生在派发与登记之间：这个任务刚起来就补一刀，别让它白跑
        if (ledger.cancelling) void sftpCancelTransfer(handle.cancelToken).catch(() => {});
        try {
          const outcome = await handle.done;
          // 收尾一律落到「最终文件大小」：有 total 就以它为准，否则用「续传起点 +
          // 本次已写」。这样同一份文件在多次尝试里只记一份大小，不会叠加。
          ledger.written.set(
            task.remotePath,
            outcome.total > 0 ? outcome.total : outcome.resumedFrom + outcome.written,
          );
          completed.add(task.remotePath);
          ledger.done += 1;
          paintLedger(ledger, ledger.cancelling ? "cancelling" : "running");
        } catch (err) {
          if (failed.value === null) failed.value = { task, message: errorMessage(err) };
          // 抛给并发池：它据此停止派发新文件
          throw err;
        } finally {
          ledger.tokens.delete(handle.cancelToken);
        }
      };

      await runPool(tasks, TRANSFER_CONCURRENCY, transferOne, {
        shouldStop: () => ledger.cancelling,
      });

      return {
        ledger,
        remaining: tasks.filter((task) => !completed.has(task.remotePath)),
        failure: failed.value,
        cancelled: ledger.cancelling,
      };
    },
    [paintLedger],
  );

  /**
   * 收尾：成功清空进度行写回执，失败或取消改成可重试的状态。
   *
   * 参数：result 为批次结果，targetDir 为远端目录（上传成功要重列），
   * refresh 表示是否顺便刷新远端列表与磁盘占用
   * 核心流程：失败时把「哪个文件失败 + 已成功几个」写进进度行，
   * 并把剩下的文件与已写字节存成重试上下文；取消时改成「已取消（已传 X）」，
   * 两种情况都给「重试」「继续传输」用，且保留 20 秒。
   */
  const settleBatch = useCallback(
    async (result: BatchResult, targetDir: string, refresh: boolean) => {
      const { ledger } = result;
      // 账本里成功文件的落账值是「最终文件大小」（见 transferOne 收尾），未完成的
      // 才是「已传部分」，所以这个和既能当成功回执的总量，也能当取消时的已传量。
      const bytes = sumWritten(ledger.written);
      if (refresh) {
        setDiskTick((value) => value + 1);
        await loadDir(ledger.id, targetDir);
      }
      // 还有文件没传完，或者有文件报错 -> 留在进度行上给「重试」「继续传输」
      if (result.remaining.length > 0 || result.failure) {
        const retry: TransferRetry | null =
          result.remaining.length > 0
            ? {
                direction: ledger.direction,
                id: ledger.id,
                tasks: result.remaining,
                // 真正写进去过字节的文件才续传，其余照旧从头传
                resume: result.remaining
                  .filter((task) => (ledger.written.get(task.remotePath) ?? 0) > 0)
                  .map((task) => task.remotePath),
                done: ledger.done,
                total: ledger.total,
                totalBytes: ledger.totalBytes,
                written: Array.from(ledger.written.entries()),
              }
            : null;
        const message = result.cancelled
          ? `已取消（已传 ${formatSize(bytes)}）`
          : `${result.failure?.task.name ?? "传输"} 失败：${
              result.failure?.message ?? "未知原因"
            }（已成功 ${ledger.done} 个文件）`;
        setTransfer({
          name: ledger.name,
          phase: result.cancelled ? "cancelled" : "error",
          done: ledger.done,
          total: ledger.total,
          bytes,
          totalBytes: ledger.totalBytes,
          message,
          retry,
        });
        holdProgress();
        return;
      }
      // 全部传完（含「取消来得太晚，其实已经传完」）：按成功收尾
      setTransfer(null);
      const verb = ledger.direction === "upload" ? "已上传" : "已下载";
      setNotice(
        ledger.total <= 1
          ? `${verb} ${ledger.name}，共 ${formatSize(bytes)}`
          : `${verb} ${ledger.done} 个文件，共 ${formatSize(bytes)}`,
      );
    },
    [holdProgress, loadDir],
  );

  /**
   * 上传一批本地路径（文件级最多 3 个并发）。
   *
   * 参数：paths 为本机绝对路径列表（按钮选的，或拖放进来的），targetDir 为远端目标目录
   * 核心流程：先扫一遍：目录建同名远端目录（已存在当成功），文件收集成任务表，
   * 顺便把文件数与总字节算出来，进度条一出现就有分母；
   * 再交给并发池跑这些文件；任何文件失败都停止派发、等在途结束，
   * 然后报告失败文件与已成功数量，剩下没传的留在进度行上等「重试」。
   */
  const uploadPaths = useCallback(
    async (paths: string[], targetDir: string) => {
      if (!clientId || paths.length === 0) return;
      // 同一时刻只允许一宗传输：按钮会被禁用，拖放这条路径要自己挡一下
      if (busyRef.current) {
        setNotice("还有传输在进行，等它结束后再拖入");
        return;
      }
      const id = clientId;
      busyRef.current = true;
      setBusy(true);
      setNotice(null);
      beginProgress();
      // 扫描阶段还没有分母，先给一条不确定态的进度行
      setTransfer({
        name: localBaseName(paths[0]),
        phase: "running",
        done: 0,
        total: 0,
        bytes: 0,
        totalBytes: 0,
        message: "正在整理待上传的文件…",
        retry: null,
      });
      const tasks: FileTask[] = [];
      /** 扫描阶段的刷新时间戳：大目录几千个文件，按 100ms 节流免得把界面拖卡 */
      let scanPaintedAt = 0;

      /** 扫描进度刷新：只在节流窗口外写一次 state，文案里带已找到的个数 */
      const paintScan = (name: string) => {
        if (Date.now() - scanPaintedAt < 100) return;
        scanPaintedAt = Date.now();
        setTransfer({
          name,
          phase: "running",
          done: 0,
          total: 0,
          bytes: 0,
          totalBytes: 0,
          message: `正在整理待上传的文件（已找到 ${tasks.length} 个）`,
          retry: null,
        });
      };

      try {
        /**
         * 递归扫描一个本地路径：目录先建远端同名目录，文件收进任务表。
         *
         * 参数：localPath 为本机路径，remoteDir 为远端父目录，
         * known 为父目录列表里已经拿到的那一项（名字、类型、大小都有，
         * 省掉对每个文件再探一次的开销）
         */
        const walk = async (localPath: string, remoteDir: string, known?: LocalEntry) => {
          if (scanCancelledRef.current) return;
          const name = known?.name ?? localBaseName(localPath);
          const remoteTarget = joinPath(remoteDir, name);
          if (known && !known.isDir) {
            tasks.push({ name, remotePath: remoteTarget, localPath, size: known.size });
            paintScan(name);
            return;
          }
          /** 目录的子项；顶层路径还没有父级信息，得先探一次 */
          const children = known?.isDir
            ? await listLocalDir(localPath)
            : await (async () => {
                const probe = await probeLocalPath(localPath);
                if (!probe.isDir) {
                  tasks.push({ name, remotePath: remoteTarget, localPath, size: probe.size });
                  paintScan(name);
                  return null;
                }
                return probe.children;
              })();
          if (children === null) return;
          try {
            await sftpMkdir(id, remoteTarget);
          } catch {
            // 目录已存在时 mkdir 会报错，不当作失败：真正的判据是后面的文件写入
          }
          for (const child of children) await walk(child.path, remoteTarget, child);
        };
        for (const localPath of paths) {
          if (scanCancelledRef.current) break;
          await walk(localPath, targetDir);
        }
      } catch (err) {
        setTransfer({
          name: localBaseName(paths[0]),
          phase: "error",
          done: 0,
          total: 0,
          bytes: 0,
          totalBytes: 0,
          message: `整理待上传文件失败：${errorMessage(err)}`,
          retry: null,
        });
        holdProgress();
        busyRef.current = false;
        setBusy(false);
        return;
      }
      if (scanCancelledRef.current) {
        setTransfer({
          name: localBaseName(paths[0]),
          phase: "cancelled",
          done: 0,
          total: 0,
          bytes: 0,
          totalBytes: 0,
          message: `已取消（已传 ${formatSize(0)}）`,
          retry: null,
        });
        holdProgress();
        busyRef.current = false;
        setBusy(false);
        return;
      }
      if (tasks.length === 0) {
        setTransfer(null);
        setNotice("这些路径里没有可上传的文件");
        busyRef.current = false;
        setBusy(false);
        return;
      }
      try {
        const result = await runBatch("upload", id, tasks);
        await settleBatch(result, targetDir, true);
      } finally {
        busyRef.current = false;
        setBusy(false);
      }
    },
    [beginProgress, clientId, holdProgress, loadDir, runBatch, settleBatch],
  );

  /**
   * 取消当前传输。
   *
   * 参数：无
   * 核心流程：点下去立刻把进度行改成「取消中…」并禁用按钮（不等后端回话），
   * 然后给每个在途任务发一次 sftp_cancel_transfer；排队中的文件由并发池的
   * shouldStop 挡住不再派发；扫描阶段的取消用 scanCancelledRef 停机。
   */
  const cancelTransfer = () => {
    scanCancelledRef.current = true;
    const ledger = ledgerRef.current;
    if (!ledger || ledger.cancelling) {
      // 还没有派发任何文件（正在扫描）：直接按已取消收尾
      if (ledger === null) {
        setTransfer((prev) =>
          prev && prev.phase === "running"
            ? {
                ...prev,
                phase: "cancelled",
                message: `已取消（已传 ${formatSize(prev.bytes)}）`,
                retry: null,
              }
            : prev,
        );
        holdProgress();
      }
      return;
    }
    ledger.cancelling = true;
    paintLedger(ledger, "cancelling");
    for (const token of Array.from(ledger.tokens)) {
      void sftpCancelTransfer(token).catch(() => {});
    }
  };

  /**
   * 重试 / 继续传输：按失败时留下的上下文重发剩下的文件。
   *
   * 参数：无
   * 核心流程：把上次的完成数、已写字节带回去，进度条不从零开始；
   * 已经写过字节的文件用 resume=true 接着 .rmxpart 传，
   * 其余文件照旧从头传，因此重试不会把整个目录再传一遍。
   */
  const retryTransfer = async () => {
    const retry = transfer?.retry ?? null;
    if (!retry || busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setNotice(null);
    beginProgress();
    try {
      const result = await runBatch(retry.direction, retry.id, retry.tasks, {
        done: retry.done,
        total: retry.total,
        totalBytes: retry.totalBytes,
        written: retry.written,
        resume: retry.resume,
      });
      await settleBatch(result, path, retry.direction === "upload");
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  };

  /** 拖放与按钮共用同一份上传实现，靠 ref 拿到本次渲染的版本，订阅只做一次 */
  const uploadPathsRef = useRef<(paths: string[]) => void>(() => {});
  useEffect(() => {
    uploadPathsRef.current = (paths) => void uploadPaths(paths, path);
  });

  /**
   * 原生拖放上传：拖入本机文件或目录就上传到当前远端目录。
   *
   * 参数：无
   * 核心流程：面板连接就绪后订阅 Tauri 的 onDragDropEvent（原生事件能拿到真实
   * 本地路径，HTML5 的 File 对象在 Tauri 里拿不到路径）；
   * 拖入过程中用 elementFromPoint 判断鼠标是否真的落在面板上，是就亮起覆盖层，
   * 松手才真正上传，离开或取消立刻清掉提示；
   * 原生 API 不存在或不可用（例如在浏览器里跑 vite dev）时退回 HTML5
   * dragover/drop，并把降级原因记在 dropFallback 里显示给用户；
   * 组件卸载或连接状态变化时取消订阅，避免多个分页重复触发。
   */
  useEffect(() => {
    setDropCount(0);
    if (status !== "ready") return undefined;
    let disposed = false;
    let unlisten: UnlistenFn | null = null;
    let detachFallback: (() => void) | null = null;
    /** 本次拖入的路径，原生事件只在 enter 给一次，over 时复用 */
    let dragged: string[] = [];

    /** 面板是否真的可见：切到别的分页时面板只是被 CSS 隐藏 */
    const panelVisible = () => (panelRef.current?.getClientRects().length ?? 0) > 0;

    /**
     * 物理像素坐标是否落在面板内。
     *
     * 参数：position 为 Tauri 给的光标位置（物理像素，相对 webview 左上角）
     * 核心流程：按 devicePixelRatio 换成 CSS 像素后用 elementFromPoint 命中测试，
     * 面板收起或不可见时直接判否。
     */
    const overPanel = (position: { x: number; y: number }) => {
      const node = panelRef.current;
      if (!node || !panelVisible()) return false;
      const scale = window.devicePixelRatio || 1;
      const hit = document.elementFromPoint(position.x / scale, position.y / scale);
      return !!hit && node.contains(hit);
    };

    const clearDrop = () => {
      dragged = [];
      setDropCount(0);
    };

    /** HTML5 兜底：没有原生事件时至少在面板上能接住拖放 */
    const attachFallback = (reason: string) => {
      const node = panelRef.current;
      setDropFallback(reason);
      if (!node || disposed) return;
      const onDragover = (event: DragEvent) => {
        event.preventDefault();
        if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
        setDropCount(Math.max(1, event.dataTransfer?.items.length ?? 1));
      };
      const onDragleave = () => setDropCount(0);
      const onDrop = (event: DragEvent) => {
        event.preventDefault();
        setDropCount(0);
        // HTML5 只给 File 对象，能否拿到真实路径取决于宿主环境
        const paths = Array.from(event.dataTransfer?.files ?? [])
          .map((file) => (file as File & { path?: string }).path)
          .filter((value): value is string => typeof value === "string" && value !== "");
        if (paths.length === 0) {
          setNotice(`拖放不可用（${reason}）：宿主没提供本地路径，请用「上传」按钮选文件`);
          return;
        }
        uploadPathsRef.current(paths);
      };
      node.addEventListener("dragover", onDragover);
      node.addEventListener("dragleave", onDragleave);
      node.addEventListener("drop", onDrop);
      detachFallback = () => {
        node.removeEventListener("dragover", onDragover);
        node.removeEventListener("dragleave", onDragleave);
        node.removeEventListener("drop", onDrop);
      };
    };

    const attach = async () => {
      try {
        const webview = getCurrentWebview();
        if (typeof webview.onDragDropEvent !== "function") {
          attachFallback("当前 Tauri API 版本没有 onDragDropEvent");
          return;
        }
        const stop = await webview.onDragDropEvent((event) => {
          if (disposed) return;
          const payload = event.payload;
          if (payload.type === "leave") {
            clearDrop();
            return;
          }
          if (payload.type === "enter") {
            dragged = payload.paths;
            setDropCount(overPanel(payload.position) ? dragged.length : 0);
            return;
          }
          if (payload.type === "over") {
            setDropCount(overPanel(payload.position) ? dragged.length : 0);
            return;
          }
          // drop：只有松手落在面板上才接管，落在终端等其他区域一律不管
          const accepted = overPanel(payload.position);
          const dropped = payload.paths;
          clearDrop();
          if (accepted && dropped.length > 0) uploadPathsRef.current(dropped);
        });
        if (disposed) {
          stop();
          return;
        }
        unlisten = stop;
      } catch (err) {
        attachFallback(errorMessage(err));
      }
    };
    void attach();

    return () => {
      disposed = true;
      if (unlisten) unlisten();
      detachFallback?.();
    };
    // path 通过 uploadPathsRef 取最新值，这里只跟连接状态走，避免重复订阅
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status]);

  /**
   * 上传单个本地文件到当前目录。
   *
   * 参数：无
   * 核心流程：选择框选文件 -> 和整目录上传走同一条实现（探测大小 -> 并发池 ->
   * 进度行可取消），因此单文件也有进度条与取消；完成后重列目录。
   */
  const upload = async () => {
    if (!clientId) return;
    setNotice(null);
    const picked = await pickPath(false);
    if (!picked) return;
    await uploadPaths([picked], path);
  };

  /**
   * 上传整个本地目录（递归）。
   *
   * 参数：无
   * 核心流程：选目录 -> 递归遍历本机目录（listLocalDir）-> 文件进并发池上传，
   * 进度行显示总量与完成数，可随时取消；失败即停并报出是哪个文件。
   */
  const uploadFolder = async () => {
    if (!clientId) return;
    setNotice(null);
    const picked = await pickPath(true);
    if (!picked) return;
    await uploadPaths([picked], path);
  };

  /**
   * 下载远端条目到本地（文件级最多 3 个并发）。
   *
   * 参数：entry 为远端条目
   * 核心流程：文件走保存对话框，变成单个任务；目录走选父目录 + 递归列远端文件表，
   * 大小直接取目录列表里的 size，进度条一出现就有分母；
   * 两条路最后都交给同一个并发池，因此进度、取消、失败续传的行为完全一致。
   */
  const download = async (entry: SftpEntry) => {
    if (!clientId) return;
    if (busyRef.current) {
      setNotice("还有传输在进行，等它结束后再下载");
      return;
    }
    const id = clientId;
    setNotice(null);

    /** 单文件：先问落盘位置，取消对话框就什么都不做 */
    if (!entry.isDir) {
      const picked = await saveDialog({ defaultPath: entry.name });
      if (typeof picked !== "string") return;
      // 用户已经写对了扩展名就不再追加，否则 `/tmp/dl-big60.bin` 会变成 `.bin.bin`
      const target = withRemoteExtension(picked, entry.name);
      busyRef.current = true;
      setBusy(true);
      beginProgress();
      try {
        const result = await runBatch("download", id, [
          { name: entry.name, remotePath: entry.path, localPath: target, size: entry.size },
        ]);
        await settleBatch(result, path, false);
      } finally {
        busyRef.current = false;
        setBusy(false);
      }
      return;
    }

    const parent = await pickPath(true);
    if (!parent) return;
    busyRef.current = true;
    setBusy(true);
    beginProgress();
    setTransfer({
      name: entry.name,
      phase: "running",
      done: 0,
      total: 0,
      bytes: 0,
      totalBytes: 0,
      message: "正在整理待下载的文件…",
      retry: null,
    });
    const tasks: FileTask[] = [];
    /** 扫描阶段的刷新时间戳：远端大目录按 100ms 节流 */
    let scanPaintedAt = 0;
    try {
      /** 递归列出远端目录，把每个文件收进任务表 */
      const walk = async (remotePath: string, localPath: string) => {
        if (scanCancelledRef.current) return;
        const list = await sftpList(id, remotePath);
        for (const item of list) {
          if (scanCancelledRef.current) return;
          const localTarget = `${localPath}/${item.name}`;
          if (item.isDir) {
            // 空目录不会在本机创建（写文件时自动建父目录），有文件就会被建出来
            await walk(item.path, localTarget);
          } else {
            tasks.push({
              name: item.name,
              remotePath: item.path,
              localPath: localTarget,
              size: item.size,
            });
            if (Date.now() - scanPaintedAt >= 100) {
              scanPaintedAt = Date.now();
              setTransfer({
                name: item.name,
                phase: "running",
                done: 0,
                total: 0,
                bytes: 0,
                totalBytes: 0,
                message: `正在整理待下载的文件（已找到 ${tasks.length} 个）`,
                retry: null,
              });
            }
          }
        }
      };
      await walk(entry.path, parent);
    } catch (err) {
      setTransfer({
        name: entry.name,
        phase: "error",
        done: 0,
        total: 0,
        bytes: 0,
        totalBytes: 0,
        message: `整理待下载文件失败：${errorMessage(err)}`,
        retry: null,
      });
      holdProgress();
      busyRef.current = false;
      setBusy(false);
      return;
    }
    if (scanCancelledRef.current) {
      setTransfer({
        name: entry.name,
        phase: "cancelled",
        done: 0,
        total: 0,
        bytes: 0,
        totalBytes: 0,
        message: `已取消（已传 ${formatSize(0)}）`,
        retry: null,
      });
      holdProgress();
      busyRef.current = false;
      setBusy(false);
      return;
    }
    if (tasks.length === 0) {
      setTransfer(null);
      setNotice("这个远端目录里没有可下载的文件");
      busyRef.current = false;
      setBusy(false);
      return;
    }
    try {
      const result = await runBatch("download", id, tasks);
      await settleBatch(result, path, false);
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  };

  /**
   * 保存编辑器内容。
   *
   * 参数：无
   * 核心流程：先卡 2 MiB（编辑器是整段文本场景）-> sftp_write_text 整段覆盖 ->
   * 记为已保存并重列目录；超限提示只出现在编辑器里，不影响上传下载。
   */
  const saveEditor = async () => {
    if (!clientId || !editor) return;
    const length = utf8Length(editor.content);
    if (length > SFTP_WRITE_LIMIT_BYTES) {
      setEditorError(`内容 ${formatSize(length)} 超过 2 MiB 上限，未保存`);
      return;
    }
    setEditorBusy(true);
    setEditorError(null);
    try {
      await sftpWriteText(clientId, editor.path, editor.content);
      setEditor({ ...editor, saved: editor.content });
      setNotice(`已保存 ${editor.path}`);
      await loadDir(clientId, path);
    } catch (err) {
      setEditorError(`保存失败：${errorMessage(err)}`);
    } finally {
      setEditorBusy(false);
    }
  };

  /** 有未保存修改时在编辑器底部提醒 */
  const dirty = editor !== null && editor.content !== editor.saved;

  /**
   * 渲染一行文件的操作按钮。
   *
   * 参数：entry 为该行的远端条目
   * 核心流程：宽布局直接放在行尾单元格里，窄布局由「⋯」展开后在下一行整行铺开；
   * 两处共用同一份按钮，避免两套逻辑跑偏。
   */
  const renderRowActions = (entry: SftpEntry) => (
    <>
      <button type="button" onClick={() => void openEntry(entry)}>
        打开
      </button>
      {!entry.isDir && (
        <button
          type="button"
          disabled={busy}
          onClick={() => void download(entry)}
          title="远端 → 本机：流式下载这个文件，不限大小"
        >
          下载
        </button>
      )}
      <button
        type="button"
        disabled={busy}
        onClick={() =>
          setPrompt({
            kind: "rename",
            title: "重命名",
            label: "新名称",
            initial: entry.name,
            target: entry,
          })
        }
      >
        重命名
      </button>
      <button type="button" className="danger" disabled={busy} onClick={() => setRemoving(entry)}>
        删除
      </button>
    </>
  );

  /**
   * 远端标识文案。
   *
   * 参数：无
   * 核心流程：用户名@主机:当前远端路径，默认目录显示 ~，
   * 主机缺失时写「远端」，让用户一眼看出操作的是远端机器而不是本机磁盘。
   */
  const remoteHostLabel = session.host ?? "远端";
  const remotePathLabel = path === DEFAULT_DIR || path === "" ? "~" : path;
  const remoteTargetLabel = `${session.username ? `${session.username}@` : ""}${remoteHostLabel}:${remotePathLabel}`;
  /** 顶部那行走不下时中间省略，完整值仍放在 title 里 */
  const remoteTargetShown = middleEllipsis(remoteTargetLabel, narrow ? 26 : 46);
  /** 磁盘水位与文案：没有数据时整行不渲染，这里只是算好备用 */
  const diskTone = disk ? diskLevel(disk.percent) : "normal";
  const diskPercent = disk ? Math.round(disk.percent) : 0;
  const diskText = disk
    ? `${formatSize(disk.usedBytes)} / ${formatSize(disk.totalBytes)} · ${diskPercent}%`
    : "";
  /** 总量已知才画确定进度：未知时进度条走不确定态，也绝不显示「0%」 */
  const transferKnown = transfer !== null && transfer.totalBytes > 0;
  const transferRatio =
    transfer && transferKnown ? Math.min(1, Math.max(0, transfer.bytes / transfer.totalBytes)) : 0;
  const transferPercent = Math.round(transferRatio * 100);
  const transferMeta = !transfer
    ? ""
    : transferKnown
      ? `已传 ${formatSize(transfer.bytes)} / ${formatSize(transfer.totalBytes)} · ${transferPercent}%`
      : `已传 ${formatSize(transfer.bytes)} · 总量未知`;
  /** 只有目录传输才需要「第 i/n 个文件」；按已完成数往后推，并发时数字也单调 */
  const transferBatch = transfer !== null && transfer.total > 1;
  const transferIndex = transfer ? Math.min(transfer.done + 1, transfer.total) : 0;

  return (
    <section className="sftp-panel" ref={panelRef}>
      {/* 常驻一行：本面板列出的始终是远端机器上的目录 */}
      <div className="sftp-remote" title={`远端目录 ${remoteTargetLabel}`}>
        <span className="sftp-remote-tag">远端</span>
        <span className="sftp-remote-target">{remoteTargetShown}</span>
      </div>
      <header className="sftp-toolbar">
        <button
          type="button"
          onClick={goParent}
          disabled={status !== "ready" || path === "" || path === DEFAULT_DIR}
          title="返回上级目录（默认目录没有上级）"
        >
          ↑
        </button>
        <input
          className="sftp-path"
          value={pathInput}
          placeholder="~（默认目录，可输入绝对路径后回车）"
          title="当前远端路径，改成绝对路径后回车可跳转"
          disabled={status !== "ready"}
          onChange={(event) => setPathInput(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter") jumpTo();
          }}
        />
        <button
          type="button"
          onClick={() => {
            if (clientId) void loadDir(clientId, path);
          }}
          disabled={status !== "ready" || loading}
          title="重新读取当前远端目录"
        >
          刷新
        </button>
        <button
          type="button"
          onClick={() =>
            setPrompt({
              kind: "mkdir",
              title: "新建目录",
              label: "目录名称",
              initial: "",
              target: null,
            })
          }
          disabled={status !== "ready" || busy}
          title="在当前远端目录下新建一个目录"
        >
          新建目录
        </button>
        {/* 上传合成一个按钮 + 下拉：主按钮传文件，下拉里再选整目录，工具栏不挤成两行 */}
        <div className="sftp-upload" ref={uploadMenuRef}>
          <button
            type="button"
            className="sftp-upload-main"
            onClick={() => void upload()}
            disabled={status !== "ready" || busy}
            title="本机 → 远端：流式上传一个本机文件，不限大小"
          >
            上传
          </button>
          <button
            type="button"
            className="sftp-upload-caret"
            aria-haspopup="menu"
            aria-expanded={uploadMenu}
            disabled={status !== "ready" || busy}
            onClick={() => setUploadMenu((prev) => !prev)}
            title="更多上传方式（整个目录 / 文件）"
          >
            ▾
          </button>
          {uploadMenu && (
            <div className="sftp-menu" role="menu">
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setUploadMenu(false);
                  void upload();
                }}
              >
                上传文件…
              </button>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setUploadMenu(false);
                  void uploadFolder();
                }}
              >
                上传整个目录…
              </button>
            </div>
          )}
        </div>
        {/* 跟随开关：与工具栏其它控件同高，靠右收尾；窄侧栏下只留短文字，
            完整说明始终在 title / aria-label 里 */}
        <label className="sftp-follow" title="文件分页跟随终端当前工作目录">
          <input
            type="checkbox"
            checked={followCwd}
            disabled={!onFollowCwdChange}
            aria-label="文件分页跟随终端当前工作目录"
            onChange={(event) => onFollowCwdChange?.(event.target.checked)}
          />
          <span>{narrow ? "跟随" : "跟随终端"}</span>
        </label>
      </header>

      {/* 磁盘占用：拿不到终端或命令失败时这行整体不渲染，不显示猜的数字 */}
      {status === "ready" && disk && (
        <div
          className={`sftp-disk is-${diskTone}`}
          title={`${disk.mount} 已用 ${formatSize(disk.usedBytes)}，可用 ${formatSize(disk.freeBytes)}，共 ${formatSize(disk.totalBytes)}`}
          aria-label={`远端磁盘 ${diskText}，${diskLevelText(diskTone)}`}
        >
          <span className="sftp-disk-text">{diskText}</span>
          <span className="sftp-disk-track">
            <span
              className="sftp-disk-fill"
              style={{ width: `${Math.min(100, Math.max(0, disk.percent))}%` }}
            />
          </span>
          <span className="sftp-disk-level">{diskLevelText(diskTone)}</span>
        </div>
      )}

      {/* 进度行独占一行：文件名 + 进度条 + 已传/总量 + 取消，窄宽时整体换行也不挤工具栏 */}
      {transfer && (
        <div
          className={`sftp-progress is-${transfer.phase}${transferKnown ? "" : " is-unknown"}`}
          role="status"
          aria-live="polite"
          title={`${transfer.name} ${transferMeta}${transfer.message ? ` ${transfer.message}` : ""}`}
        >
          <div className="sftp-progress-line">
            <span className="sftp-progress-name">{transfer.name}</span>
            <span className="sftp-progress-track" aria-hidden="true">
              <span
                className="sftp-progress-fill"
                style={transferKnown ? { width: `${transferPercent}%` } : undefined}
              />
            </span>
            <span className="sftp-progress-meta">{transferMeta}</span>
            {transfer.phase === "running" && (
              <button
                type="button"
                className="sftp-progress-action"
                onClick={cancelTransfer}
                title="停止这次传输；已传的部分留在 .rmxpart 里，可以接着传"
              >
                取消
              </button>
            )}
            {transfer.phase === "cancelling" && (
              <button
                type="button"
                className="sftp-progress-action"
                disabled
                title="已通知后端停止，等它回话"
              >
                取消中…
              </button>
            )}
            {transfer.phase === "cancelled" && transfer.retry && (
              <button
                type="button"
                className="sftp-progress-action"
                onClick={() => void retryTransfer()}
                title="从断点接着传剩下的文件"
              >
                继续传输
              </button>
            )}
            {transfer.phase === "error" && transfer.retry && (
              <button
                type="button"
                className="sftp-progress-action"
                onClick={() => void retryTransfer()}
                title="重试失败的文件，已传的部分会续传"
              >
                重试
              </button>
            )}
          </div>
          {transferBatch && (
            <p className="sftp-progress-sub">
              完成 {transfer.done}/{transfer.total} · 第 {transferIndex}/{transfer.total} 个文件
            </p>
          )}
          {transfer.message && <p className="sftp-progress-sub is-note">{transfer.message}</p>}
        </div>
      )}

      {/* 原生拖放不可用时只在这一行说明原因，正常环境不占位置 */}
      {status === "ready" && dropFallback && (
        <p className="sftp-note">拖放降级为浏览器事件：{dropFallback}</p>
      )}

      {/* 拖入过程中的覆盖层：说清松手会落到哪个远端目录 */}
      {dropCount > 0 && (
        <div className="sftp-drop" role="status">
          <span className="sftp-drop-title">松手上传到</span>
          <span className="sftp-drop-path">{remoteTargetLabel}</span>
          <span className="sftp-drop-hint">{dropCount} 个本机路径（目录会递归上传）</span>
        </div>
      )}

      {status === "connecting" && (
        <p className="sftp-hint">正在连接远端主机 {remoteHostLabel} 的 SFTP…</p>
      )}
      {status === "error" && (
        <p className="sftp-error">连接远端主机 {remoteHostLabel} 失败：{connectError}</p>
      )}

      {status === "ready" && (
        <div className="sftp-body">
          {notice && <p className="sftp-notice">{notice}</p>}
          {loading ? (
            <p className="sftp-hint">正在读取目录…</p>
          ) : listError ? (
            <div className="sftp-error">
              <span>读取目录失败：{listError}</span>
              <span>
                可直接输入绝对路径，例如 /home/{session.username || "用户名"} 后回车
              </span>
              <button type="button" className="sftp-error-action" onClick={backToDefault}>
                回到默认目录
              </button>
            </div>
          ) : entries.length === 0 ? (
            <p className="sftp-hint">远端这个目录是空的</p>
          ) : (
            <table className={`sftp-table ${narrow ? "is-narrow" : ""}`}>
              <thead>
                <tr>
                  <th>名称</th>
                  {!narrow && <th className="sftp-size">大小</th>}
                  {!narrow && <th className="sftp-time">修改时间</th>}
                  <th className="sftp-actions" />
                </tr>
              </thead>
              <tbody>
                {entries.map((entry) => (
                  <Fragment key={entry.path}>
                    <tr
                      className={actionRow === entry.path ? "is-open" : ""}
                      onDoubleClick={() => void openEntry(entry)}
                    >
                      <td className="sftp-name" title={entry.name}>
                        {entry.isDir ? `${entry.name}/` : entry.name}
                      </td>
                      {!narrow && (
                        <td className="sftp-size">{entry.isDir ? "-" : formatSize(entry.size)}</td>
                      )}
                      {!narrow && <td className="sftp-time">{formatTime(entry.modified)}</td>}
                      {/* 行操作按钮上双击不该再触发整行的双击动作 */}
                      <td className="sftp-actions" onDoubleClick={(event) => event.stopPropagation()}>
                        {narrow ? (
                          // 窄容器：四个按钮收进「⋯」，选中行下方展开一行按钮，彻底避免横向滚动
                          <button
                            type="button"
                            className="sftp-more"
                            aria-expanded={actionRow === entry.path}
                            title="更多操作"
                            onClick={() =>
                              setActionRow((prev) => (prev === entry.path ? null : entry.path))
                            }
                          >
                            ⋯
                          </button>
                        ) : (
                          renderRowActions(entry)
                        )}
                      </td>
                    </tr>
                    {narrow && actionRow === entry.path && (
                      <tr className="sftp-action-row">
                        <td className="sftp-action-cell" colSpan={2}>
                          <div className="sftp-action-list">{renderRowActions(entry)}</div>
                        </td>
                      </tr>
                    )}
                  </Fragment>
                ))}
              </tbody>
            </table>
          )}
          {/* 说明压成一行：能力细节都挪到了对应按钮的 title 里 */}
          <p className="sftp-hint">远端目录 · 双击进入，双击文件编辑 · 可直接拖入本机文件</p>
        </div>
      )}

      {prompt && (
        <NameDialog
          title={prompt.title}
          label={prompt.label}
          initial={prompt.initial}
          busy={busy}
          onSubmit={(value) => void submitPrompt(value)}
          onCancel={() => setPrompt(null)}
        />
      )}

      {removing && (
        <ConfirmDialog
          message={`确认删除${removing.isDir ? "空目录" : "文件"}「${removing.name}」？删除后无法恢复。`}
          onConfirm={() => {
            const target = removing;
            setRemoving(null);
            if (!clientId) return;
            void runFileAction(
              () => sftpRemove(clientId, target.path, target.isDir),
              `已删除 ${target.name}`,
            );
          }}
          onCancel={() => setRemoving(null)}
        />
      )}

      {editor && (
        <div className="modal-backdrop" role="presentation">
          <div className="modal modal-wide" role="dialog" aria-label="内置文本编辑器">
            <header className="dialog-header">
              <h2 className="sftp-editor-title" title={editor.path}>
                {editor.path}
              </h2>
              <button
                type="button"
                className="dialog-close"
                onClick={closeEditor}
                disabled={editorBusy}
              >
                <IconClose size={14} />
              </button>
            </header>
            <div className="dialog-body">
              {editorError && <p className="form-error">{editorError}</p>}
              <textarea
                className="sftp-editor"
                value={editor.content}
                spellCheck={false}
                onChange={(event) => setEditor({ ...editor, content: event.target.value })}
              />
              <p className="setting-note">
                纯文本编辑，无语法高亮；保存会整段覆盖远端文件，编辑器上限 2 MiB（更大的文件请用下载/上传）。
                {dirty ? "当前有未保存的修改。" : ""}
              </p>
            </div>
            <footer className="dialog-footer">
              <button
                type="button"
                disabled={editorBusy}
                onClick={closeEditor}
              >
                关闭（不保存）
              </button>
              <button
                type="button"
                className="primary"
                disabled={editorBusy}
                onClick={() => void saveEditor()}
              >
                {editorBusy ? "保存中…" : "保存"}
              </button>
            </footer>
          </div>
        </div>
      )}
    </section>
  );
}
