import { invoke } from "@tauri-apps/api/core";
import { BUILTIN_THEME_IDS, isCustomThemeId } from "../lib/themes";
import { DEFAULT_SHORTCUTS, parseShortcuts, serializeShortcuts } from "../lib/shortcuts";
import type { ShortcutMap } from "../lib/shortcuts";

/**
 * 主题偏好：system 跟随系统配色，其余取值为内置主题 id 或 custom:<uuid>。
 *
 * 之所以用 string：自定义主题是运行期从后端拉回来的，编译期无法枚举；
 * 合法性交给 parseThemeChoice 做白名单校验。
 */
export type ThemePreference = string;
/** 主题明暗种类，system 会被解析成这两者之一 */
export type ResolvedTheme = "dark" | "light";
/**
 * 终端字体的旧预设 id（四档硬编码时代的取值）。
 *
 * 新版本已经改成「枚举系统字体族 + 手输族名」，这个类型只为读取旧库里的值而保留，
 * 迁移规则见 FONT_STACKS 与 parseSettings。
 */
export type TerminalFontId = "default" | "menlo" | "consolas" | "mono";
/** 左侧栏的四个分页，自上而下与 MobaXterm 的图标栏对应 */
export type SidebarTab = "sessions" | "tools" | "macros" | "sftp";

/** 侧栏分页的合法取值，解析设置时按它做白名单校验 */
export const SIDEBAR_TABS: SidebarTab[] = ["sessions", "tools", "macros", "sftp"];

/**
 * 侧栏宽度区间（px）。
 *
 * 键名沿用 sftp.width：早期版本用它存标签内文件面板的宽度，
 * 这一版起同一个值描述整个左栏的宽度，因此不新增后端键。
 */
export const SFTP_WIDTH_MIN = 260;
export const SFTP_WIDTH_MAX = 520;

/**
 * 前端使用的强类型设置。
 *
 * 无参数
 * 核心流程：字段与后端约定键一一对应（映射见 SETTING_KEYS），
 * 所有数值都已在合法区间内，脏值在解析时就被夹好。
 */
export type AppSettings = {
  theme: ThemePreference;
  /** 跟随系统时，系统为暗色用哪套主题 */
  themeSystemDark: ThemePreference;
  /** 跟随系统时，系统为浅色用哪套主题 */
  themeSystemLight: ThemePreference;
  /** 界面缩放系数，0.85 ~ 1.25，步长 0.05 */
  uiScale: number;
  /** 终端字号，10 ~ 24 的整数 */
  terminalFontSize: number;
  /**
   * 终端字体：可能是用户选的族名（如 "JetBrains Mono"），
   * 也可能是迁移后留下来的完整 CSS 字体栈（旧四档预设就是这种形状）。
   * 交给 resolveFontStack 统一转成可用的 font-family 值。
   */
  terminalFontFamily: string;
  /** 侧栏默认展开的分页 */
  sidebarDefaultTab: SidebarTab;
  /** 切到「文件」分页时是否自动建立 SFTP 连接 */
  sidebarAutoConnect: boolean;
  /** 侧栏宽度（px），260 ~ 520，拖动侧栏右边缘调整 */
  sftpWidth: number;
  /** 文件分页是否跟随终端上报的工作目录 */
  sftpFollowCwd: boolean;
  /** 可配置快捷键：动作 id 到组合 */
  shortcuts: ShortcutMap;
};

/** 后端设置键，字段名到键名的唯一映射 -> 改字段不用改魔法字符串 */
export const SETTING_KEYS: Record<keyof AppSettings, string> = {
  theme: "theme",
  themeSystemDark: "theme.systemDark",
  themeSystemLight: "theme.systemLight",
  uiScale: "ui.scale",
  terminalFontSize: "terminal.fontSize",
  terminalFontFamily: "terminal.fontFamily",
  sidebarDefaultTab: "sidebar.defaultTab",
  sidebarAutoConnect: "sidebar.autoConnect",
  // 旧键 sftp.dock / sftp.autoOpen 自本版起不再读取：标签内停靠面板已经移除，
  // 库里的旧值原样保留不删，需要时仍可人工回滚
  sftpWidth: "sftp.width",
  sftpFollowCwd: "sftp.followCwd",
  shortcuts: "shortcuts",
};

/**
 * 旧预设 id 到 CSS 字体栈的映射表。
 *
 * 库里早期存的是 menlo/consolas/mono/default 这样的 id，读到旧值时就靠这张表
 * 还原成字体栈（见 parseSettings），因此它不能删，但界面上不再按它做下拉。
 */
export const FONT_STACKS: Record<string, string> = {
  default: 'ui-monospace, "SF Mono", Menlo, Consolas, "Courier New", monospace',
  menlo: 'Menlo, Monaco, "Courier New", monospace',
  consolas: 'Consolas, "Courier New", monospace',
  mono: '"JetBrains Mono", "Fira Code", ui-monospace, monospace',
};

/** 缺省值：库中没有对应键时用它，也用于「恢复默认」 */
export const DEFAULT_SETTINGS: AppSettings = {
  theme: "dark",
  themeSystemDark: "dark",
  themeSystemLight: "light",
  uiScale: 1,
  terminalFontSize: 13,
  terminalFontFamily: FONT_STACKS.default,
  sidebarDefaultTab: "sessions",
  sidebarAutoConnect: true,
  sftpWidth: 280,
  sftpFollowCwd: true,
  shortcuts: DEFAULT_SHORTCUTS,
};

/** 手输族名时的兜底尾巴：族名本身不存在的机器上还能落到系统等宽 */
const FONT_FALLBACK_TAIL = "ui-monospace, monospace";

/**
 * 把设置里的字体值转成可直接给 CSS / xterm 用的 font-family。
 *
 * 参数：value 为库里的字体值（族名、旧 id，或迁移后的完整字体栈）
 * 核心流程：空值回落默认 -> 旧 id 查表 -> 已经是完整栈（含逗号）原样透传 ->
 * 其余当族名，加引号后补一条等宽兜底。
 * 加引号是必须的：族名常带空格（"JetBrains Mono"），不加引号 CSS 会按空格切成多个
 * 非法标识符，整条声明失效。
 */
export function resolveFontStack(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) return FONT_STACKS.default;
  if (Object.prototype.hasOwnProperty.call(FONT_STACKS, trimmed)) return FONT_STACKS[trimmed];
  if (trimmed.includes(",")) return trimmed;
  return `"${trimmed.replace(/"/g, "")}", ${FONT_FALLBACK_TAIL}`;
}

/** 界面缩放的范围与步长 */
export const UI_SCALE_MIN = 0.85;
export const UI_SCALE_MAX = 1.25;
export const UI_SCALE_STEP = 0.05;
/** 终端字号的合法区间 */
export const FONT_SIZE_MIN = 10;
export const FONT_SIZE_MAX = 24;
/**
 * 把数值夹进区间。
 *
 * 参数：value 为待夹值，min/max 为闭区间端点
 * 核心流程：先取上下界再返回，NaN 会走到 max 分支前由调用方过滤。
 */
function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/**
 * 解析字符串数字，失败时回落到缺省。
 *
 * 参数：raw 为后端返回的原值（可能是 undefined），fallback 为缺省值
 * 核心流程：缺失或非有限数直接返回缺省，避免把 NaN 带进布局。
 */
function parseNumber(raw: string | undefined, fallback: number): number {
  if (raw === undefined) return fallback;
  const value = Number(raw);
  return Number.isFinite(value) ? value : fallback;
}

/**
 * 解析 0/1 布尔值。
 *
 * 参数：raw 为后端返回的原值，fallback 为缺省值
 * 核心流程：兼容 "1"/"0" 与 "true"/"false"，其余脏值回落到缺省。
 */
function parseFlag(raw: string | undefined, fallback: boolean): boolean {
  if (raw === undefined) return fallback;
  if (raw === "1" || raw === "true") return true;
  if (raw === "0" || raw === "false") return false;
  return fallback;
}

/**
 * 把缩放系数夹到合法区间并对齐步长。
 *
 * 参数：value 为任意数值
 * 核心流程：先按 0.05 取整（消除浮点误差），再夹进 0.85 ~ 1.25。
 */
export function clampUiScale(value: number): number {
  const stepped = Math.round(value / UI_SCALE_STEP) * UI_SCALE_STEP;
  return clamp(Math.round(stepped * 100) / 100, UI_SCALE_MIN, UI_SCALE_MAX);
}

/**
 * 把终端字号夹到 10 ~ 24 的整数。
 *
 * 参数：value 为任意数值
 * 核心流程：四舍五入后夹进区间，非法值由调用方先过滤。
 */
export function clampFontSize(value: number): number {
  return clamp(Math.round(value), FONT_SIZE_MIN, FONT_SIZE_MAX);
}

/**
 * 把文件面板宽度夹到存储区间。
 *
 * 参数：value 为任意数值
 * 核心流程：取整后夹进 220 ~ 1600，容器 60% 由 .dock-panel 的 max-width 兜底。
 */
export function clampSftpWidth(value: number): number {
  return clamp(Math.round(value), SFTP_WIDTH_MIN, SFTP_WIDTH_MAX);
}

/**
 * 把设置值序列化成后端要的字符串。
 *
 * 参数：value 为布尔、数字或字符串
 * 核心流程：布尔统一成 "1"/"0"，数字交给 String，和后端约定一致。
 */
export function serializeSetting(value: string | number | boolean | ShortcutMap): string {
  if (typeof value === "boolean") return value ? "1" : "0";
  if (typeof value === "string" || typeof value === "number") return String(value);
  // 快捷键表按 JSON 存，键顺序由 serializeShortcuts 固定
  return serializeShortcuts(value);
}

/** 自定义主题 id 的形状：custom: 后面跟一段 uuid（长度只做宽松限制） */
const CUSTOM_THEME_ID = /^custom:[0-9a-zA-Z-]{4,64}$/;

/**
 * 校验主题选择值。
 *
 * 参数：raw 为库里的原始字符串（可能是脏值/缺失），
 *       fallback 为非法时的回落值，allowSystem 表示这一项能否取 system
 * 核心流程：system -> 内置 id -> custom:<uuid> 依次比对；
 * 自定义主题是否还存在由「取主题」那一步兜底（删掉后回落到缺省）。
 */
export function parseThemeChoice(
  raw: string | undefined,
  fallback: string,
  allowSystem: boolean,
): string {
  if (raw === undefined) return fallback;
  const value = raw.trim();
  if (allowSystem && value === "system") return "system";
  if (BUILTIN_THEME_IDS.includes(value)) return value;
  if (isCustomThemeId(value) && CUSTOM_THEME_ID.test(value)) return value;
  return fallback;
}

/**
 * 迁移终端字体设置值。
 *
 * 参数：raw 为库里的原值（可能是旧档位 id、族名、迁移后的字体栈或缺失）
 * 核心流程：旧 id（default/menlo/consolas/mono）换成 FONT_STACKS 里的字体栈，
 * 让升级后的用户保持原来的观感；其余值一律当用户选的族名原样留着，不做白名单校验
 * —— 枚举不到系统字体时也要能手输，这里卡掉就等于把人锁死了。
 */
function parseFontFamily(raw: string | undefined): string {
  if (raw === undefined) return DEFAULT_SETTINGS.terminalFontFamily;
  const value = raw.trim();
  if (!value) return DEFAULT_SETTINGS.terminalFontFamily;
  if (Object.prototype.hasOwnProperty.call(FONT_STACKS, value)) return FONT_STACKS[value];
  return value;
}

/**
 * 把后端返回的键值表解析成强类型设置。
 *
 * 参数：raw 为 settings_get_all 的结果，库中没有的键不会出现
 * 核心流程：逐字段做白名单校验，枚举不认识就回缺省，
 * 数值统一夹进区间 -> 脏数据不会让界面尺寸失控。
 */
export function parseSettings(raw: Record<string, string>): AppSettings {
  const fontFamily = raw[SETTING_KEYS.terminalFontFamily];
  const defaultTab = raw[SETTING_KEYS.sidebarDefaultTab];

  return {
    theme: parseThemeChoice(raw[SETTING_KEYS.theme], DEFAULT_SETTINGS.theme, true),
    // 系统配对不允许填 system（会自己套自己），非法值回落到暗/浅
    themeSystemDark: parseThemeChoice(
      raw[SETTING_KEYS.themeSystemDark],
      DEFAULT_SETTINGS.themeSystemDark,
      false,
    ),
    themeSystemLight: parseThemeChoice(
      raw[SETTING_KEYS.themeSystemLight],
      DEFAULT_SETTINGS.themeSystemLight,
      false,
    ),
    uiScale: clampUiScale(parseNumber(raw[SETTING_KEYS.uiScale], DEFAULT_SETTINGS.uiScale)),
    terminalFontSize: clampFontSize(
      parseNumber(raw[SETTING_KEYS.terminalFontSize], DEFAULT_SETTINGS.terminalFontSize),
    ),
    terminalFontFamily: parseFontFamily(fontFamily),
    sidebarDefaultTab: SIDEBAR_TABS.some((tab) => tab === defaultTab)
      ? (defaultTab as SidebarTab)
      : DEFAULT_SETTINGS.sidebarDefaultTab,
    sidebarAutoConnect: parseFlag(
      raw[SETTING_KEYS.sidebarAutoConnect],
      DEFAULT_SETTINGS.sidebarAutoConnect,
    ),
    sftpWidth: clampSftpWidth(
      parseNumber(raw[SETTING_KEYS.sftpWidth], DEFAULT_SETTINGS.sftpWidth),
    ),
    sftpFollowCwd: parseFlag(raw[SETTING_KEYS.sftpFollowCwd], DEFAULT_SETTINGS.sftpFollowCwd),
    shortcuts: parseShortcuts(raw[SETTING_KEYS.shortcuts]),
  };
}

/**
 * 读取全部设置。
 *
 * 参数：无
 * 核心流程：直接调 settings_get_all，解析交给调用方（便于迁移逻辑先插手）。
 */
export function settingsGetAll(): Promise<Record<string, string>> {
  return invoke<Record<string, string>>("settings_get_all");
}

/**
 * 写入单个设置。
 *
 * 参数：key 为约定键（≤64 字节），value 为字符串值（≤4096 字节）
 * 核心流程：整值覆盖，后端不做合并；同一 key 的防抖由调用方负责。
 */
export function settingsSet(key: string, value: string): Promise<null> {
  return invoke<null>("settings_set", { key, value });
}

/**
 * 把快捷键表编码成设置值。
 *
 * 参数：map 为当前键位
 * 核心流程：交给 shortcuts 模块统一序列化，保证键顺序稳定；
 * 这里只做一层转出，避免 App 直接依赖 lib 里的序列化细节。
 */
export function encodeShortcuts(map: ShortcutMap): string {
  return serializeShortcuts(map);
}
