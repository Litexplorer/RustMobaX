import type { CustomThemeRecord, CustomThemeInput } from "../api/theme";
import { THEME_NAME_MAX_BYTES, THEME_PAYLOAD_MAX_BYTES } from "../api/theme";
import { CSS_VAR_KEYS, buildTerminal, getTheme, isHexColor } from "./themes";
import type { AnsiSeed, CssVarKey, ThemeKind, ThemePalette } from "./themes";

/**
 * 主题 JSON 的导入契约与校验。
 *
 * 契约（写进设置弹窗的帮助文案）：
 * {
 *   "name": "我的主题",
 *   "kind": "dark",
 *   "colors": { "bg": "#16181d", "bgChrome": "#1b1e24", "bgPanel": "#1c1f26",
 *               "bgHover": "#23272f", "bgActive": "#2b3f57", "border": "#2a2e37",
 *               "borderStrong": "#3c424e", "text": "#d7dae0", "textDim": "#8b919d",
 *               "accent": "#4a9eff", "danger": "#ff6b6b" },
 *   "terminal": { "background": "#16181d", "foreground": "#d7dae0", "cursor": "#4a9eff" }
 * }
 *
 * 校验逻辑全部是纯函数：不碰 DOM、不碰后端 -> 可以单独跑测试。
 * 这里只解析 JSON 与颜色，不执行任何代码，导入的主题不可能带脚本能力。
 */

/** terminal 段允许出现的字段：只认这四个，别的一律忽略 */
const TERMINAL_KEYS = ["background", "foreground", "cursor", "selectionBackground"] as const;
type TerminalKey = (typeof TERMINAL_KEYS)[number];

/** xterm ansi 种子：自定义主题没写 terminal 时用内置 dark 的绿/黄/紫/青兜底 */
function darkSeed(): AnsiSeed {
  const dark = getTheme("dark")?.terminal;
  return {
    green: dark?.green ?? "#6ee7a8",
    yellow: dark?.yellow ?? "#e8c07a",
    magenta: dark?.magenta ?? "#c792ea",
    cyan: dark?.cyan ?? "#89ddff",
  };
}

/** UTF-8 字节数：后端按字节限长，中文一个字算 3 字节 */
export function byteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

/** 校验结果：成功带出可直接提交的 payload 与解析好的调色板，失败带人话原因 */
export type ThemeImportResult =
  | { ok: true; name: string; kind: ThemeKind; payload: string; palette: ThemePalette }
  | { ok: false; error: string };

/** 校验通过后的中间形态 */
type ParsedTheme = {
  name: string;
  kind: ThemeKind;
  colors: Record<CssVarKey, string>;
  terminal: Partial<Record<TerminalKey, string>>;
};

/**
 * 判断是不是普通对象（数组/null 都不算）。
 *
 * 参数：value 为任意值
 * 核心流程：排除 null、数组，只认 typeof object -> 后面的字段读取才安全。
 */
function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * 把任意 JSON 校验成主题。
 *
 * 参数：data 为 JSON.parse 的结果
 * 核心流程：name -> kind -> colors（11 个键逐个查）-> terminal（可选）；
 * 每一步失败都返回具体字段名，方便用户直接定位错在哪一行。
 */
function validateTheme(data: unknown): { ok: true; value: ParsedTheme } | { ok: false; error: string } {
  if (!isPlainObject(data)) {
    return { ok: false, error: "主题文件最外层必须是 JSON 对象（以 { 开头）" };
  }

  const rawName = data.name;
  if (rawName !== undefined && typeof rawName !== "string") {
    return { ok: false, error: "name 必须是字符串" };
  }
  const name = typeof rawName === "string" ? rawName.trim() : "";
  if (name === "") {
    return { ok: false, error: "缺少 name，或 name 是空字符串" };
  }
  const nameBytes = byteLength(name);
  if (nameBytes > THEME_NAME_MAX_BYTES) {
    return {
      ok: false,
      error: `name 有 ${nameBytes} 字节，超过 ${THEME_NAME_MAX_BYTES} 字节上限`,
    };
  }

  const kind = data.kind;
  if (kind !== "dark" && kind !== "light") {
    return { ok: false, error: 'kind 必须是 "dark" 或 "light"' };
  }

  const rawColors = data.colors;
  if (!isPlainObject(rawColors)) {
    return { ok: false, error: "缺少 colors 对象" };
  }
  const colors = {} as Record<CssVarKey, string>;
  for (const key of CSS_VAR_KEYS) {
    const value = (rawColors as Record<string, unknown>)[key];
    if (value === undefined) {
      return { ok: false, error: `缺少 colors.${key}` };
    }
    if (!isHexColor(value)) {
      return {
        ok: false,
        error: `颜色值 ${String(value)} 不是合法十六进制（colors.${key} 支持 #rgb 或 #rrggbb）`,
      };
    }
    colors[key] = value.trim().toLowerCase();
  }

  const terminal: Partial<Record<TerminalKey, string>> = {};
  const rawTerminal = data.terminal;
  if (rawTerminal !== undefined) {
    if (!isPlainObject(rawTerminal)) {
      return { ok: false, error: "terminal 必须是对象（不想要可以整段删掉）" };
    }
    for (const key of TERMINAL_KEYS) {
      const value = (rawTerminal as Record<string, unknown>)[key];
      if (value === undefined) continue;
      if (!isHexColor(value)) {
        return {
          ok: false,
          error: `颜色值 ${String(value)} 不是合法十六进制（terminal.${key} 支持 #rgb 或 #rrggbb）`,
        };
      }
      terminal[key] = value.trim().toLowerCase();
    }
  }

  return { ok: true, value: { name, kind, colors, terminal } };
}

/**
 * 由校验后的字段组装调色板。
 *
 * 参数：id 为主题 id，value 为校验结果
 * 核心流程：xterm 配色先按内置 dark 种子派生，再用主题里显式给的
 * background/foreground/cursor/selectionBackground 覆盖 -> 缺省永远可用。
 */
export function paletteFromParsed(id: string, value: ParsedTheme, builtin: boolean): ThemePalette {
  const seed = darkSeed();
  const terminal = {
    ...buildTerminal(value.colors, seed, value.kind),
    ...value.terminal,
  };
  return {
    id,
    name: value.name,
    kind: value.kind,
    builtin,
    colors: value.colors,
    terminal,
  };
}

/**
 * 解析并校验一份主题 JSON 文本。
 *
 * 参数：text 为文件原文（或用户粘贴的内容）
 * 核心流程：字节数 -> JSON 可解析 -> 字段校验 -> 生成规范化 payload；
 * 失败时给出具体原因，绝不只报「导入失败」。
 */
export function parseThemeFile(text: string): ThemeImportResult {
  const bytes = byteLength(text);
  if (bytes > THEME_PAYLOAD_MAX_BYTES) {
    return {
      ok: false,
      error: `文件有 ${(bytes / 1024).toFixed(1)} KB，超过 ${THEME_PAYLOAD_MAX_BYTES / 1024} KB 上限`,
    };
  }

  let data: unknown;
  try {
    data = JSON.parse(text);
  } catch (err) {
    return {
      ok: false,
      error: `不是合法的 JSON：${err instanceof Error ? err.message : String(err)}`,
    };
  }

  const checked = validateTheme(data);
  if (!checked.ok) return checked;

  // 规范化：只保留认识的字段，payload 由前端重新序列化
  const payload = JSON.stringify({
    name: checked.value.name,
    kind: checked.value.kind,
    colors: checked.value.colors,
    ...(Object.keys(checked.value.terminal).length > 0
      ? { terminal: checked.value.terminal }
      : {}),
  });
  if (byteLength(payload) > THEME_PAYLOAD_MAX_BYTES) {
    return {
      ok: false,
      error: `规范化后仍有 ${(byteLength(payload) / 1024).toFixed(1)} KB，超过 ${
        THEME_PAYLOAD_MAX_BYTES / 1024
      } KB 上限`,
    };
  }

  return {
    ok: true,
    name: checked.value.name,
    kind: checked.value.kind,
    payload,
    palette: paletteFromParsed("custom:preview", checked.value, false),
  };
}

/**
 * 把后端记录还原成调色板。
 *
 * 参数：record 为 theme_list_custom 的一条记录
 * 核心流程：payload 直接交给 parseThemeFile 做同一套校验（库里的数据也可能被手改）；
 * 解析失败返回 null，调用方跳过这一条并提示。
 */
export function paletteFromRecord(record: CustomThemeRecord): ThemePalette | null {
  const result = parseThemeFile(record.payload);
  if (!result.ok) return null;
  return {
    ...result.palette,
    id: `custom:${record.id}`,
    // 名称以库里那条为准，避免 payload 里的 name 与列表显示不一致
    name: record.name || result.name,
  };
}

/**
 * 由校验结果拼出保存请求。
 *
 * 参数：result 为通过校验的导入结果
 * 核心流程：只带 name/kind/payload 三个字段，id 交给后端生成。
 */
export function toSaveInput(result: Extract<ThemeImportResult, { ok: true }>): CustomThemeInput {
  return { name: result.name, kind: result.kind, payload: result.payload };
}
