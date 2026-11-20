import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  FONT_SIZE_MAX,
  FONT_SIZE_MIN,
  FONT_STACKS,
  MCP_PORT_DEFAULT,
  MCP_PORT_MAX,
  MCP_PORT_MIN,
  SFTP_WIDTH_MAX,
  SFTP_WIDTH_MIN,
  UI_SCALE_MAX,
  UI_SCALE_MIN,
  UI_SCALE_STEP,
  clampFontSize,
  clampSftpWidth,
  clampUiScale,
  resolveFontStack,
} from "../api/settings";
import type { AppSettings, SidebarTab } from "../api/settings";
import { mcpAudit, mcpResetToken, mcpStatus } from "../api/mcp";
import type { McpAuditEntry, McpStatus } from "../api/mcp";
import { systemFonts } from "../api/fonts";
import type { FontFamily } from "../api/fonts";
import {
  DEFAULT_SHORTCUTS,
  SHORTCUT_ACTIONS,
  findConflicts,
  formatCombo,
  normalizeCombo,
} from "../lib/shortcuts";
import type { ShortcutActionId } from "../lib/shortcuts";
import { api, errorMessage } from "../api/session";
import { themeDeleteCustom, themeSaveCustom } from "../api/theme";
import type { CustomThemeRecord } from "../api/theme";
import { BUILTIN_THEMES, CUSTOM_THEME_PREFIX } from "../lib/themes";
import type { ThemePalette } from "../lib/themes";
import { paletteFromRecord, parseThemeFile, toSaveInput } from "../lib/themeImport";
import { useEscapeClose } from "../lib/useEscapeClose";

type Props = {
  /** 当前生效的设置，弹窗只做受控展示 */
  settings: AppSettings;
  /** 已导入的自定义主题（原始记录），主题区按它渲染自定义分区 */
  customThemes: CustomThemeRecord[];
  /** 系统当前是否偏暗，决定「取消跟随系统」时落到哪一套 */
  systemDark: boolean;
  /** 改一项设置：先应用再持久化，由 App 负责防抖 */
  onChange: <K extends keyof AppSettings>(key: K, value: AppSettings[K]) => void;
  /** 恢复全部缺省值 */
  onReset: () => void;
  /** 导入/删除后重拉自定义主题列表，让 App 里的解析结果同步 */
  onReloadThemes: () => Promise<void>;
  /** 状态栏提示（导入成功、删除回落等） */
  onNotice: (text: string) => void;
  onClose: () => void;
};

/** 侧栏默认分页四选一，顺序与图标栏自上而下一致 */
const TAB_OPTIONS: { id: SidebarTab; label: string }[] = [
  { id: "sessions", label: "会话" },
  { id: "tools", label: "工具" },
  { id: "macros", label: "宏" },
  { id: "sftp", label: "文件" },
];

/**
 * 改完 MCP 设置后等多久再拉状态。
 *
 * App 对设置写入做了 300ms 防抖，后端还会联动重启服务；
 * 这里留出余量，避免读到重启前的旧状态。
 */
const MCP_STATUS_REFRESH_DELAY = 500;

/** 监听地址按终端里的写法展示，用等宽字体便于逐字核对端口 */
const MCP_ADDR_FONT_STACK = 'ui-monospace, "SF Mono", Menlo, Consolas, monospace';

/** 「最近调用」默认展示的条数：一屏能扫完，又足够看出最近在调什么 */
const MCP_AUDIT_PREVIEW_ROWS = 20;

/**
 * 把毫秒时间戳格式化成时分秒。
 *
 * 参数：ts 为毫秒时间戳
 * 核心流程：0 表示没有时间信息，显示占位符；其余按本地时区只显示时间，
 * 审计列表都是最近调用，不需要带日期。
 */
function formatAuditTime(ts: number): string {
  if (!ts) return "-";
  return new Date(ts).toLocaleTimeString();
}

/** 导入契约示例：帮助文案里原样展示，照着改就能用 */
const THEME_JSON_SAMPLE = `{
  "name": "我的主题",
  "kind": "dark",
  "colors": {
    "bg": "#16181d", "bgChrome": "#1b1e24", "bgPanel": "#1c1f26",
    "bgHover": "#23272f", "bgActive": "#2b3f57", "border": "#2a2e37",
    "borderStrong": "#3c424e", "text": "#d7dae0", "textDim": "#8b919d",
    "accent": "#4a9eff", "danger": "#ff6b6b"
  },
  "terminal": { "background": "#16181d", "foreground": "#d7dae0", "cursor": "#4a9eff" }
}`;

/**
 * 步进器。
 *
 * 参数：label 为无障碍名称，text 为展示文案，step 为一步的回调，
 *       canPrev/canNext 表示还能不能继续减/加
 * 核心流程：两侧按钮到头就禁用，中间数字用等宽数字排版避免跳动。
 */
function Stepper({
  label,
  text,
  step,
  canPrev,
  canNext,
}: {
  label: string;
  text: string;
  step: (delta: number) => void;
  canPrev: boolean;
  canNext: boolean;
}) {
  return (
    <span className="stepper" role="group" aria-label={label}>
      <button
        type="button"
        className="stepper-button"
        aria-label={`${label}减小`}
        disabled={!canPrev}
        onClick={() => step(-1)}
      >
        −
      </button>
      <span className="stepper-value">{text}</span>
      <button
        type="button"
        className="stepper-button"
        aria-label={`${label}增大`}
        disabled={!canNext}
        onClick={() => step(1)}
      >
        +
      </button>
    </span>
  );
}

/**
 * 主题卡片。
 *
 * 参数：palette 为主题，active 表示当前选中，onSelect 选中回调，
 *       pendingDelete 表示正在二次确认删除，onDelete/onCancelDelete 为删除与取消
 * 核心流程：卡片主体是按钮（可键盘选中），四块色块只作视觉预览；
 * 自定义主题多一个删除按钮，点一次变成「确认删除 + 取消」，避免误删。
 */
function ThemeCard({
  palette,
  active,
  onSelect,
  onDelete,
  onCancelDelete,
  pendingDelete,
}: {
  palette: ThemePalette;
  active: boolean;
  onSelect: () => void;
  onDelete?: () => void;
  onCancelDelete?: () => void;
  pendingDelete?: boolean;
}) {
  // 预览用的四个色块：底色 / 面板色 / 强调色 / 正文色
  const swatches = [
    palette.colors.bg,
    palette.colors.bgPanel,
    palette.colors.accent,
    palette.colors.text,
  ];
  return (
    <div className={`theme-card ${active ? "is-active" : ""}`}>
      <button
        type="button"
        className="theme-card-main"
        aria-pressed={active}
        onClick={onSelect}
        title={`${palette.name}（${palette.kind === "dark" ? "暗色" : "浅色"}）`}
      >
        <span className="theme-card-name">
          {palette.name}
          <span className="theme-card-kind">{palette.kind === "dark" ? "暗" : "浅"}</span>
        </span>
        <span className="theme-swatches" aria-hidden="true">
          {swatches.map((color, index) => (
            <span
              key={`${palette.id}-${index}`}
              className="theme-swatch"
              style={{ background: color }}
            />
          ))}
        </span>
      </button>
      {onDelete ? (
        pendingDelete ? (
          <span className="theme-card-actions">
            <button type="button" className="theme-delete is-danger" onClick={onDelete}>
              确认删除
            </button>
            <button type="button" className="theme-delete" onClick={onCancelDelete}>
              取消
            </button>
          </span>
        ) : (
          <button type="button" className="theme-delete" onClick={onDelete}>
            删除
          </button>
        )
      ) : null}
    </div>
  );
}

/**
 * 终端字体选择器。
 *
 * 参数：value 为当前设置值（族名，或迁移后留下的完整字体栈），
 *       onSelect 选中回调，参数是要写回设置的字体值
 * 核心流程：挂载时枚举一次系统字体 -> 输入框实时过滤（不区分大小写、子串匹配）->
 * 等宽字体排在前面（后端已按族名排序，这里用稳定排序只把等宽提上来）->
 * 第一项固定是「系统等宽（默认）」-> 列表里没有的族名可以直接敲进输入框按回车。
 * 枚举为空或调用失败时不报错：照样给出输入框和默认项，用户手输兜底。
 */
function FontPicker({
  value,
  onSelect,
}: {
  value: string;
  onSelect: (next: string) => void;
}) {
  const [fonts, setFonts] = useState<FontFamily[]>([]);
  const [loading, setLoading] = useState(true);
  /** 枚举本身失败（IPC 出错），与「本机确实没字体」分开提示 */
  const [failed, setFailed] = useState(false);
  const [keyword, setKeyword] = useState("");

  useEffect(() => {
    // 弹窗关掉后回调仍可能到达，用一个开关丢掉迟到结果
    let alive = true;
    systemFonts()
      .then((list) => {
        if (alive) setFonts(list);
      })
      .catch(() => {
        if (alive) setFailed(true);
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, []);

  /** 等宽优先：Array.prototype.sort 是稳定排序，同组内保持后端给的族名字典序 */
  const ordered = useMemo(
    () => [...fonts].sort((a, b) => Number(b.monospaced) - Number(a.monospaced)),
    [fonts],
  );

  const needle = keyword.trim().toLowerCase();
  const matched = needle
    ? ordered.filter((font) => font.name.toLowerCase().includes(needle))
    : ordered;
  /** 输入框里的名字刚好是某个枚举到的字体时，不再额外给「手输」入口 */
  const typedExists = fonts.some((font) => font.name.toLowerCase() === needle);
  const showTyped = needle.length > 0 && !typedExists;
  const current = value.trim();
  const isDefault = current === FONT_STACKS.default;

  /**
   * 采用输入框里的名字。
   *
   * 参数：无
   * 核心流程：去掉首尾空白后当族名存下，并清空过滤词让列表回到全量。
   */
  const commitTyped = () => {
    const typed = keyword.trim();
    if (!typed) return;
    onSelect(typed);
    setKeyword("");
  };

  return (
    <div className="font-picker">
      <input
        id="settings-font-family"
        className="font-filter"
        type="text"
        autoComplete="off"
        spellCheck={false}
        value={keyword}
        placeholder={loading ? "正在读取系统字体…" : "搜索字体，或直接输入字体名后回车"}
        aria-label="搜索或输入终端字体"
        onChange={(event) => setKeyword(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            commitTyped();
          }
        }}
      />
      <div className="font-list" role="listbox" aria-label="字体候选">
        <button
          type="button"
          role="option"
          aria-selected={isDefault}
          className={`font-option ${isDefault ? "is-active" : ""}`}
          onClick={() => onSelect(FONT_STACKS.default)}
        >
          <span className="font-option-name">系统等宽（默认）</span>
          <span className="font-option-tag">默认</span>
        </button>
        {showTyped && (
          <button
            type="button"
            role="option"
            aria-selected={current === keyword.trim()}
            className={`font-option ${current === keyword.trim() ? "is-active" : ""}`}
            onClick={commitTyped}
          >
            <span className="font-option-name">使用「{keyword.trim()}」</span>
            <span className="font-option-tag">手输</span>
          </button>
        )}
        {matched.map((font) => (
          <button
            key={font.name}
            type="button"
            role="option"
            aria-selected={current === font.name}
            className={`font-option ${current === font.name ? "is-active" : ""}`}
            onClick={() => onSelect(font.name)}
          >
            <span className="font-option-name">{font.name}</span>
            {font.monospaced && <span className="font-option-tag">等宽</span>}
          </button>
        ))}
        {!loading && matched.length === 0 && !showTyped && (
          <p className="setting-note font-empty">
            {failed || fonts.length === 0
              ? "未能读取系统字体，可直接输入字体名"
              : "没有匹配的字体，可直接输入字体名后回车"}
          </p>
        )}
      </div>
      <p className="setting-note font-current" title={resolveFontStack(current)}>
        当前：{isDefault ? "系统等宽（默认）" : current}
      </p>
    </div>
  );
}

/**
 * 首选项弹窗。
 *
 * 参数：settings 为当前设置，customThemes 为已导入主题，systemDark 为系统明暗，
 *       onChange 单项修改，onReset 恢复默认，onReloadThemes 重拉主题列表，
 *       onNotice 状态栏提示，onClose 关闭
 * 核心流程：所有控件都是受控的，改动立刻回调给 App -> App 先应用再防抖落库；
 * 主题区把内置/自定义/导入放进同一个滚动容器，主题再多也不会撑爆弹窗；
 * Esc 关闭，焦点环由全局 :focus-visible 样式保证。
 */
export function SettingsDialog({
  settings,
  customThemes,
  systemDark,
  onChange,
  onReset,
  onReloadThemes,
  onNotice,
  onClose,
}: Props) {
  /** 导入成功后的一次性回执文案 */
  /** 正在录制键位的动作 id，null 表示没在录制 */
  const [recording, setRecording] = useState<ShortcutActionId | null>(null);
  /** 录制时的即时报错（冲突、非法组合） */
  const [shortcutError, setShortcutError] = useState<string | null>(null);

  const [importOk, setImportOk] = useState<string | null>(null);
  /** 导入失败的具体原因（字段名/颜色值/体积），不要只报「导入失败」 */
  const [importError, setImportError] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  /** 正在等待二次确认删除的主题 id */
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  /** 5 秒没等到选择器返回就提示（xrdp 下 dialog 插件可能挂起） */
  const pickTimer = useRef<number | null>(null);
  /** MCP 端口输入框的文本：允许中途是空串，失焦时才校验回写 */
  const [portText, setPortText] = useState(String(settings.mcpPort));
  /** MCP 服务当前状态：打开弹窗时拉一次，改完设置或重置令牌后再拉 */
  const [mcpState, setMcpState] = useState<McpStatus | null>(null);
  /** MCP 操作（重置令牌、读取状态）失败的原因，直接显示在分组里 */
  const [mcpActionError, setMcpActionError] = useState<string | null>(null);
  /** 最近的 MCP 调用审计，打开弹窗与点「刷新」时各拉一次 */
  const [auditRows, setAuditRows] = useState<McpAuditEntry[]>([]);
  /** 审计读取失败的原因，保留上一次的列表不清空 */
  const [auditError, setAuditError] = useState<string | null>(null);
  /** 审计正在拉取，按钮据此禁用避免连点 */
  const [auditLoading, setAuditLoading] = useState(false);
  /** 状态请求序号：连续刷新时只认最后一次的结果，迟到的旧结果直接丢掉 */
  const mcpRefreshSeq = useRef(0);
  /** 首次渲染由挂载时那次拉取负责，设置变更的监听跳过第一轮 */
  const mcpSkipFirstChange = useRef(true);

  // 端口被外部改动（恢复默认、重新载入设置）时把输入框同步过来
  useEffect(() => {
    setPortText(String(settings.mcpPort));
  }, [settings.mcpPort]);

  /**
   * 拉取一次 MCP 服务状态。
   *
   * 参数：无
   * 核心流程：序号自增后发起请求，只有最新一次的结果会写进状态；
   * 失败时保留上一次的状态，只把原因写进分组错误行，避免一次抖动把整块信息清空。
   */
  const refreshMcpStatus = useCallback(async () => {
    const seq = ++mcpRefreshSeq.current;
    try {
      const next = await mcpStatus();
      if (seq !== mcpRefreshSeq.current) return;
      setMcpState(next);
      setMcpActionError(null);
    } catch (err) {
      if (seq !== mcpRefreshSeq.current) return;
      setMcpActionError(`读取 MCP 状态失败：${errorMessage(err)}`);
    }
  }, []);

  /**
   * 拉取最近的 MCP 调用审计。
   *
   * 参数：无
   * 核心流程：成功时整表替换，失败时只写错误行并保留旧列表；
   * 无论成败都关掉 loading，按钮不会卡在禁用状态。
   */
  const refreshMcpAudit = useCallback(async () => {
    setAuditLoading(true);
    try {
      const rows = await mcpAudit(MCP_AUDIT_PREVIEW_ROWS);
      setAuditRows(rows);
      setAuditError(null);
    } catch (err) {
      setAuditError(`读取调用记录失败：${errorMessage(err)}`);
    } finally {
      setAuditLoading(false);
    }
  }, []);

  // 打开弹窗时先拉一次：未启用时也能看到端口有没有被别的进程占着
  useEffect(() => {
    void refreshMcpStatus();
  }, [refreshMcpStatus]);

  // 打开弹窗时同时拉一次审计，和状态一起展示
  useEffect(() => {
    void refreshMcpAudit();
  }, [refreshMcpAudit]);

  // 开关或端口改完后等 App 防抖落库、后端重启服务，再拉一次刷新展示
  useEffect(() => {
    if (mcpSkipFirstChange.current) {
      mcpSkipFirstChange.current = false;
      return undefined;
    }
    const timer = window.setTimeout(() => {
      void refreshMcpStatus();
    }, MCP_STATUS_REFRESH_DELAY);
    return () => window.clearTimeout(timer);
  }, [settings.mcpEnabled, settings.mcpPort, refreshMcpStatus]);

  // Esc 关闭：走全局弹窗栈，本弹窗之上若还叠着别的弹窗，Esc 只关最上层；
  // 正在录制快捷键时退出栈，把 Esc 让给录制逻辑（那一下只取消录制，不关弹窗）
  useEscapeClose(onClose, recording === null);

  // 弹窗卸载时清掉超时定时器，避免关窗后再弹提示
  useEffect(
    () => () => {
      if (pickTimer.current !== null) window.clearTimeout(pickTimer.current);
    },
    [],
  );

  /** 自定义主题解析成调色板；payload 坏掉的记录跳过，不让一条脏数据拖垮列表 */
  const customPalettes = useMemo(
    () =>
      customThemes
        .map((record) => paletteFromRecord(record))
        .filter((palette): palette is ThemePalette => palette !== null),
    [customThemes],
  );

  /** 系统配对下拉的候选：内置 + 自定义（不含 system 自己） */
  const pairOptions = useMemo(() => [...BUILTIN_THEMES, ...customPalettes], [customPalettes]);

  const scalePercent = Math.round(settings.uiScale * 100);
  const systemMode = settings.theme === "system";

  /** 令牌缺失时按空串展示，输入框不会是 undefined 受控告警 */
  const mcpToken = mcpState?.token ?? "";
  /** 最近一次启动失败的原因，成功后后端会清空 */
  const mcpLastError = mcpState?.lastError ?? null;
  /** 监听地址：后端直接给 host:port；运行中却拿不到地址时用设置里的端口拼一个 */
  const mcpAddr = mcpState
    ? mcpState.addr?.trim() || (mcpState.running ? `127.0.0.1:${mcpState.port}` : "")
    : "";

  /**
   * 选中一套主题。
   *
   * 参数：id 为主题 id（内置 id 或 custom:<uuid>）
   * 核心流程：只改 theme 键 -> App 解析成调色板后立刻应用到界面与已打开的终端。
   */
  const selectTheme = (id: string) => onChange("theme", id);

  /**
   * 调整界面缩放。
   *
   * 参数：next 为缩放系数
   * 核心流程：统一走 clampUiScale，夹进 0.85 ~ 1.25 并对齐 0.05 步长。
   */
  const setScale = (next: number) => onChange("uiScale", clampUiScale(next));

  /**
   * 调整终端字号。
   *
   * 参数：next 为目标字号
   * 核心流程：夹进 10 ~ 24 的整数，非法值在这里就被挡住。
   */
  const setFontSize = (next: number) => onChange("terminalFontSize", clampFontSize(next));

  /**
   * 提交 MCP 监听端口。
   *
   * 参数：无，读取输入框里的文本
   * 核心流程：非整数或超出 1024 ~ 65535 时回落到缺省端口并写回设置，
   * 合法值直接写入；两种情况都把输入框文本同步成最终值，不留下脏输入。
   */
  const commitMcpPort = () => {
    const parsed = Number(portText.trim());
    const next =
      Number.isInteger(parsed) && parsed >= MCP_PORT_MIN && parsed <= MCP_PORT_MAX
        ? parsed
        : MCP_PORT_DEFAULT;
    setPortText(String(next));
    onChange("mcpPort", next);
  };

  /**
   * 复制当前 MCP 令牌。
   *
   * 参数：token 为要写入剪贴板的令牌
   * 核心流程：剪贴板不可用或写入被拒时静默忽略，用户可以手动选中输入框里的文本复制。
   */
  const copyMcpToken = (token: string) => {
    const clipboard = navigator.clipboard;
    if (!token || !clipboard) return;
    void clipboard.writeText(token).catch(() => {
      // 静默忽略：复制失败不打断设置流程，也不弹额外提示
    });
  };

  /**
   * 重置 MCP 令牌。
   *
   * 参数：无
   * 核心流程：先二次确认（已连接的客户端会失效）-> 调后端生成新令牌并重启服务 ->
   * 成功后重拉状态把新令牌显示出来；失败把中文原因写进分组错误行。
   */
  const resetMcpToken = async () => {
    const confirmed = window.confirm(
      "重置后已连接的 MCP 客户端需要重新配置令牌，确定要重置吗？",
    );
    if (!confirmed) return;
    setMcpActionError(null);
    try {
      await mcpResetToken();
      await refreshMcpStatus();
    } catch (err) {
      setMcpActionError(`重置令牌失败：${errorMessage(err)}`);
    }
  };

  /**
   * 切换「跟随系统」。
   *
   * 参数：enabled 为开关状态
   * 核心流程：打开时写 system；关闭时落到系统当前明暗所对应的那套，
   * 避免用户在浅色系统下取消跟随却被切回暗色。
   */
  const toggleSystem = (enabled: boolean) => {
    if (enabled) {
      selectTheme("system");
      return;
    }
    selectTheme(systemDark ? settings.themeSystemDark : settings.themeSystemLight);
  };

  /**
   * 导入一份主题 JSON。
   *
   * 参数：无
   * 核心流程：选文件（5 秒无响应给提示）-> 读全文 -> 纯函数校验 ->
   * 通过则落库 -> 重拉列表 -> 立即切到新主题；
   * 任何一步失败都把具体原因写进错误行。
   */
  const importTheme = async () => {
    setImportError(null);
    setImportOk(null);
    setImporting(true);
    pickTimer.current = window.setTimeout(() => {
      setImportError("文件选择器 5 秒未响应（xrdp 下常见），请重试，或把 JSON 放到本机后再导入");
      setImporting(false);
    }, 5000);
    try {
      const picked = await openDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "主题 JSON", extensions: ["json"] }],
      });
      if (pickTimer.current !== null) {
        window.clearTimeout(pickTimer.current);
        pickTimer.current = null;
      }
      if (typeof picked !== "string") {
        setImporting(false);
        return;
      }

      const text = await api.readTextFile(picked);
      const checked = parseThemeFile(text);
      if (!checked.ok) {
        setImportError(checked.error);
        setImporting(false);
        return;
      }

      const id = await themeSaveCustom(toSaveInput(checked));
      await onReloadThemes();
      onChange("theme", `${CUSTOM_THEME_PREFIX}${id}`);
      setImportOk(
        `已导入「${checked.name}」并切换过去（${checked.kind === "dark" ? "暗色" : "浅色"}）`,
      );
      onNotice(`已导入主题「${checked.name}」`);
    } catch (err) {
      if (pickTimer.current !== null) {
        window.clearTimeout(pickTimer.current);
        pickTimer.current = null;
      }
      setImportError(`导入失败：${errorMessage(err)}`);
    } finally {
      setImporting(false);
    }
  };

  /**
   * 删除一套自定义主题。
   *
   * 参数：palette 为待删除的主题（id 形如 custom:<uuid>）
   * 核心流程：去掉前缀传给后端 -> 重拉列表 -> 删的正是当前主题时回落到暗色
   * 并提示，避免界面挂在一套已经不存在的主题上。
   */
  const removeTheme = async (palette: ThemePalette) => {
    const recordId = palette.id.slice(CUSTOM_THEME_PREFIX.length);
    try {
      await themeDeleteCustom(recordId);
      await onReloadThemes();
      setPendingDelete(null);
      if (settings.theme === palette.id) {
        onChange("theme", "dark");
        onNotice(`已删除「${palette.name}」，主题回落到「暗色」`);
      } else {
        onNotice(`已删除主题「${palette.name}」`);
      }
    } catch (err) {
      setImportError(`删除失败：${errorMessage(err)}`);
    }
  };

  // 录制态：捕获阶段监听，按下组合就写入；Esc 取消
  useEffect(() => {
    if (recording === null) return undefined;
    const onKeyDown = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();
      if (event.key === "Escape") {
        setRecording(null);
        setShortcutError(null);
        return;
      }
      const combo = normalizeCombo(event);
      if (!combo) return;
      const conflict = SHORTCUT_ACTIONS.find(
        (action) => action.id !== recording && settings.shortcuts[action.id] === combo,
      );
      if (conflict) {
        setShortcutError(`${formatCombo(combo)} 已被「${conflict.label}」占用`);
        return;
      }
      onChange("shortcuts", { ...settings.shortcuts, [recording]: combo });
      setRecording(null);
      setShortcutError(null);
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [recording, settings.shortcuts, onChange]);

  const conflicts = findConflicts(settings.shortcuts);

  return (
    <div className="modal-backdrop" role="presentation">
      <div
        className="modal modal-wide settings-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="首选项"
      >
        <div className="dialog-header">
          <h2>首选项</h2>
          <button type="button" className="dialog-close" aria-label="关闭" onClick={onClose}>
            ×
          </button>
        </div>

        <div className="dialog-body">
          <section className="settings-group">
            <h3>外观</h3>

            <div className="setting-row">
              <label className="setting-label" htmlFor="settings-follow-system">
                跟随系统
              </label>
              <label className="check-line">
                <input
                  id="settings-follow-system"
                  className="inline-check"
                  type="checkbox"
                  checked={systemMode}
                  onChange={(event) => toggleSystem(event.target.checked)}
                />
                <span>系统配色变化时自动切换，无需重启</span>
              </label>
            </div>

            {/* 配对下拉始终可见，未开启跟随时置灰 -> 用户能先看到「可以怎么配」 */}
            <div className="setting-row">
              <label className="setting-label" htmlFor="settings-system-dark">
                系统暗色用
              </label>
              <select
                id="settings-system-dark"
                value={settings.themeSystemDark}
                disabled={!systemMode}
                onChange={(event) => onChange("themeSystemDark", event.target.value)}
              >
                {pairOptions.map((palette) => (
                  <option key={palette.id} value={palette.id}>
                    {palette.name}
                  </option>
                ))}
              </select>
            </div>
            <div className="setting-row">
              <label className="setting-label" htmlFor="settings-system-light">
                系统浅色用
              </label>
              <select
                id="settings-system-light"
                value={settings.themeSystemLight}
                disabled={!systemMode}
                onChange={(event) => onChange("themeSystemLight", event.target.value)}
              >
                {pairOptions.map((palette) => (
                  <option key={palette.id} value={palette.id}>
                    {palette.name}
                  </option>
                ))}
              </select>
            </div>
            {!systemMode && (
              <p className="setting-note">开启「跟随系统」后，上表两套主题会随系统配色自动切换。</p>
            )}

            {/* 三个分区共用一个滚动容器，弹窗高度不随主题数量膨胀 */}
            <div className="theme-scroll">
              <p className="theme-subtitle">内置主题</p>
              <div className="theme-grid" role="group" aria-label="内置主题">
                {BUILTIN_THEMES.map((palette) => (
                  <ThemeCard
                    key={palette.id}
                    palette={palette}
                    active={settings.theme === palette.id}
                    onSelect={() => selectTheme(palette.id)}
                  />
                ))}
              </div>

              <p className="theme-subtitle">自定义主题</p>
              {customPalettes.length === 0 ? (
                <p className="setting-note">
                  还没有导入的主题。可以在下方「导入主题」里加入自己的配色。
                </p>
              ) : (
                <div className="theme-grid" role="group" aria-label="自定义主题">
                  {customPalettes.map((palette) => (
                    <ThemeCard
                      key={palette.id}
                      palette={palette}
                      active={settings.theme === palette.id}
                      onSelect={() => selectTheme(palette.id)}
                      pendingDelete={pendingDelete === palette.id}
                      onDelete={() => {
                        if (pendingDelete === palette.id) void removeTheme(palette);
                        else setPendingDelete(palette.id);
                      }}
                      onCancelDelete={() => setPendingDelete(null)}
                    />
                  ))}
                </div>
              )}

              <p className="theme-subtitle">导入主题</p>
              <div className="theme-import-row">
                <button type="button" onClick={() => void importTheme()} disabled={importing}>
                  {importing ? "等待选择文件…" : "导入主题…"}
                </button>
                <span className="setting-note">选择 .json 文件，最大 16 KB</span>
              </div>
              {importError && (
                <p className="theme-message is-error" role="alert">
                  {importError}
                </p>
              )}
              {importOk && <p className="theme-message is-ok">{importOk}</p>}
              <details className="theme-help">
                <summary>主题 JSON 契约</summary>
                <p className="setting-note">
                  colors 的 11 个键必填，颜色写 #rgb 或 #rrggbb；terminal 可省略，省略时由
                  colors 派生（背景取 bg、前景取 text、光标取 accent、选区取 bgActive，其余 ansi
                  色用内置暗色的值兜底）。主题只描述颜色，不会执行任何代码。
                </p>
                <pre>{THEME_JSON_SAMPLE}</pre>
              </details>
            </div>

            <div className="setting-row">
              <label className="setting-label" htmlFor="settings-scale">
                界面缩放
              </label>
              <div className="scale-control">
                <Stepper
                  label="界面缩放"
                  text={`${scalePercent}%`}
                  canPrev={settings.uiScale > UI_SCALE_MIN}
                  canNext={settings.uiScale < UI_SCALE_MAX}
                  step={(delta) => setScale(settings.uiScale + delta * UI_SCALE_STEP)}
                />
                <input
                  id="settings-scale"
                  className="range-input"
                  type="range"
                  min={UI_SCALE_MIN}
                  max={UI_SCALE_MAX}
                  step={UI_SCALE_STEP}
                  onChange={(event) => setScale(Number(event.target.value))}
                />
              </div>
            </div>
            <p className="setting-note">
              只缩放字号，行高与间距保持像素值，避免整体布局重排；主题颜色全部走 CSS 变量，缩放时一起生效。
            </p>
          </section>

          <section className="settings-group">
            <h3>终端</h3>

            <div className="setting-row">
              <span className="setting-label" id="settings-font-size-label">
                字号
              </span>
              <div className="scale-control">
                <Stepper
                  label="终端字号"
                  text={`${settings.terminalFontSize} px`}
                  canPrev={settings.terminalFontSize > FONT_SIZE_MIN}
                  canNext={settings.terminalFontSize < FONT_SIZE_MAX}
                  step={(delta) => setFontSize(settings.terminalFontSize + delta)}
                />
                <span className="setting-note" id="settings-font-size-hint">
                  {FONT_SIZE_MIN} ~ {FONT_SIZE_MAX}，也可在终端里 Ctrl + 滚轮调整
                </span>
              </div>
            </div>

            <div className="setting-row setting-row-tall">
              <label className="setting-label" htmlFor="settings-font-family">
                字体
              </label>
              <FontPicker
                value={settings.terminalFontFamily}
                onSelect={(next) => onChange("terminalFontFamily", next)}
              />
            </div>

            <div className="setting-row">
              <span className="setting-label">实时预览</span>
              <div
                className="term-preview"
                style={{
                  fontFamily: resolveFontStack(settings.terminalFontFamily),
                  fontSize: `${settings.terminalFontSize}px`,
                }}
                aria-label="终端字号与字体预览"
              >
                user@rustmobax:~$ echo 字号 {settings.terminalFontSize}px
              </div>
            </div>

            <div className="setting-row">
              <label className="setting-label" htmlFor="settings-terminal-banner">
                连接时显示信息横幅
              </label>
              <label className="check-line">
                <input
                  id="settings-terminal-banner"
                  className="inline-check"
                  type="checkbox"
                  checked={settings.terminalBanner}
                  onChange={(event) => onChange("terminalBanner", event.target.checked)}
                />
                <span>连上后在终端顶部画出本次连接生效的认证方式、SFTP 与隧道数</span>
              </label>
            </div>
          </section>

          <section className="settings-group">
            <h3>侧栏</h3>

            <div className="setting-row">
              <span className="setting-label" id="settings-default-tab-label">
                默认打开的分页
              </span>
              <div className="segmented" role="group" aria-labelledby="settings-default-tab-label">
                {TAB_OPTIONS.map((option) => (
                  <button
                    key={option.id}
                    type="button"
                    className={`segmented-button ${
                      settings.sidebarDefaultTab === option.id ? "is-active" : ""
                    }`}
                    aria-pressed={settings.sidebarDefaultTab === option.id}
                    onClick={() => onChange("sidebarDefaultTab", option.id)}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
            </div>

            <div className="setting-row">
              <label className="setting-label" htmlFor="settings-auto-connect">
                切到文件分页时自动连接
              </label>
              <label className="check-line">
                <input
                  id="settings-auto-connect"
                  className="inline-check"
                  type="checkbox"
                  checked={settings.sidebarAutoConnect}
                  onChange={(event) => onChange("sidebarAutoConnect", event.target.checked)}
                />
                <span>切到「文件」分页就用当前会话建立 SFTP 连接</span>
              </label>
            </div>

            <div className="setting-row">
              <label className="setting-label" htmlFor="settings-follow-cwd">
                跟随终端目录
              </label>
              <label className="check-line">
                <input
                  id="settings-follow-cwd"
                  className="inline-check"
                  type="checkbox"
                  checked={settings.sftpFollowCwd}
                  onChange={(event) => onChange("sftpFollowCwd", event.target.checked)}
                />
                <span>终端上报工作目录时同步文件分页</span>
              </label>
            </div>

            <div className="setting-row">
              <label className="setting-label" htmlFor="settings-sftp-width">
                侧栏宽度
              </label>
              <div className="scale-control">
                <input
                  id="settings-sftp-width"
                  className="range-input"
                  type="range"
                  min={SFTP_WIDTH_MIN}
                  max={SFTP_WIDTH_MAX}
                  step={10}
                  value={settings.sftpWidth}
                  onChange={(event) =>
                    onChange("sftpWidth", clampSftpWidth(Number(event.target.value)))
                  }
                />
                <span className="stepper-value">{settings.sftpWidth} px</span>
              </div>
            </div>
            <p className="setting-note">
              也可以直接拖动侧栏右边缘调整，允许 260 到 520 px；窄于 360 px 时文件分页会自动切成紧凑行。
            </p>
          </section>

          <section className="settings-group">
            <h3>MCP 服务</h3>

            <div className="setting-row">
              <label className="setting-label" htmlFor="settings-mcp-enabled">
                启用 MCP 服务
              </label>
              <label className="check-line">
                <input
                  id="settings-mcp-enabled"
                  className="inline-check"
                  type="checkbox"
                  checked={settings.mcpEnabled}
                  onChange={(event) => onChange("mcpEnabled", event.target.checked)}
                />
                <span>允许本机客户端通过 MCP 协议调用终端与会话能力</span>
              </label>
            </div>

            <div className="setting-row">
              <label className="setting-label" htmlFor="settings-mcp-port">
                监听端口
              </label>
              <input
                id="settings-mcp-port"
                className="input-narrow"
                type="number"
                min={MCP_PORT_MIN}
                max={MCP_PORT_MAX}
                step={1}
                value={portText}
                disabled={!settings.mcpEnabled}
                onChange={(event) => setPortText(event.target.value)}
                onBlur={commitMcpPort}
                onKeyDown={(event) => {
                  if (event.key === "Enter") commitMcpPort();
                }}
                aria-describedby="settings-mcp-hint"
              />
            </div>
            <div className="setting-row">
              <span className="setting-label">运行状态</span>
              <div>
                <span className="tunnel-status">
                  <span
                    className={`tunnel-dot ${mcpState?.running ? "is-running" : ""}`}
                    aria-hidden="true"
                  />
                  <span>{mcpState?.running ? "运行中" : "未运行"}</span>
                  {mcpAddr && (
                    <span style={{ fontFamily: MCP_ADDR_FONT_STACK }} title={mcpAddr}>
                      {mcpAddr}
                    </span>
                  )}
                </span>
                {mcpLastError && (
                  <p className="form-error" role="alert">
                    服务启动失败：{mcpLastError}
                  </p>
                )}
              </div>
            </div>

            <div className="setting-row">
              <label className="setting-label" htmlFor="settings-mcp-token">
                访问令牌
              </label>
              <div className="path-row">
                <input
                  id="settings-mcp-token"
                  type="text"
                  readOnly
                  spellCheck={false}
                  autoComplete="off"
                  value={mcpToken}
                  placeholder="尚未生成"
                  disabled={!settings.mcpEnabled}
                  aria-label="MCP 访问令牌"
                />
                <button
                  type="button"
                  disabled={!settings.mcpEnabled}
                  onClick={() => copyMcpToken(mcpToken)}
                >
                  复制
                </button>
                <button
                  type="button"
                  disabled={!settings.mcpEnabled}
                  onClick={() => void resetMcpToken()}
                >
                  重置令牌
                </button>
              </div>
            </div>

            <div className="setting-row">
              <span className="setting-label">最近调用</span>
              <div style={{ width: "100%" }}>
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <span className="setting-note">参数只保留键名与字节数，不含原文</span>
                  <button
                    type="button"
                    onClick={() => void refreshMcpAudit()}
                    disabled={auditLoading}
                  >
                    {auditLoading ? "刷新中" : "刷新"}
                  </button>
                </div>
                {auditError && (
                  <p className="form-error" role="alert">
                    {auditError}
                  </p>
                )}
                {auditRows.length === 0 ? (
                  <p className="setting-note">暂无调用记录</p>
                ) : (
                  <div
                    style={{
                      display: "grid",
                      gap: 2,
                      marginTop: 6,
                      fontFamily: MCP_ADDR_FONT_STACK,
                      fontSize: "calc(11px * var(--ui-scale))",
                    }}
                  >
                    <div
                      style={{
                        display: "grid",
                        gridTemplateColumns: "78px 128px minmax(0, 1fr) 58px 48px",
                        gap: 8,
                        color: "var(--text-dim)",
                      }}
                    >
                      <span>时间</span>
                      <span>工具</span>
                      <span>参数摘要</span>
                      <span>耗时</span>
                      <span>结果</span>
                    </div>
                    {auditRows.map((row) => (
                      <div
                        key={row.id}
                        className={row.ok ? undefined : "form-error"}
                        title={!row.ok && row.error ? row.error : undefined}
                        style={{
                          display: "grid",
                          gridTemplateColumns: "78px 128px minmax(0, 1fr) 58px 48px",
                          gap: 8,
                          margin: 0,
                          whiteSpace: "nowrap",
                        }}
                      >
                        <span style={{ color: "var(--text-dim)" }}>
                          {formatAuditTime(row.ts)}
                        </span>
                        <span
                          style={{ overflow: "hidden", textOverflow: "ellipsis" }}
                          title={row.tool}
                        >
                          {row.tool}
                        </span>
                        <span
                          style={{ overflow: "hidden", textOverflow: "ellipsis" }}
                          title={row.argsDigest}
                        >
                          {row.argsDigest}
                        </span>
                        <span style={{ color: "var(--text-dim)" }}>{row.durationMs} ms</span>
                        <span style={row.ok ? { color: "var(--text-dim)" } : undefined}>
                          {row.ok ? "成功" : "失败"}
                        </span>
                        {!row.ok && row.error && (
                          <span
                            style={{
                              gridColumn: "1 / -1",
                              overflow: "hidden",
                              textOverflow: "ellipsis",
                              opacity: 0.85,
                            }}
                          >
                            {row.error}
                          </span>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>

            {mcpActionError && (
              <p className="form-error" role="alert">
                {mcpActionError}
              </p>
            )}
            <p className="setting-note" id="settings-mcp-hint">
              仅监听本机回环地址，客户端需带 Bearer 令牌；重置后已连接的 MCP 客户端需要重新配置令牌
            </p>
          </section>

          <section className="settings-group">
            <h3>快捷键</h3>
            {conflicts.length > 0 && (
              <p className="form-error" role="alert">
                有按键被重复占用，冲突的动作会以列表里先出现的那个为准
              </p>
            )}
            {shortcutError && (
              <p className="form-error" role="alert">
                {shortcutError}
              </p>
            )}
            <div className="shortcut-list">
              {SHORTCUT_ACTIONS.map((action) => {
                const combo = settings.shortcuts[action.id];
                const isRecording = recording === action.id;
                const conflicted = conflicts.includes(action.id);
                return (
                  <div
                    key={action.id}
                    className={`shortcut-row ${conflicted ? "is-conflict" : ""}`}
                  >
                    <span className="shortcut-label" title={action.hint}>
                      {action.label}
                    </span>
                    <span className="shortcut-combo">
                      {isRecording ? "按下新组合…" : formatCombo(combo)}
                    </span>
                    <button
                      type="button"
                      className={isRecording ? "primary" : ""}
                      onClick={() => {
                        setShortcutError(null);
                        setRecording(isRecording ? null : action.id);
                      }}
                    >
                      {isRecording ? "取消" : "录制"}
                    </button>
                    <button
                      type="button"
                      disabled={DEFAULT_SHORTCUTS[action.id] === combo}
                      onClick={() => {
                        setShortcutError(null);
                        setRecording(null);
                        onChange("shortcuts", {
                          ...settings.shortcuts,
                          [action.id]: DEFAULT_SHORTCUTS[action.id],
                        });
                      }}
                    >
                      重置
                    </button>
                  </div>
                );
              })}
            </div>
            <p className="setting-note">
              点「录制」后按下想要的组合，Esc 取消；终端内的搜索与粘贴仍由终端自己处理，不在此处覆盖。
            </p>
          </section>
        </div>

        <div className="dialog-footer">
          <button type="button" onClick={onReset}>
            恢复默认
          </button>
          <button type="button" className="primary" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
