import type { ITheme } from "@xterm/xterm";

/**
 * 主题注册表。
 *
 * 设计要点：
 * 1. 一套主题只描述 11 个基础色（和导入契约里的 colors 完全同构），
 *    其余约 20 个派生色（输入框底、悬浮底、强调软色、协议色……）由基础色算出，
 *    所以新增一套主题不需要再抄一遍派生色。
 * 2. 运行时把颜色写成 document.documentElement 上的 CSS 变量，
 *    组件只认 var(--xxx) -> ui.scale、分屏、停靠等布局逻辑完全不受影响。
 * 3. App.css 里 :root[data-theme="dark"|"light"] 的两套变量保留作为首帧兜底。
 */

/** 11 个基础色键：导入契约里的 colors 就是这 11 个，缺一个都不合法 */
export type CssVarKey =
  | "bg"
  | "bgChrome"
  | "bgPanel"
  | "bgHover"
  | "bgActive"
  | "border"
  | "borderStrong"
  | "text"
  | "textDim"
  | "accent"
  | "danger";

/** 基础色键的固定顺序：导入校验与提示文案都按它走 */
export const CSS_VAR_KEYS: CssVarKey[] = [
  "bg",
  "bgChrome",
  "bgPanel",
  "bgHover",
  "bgActive",
  "border",
  "borderStrong",
  "text",
  "textDim",
  "accent",
  "danger",
];

/** 基础色键到 CSS 变量名的唯一映射 */
export const CSS_VAR_OF: Record<CssVarKey, string> = {
  bg: "--bg",
  bgChrome: "--bg-chrome",
  bgPanel: "--bg-panel",
  bgHover: "--bg-hover",
  bgActive: "--bg-active",
  border: "--border",
  borderStrong: "--border-strong",
  text: "--text",
  textDim: "--text-dim",
  accent: "--accent",
  danger: "--danger",
};

/** 主题明暗种类：决定 data-theme、color-scheme 和图标滤镜 */
export type ThemeKind = "dark" | "light";

/** 派生色变量名：都由基础色算出来，导入契约里不出现 */
export type ExtraVarKey =
  | "--bg-input"
  | "--bg-raised"
  | "--bg-tab"
  | "--bg-sunken"
  | "--bg-chip"
  | "--text-soft"
  | "--text-faint"
  | "--on-accent"
  | "--btn-hover"
  | "--accent-soft"
  | "--accent-line"
  | "--danger-bg"
  | "--danger-border"
  | "--danger-soft"
  | "--control-border"
  | "--overlay"
  | "--shadow-menu"
  | "--proto-ssh"
  | "--proto-sftp"
  | "--proto-local"
  | "--warning";

/** 一套主题的内存形态 */
export type ThemePalette = {
  id: string;
  name: string;
  kind: ThemeKind;
  builtin: boolean;
  colors: Record<CssVarKey, string>;
  terminal: ITheme;
};

/** 自定义主题 id 前缀：设置里写成 custom:<uuid> */
export const CUSTOM_THEME_PREFIX = "custom:";

/** 首帧预读用的 localStorage 键，index.html 的内联脚本读同一份 */
export const THEME_CACHE_KEY = "rustmobax.themeCache";

/** 判断是否是自定义主题 id（只做前缀判断，uuid 形态由后端保证） */
export function isCustomThemeId(id: string): boolean {
  return id.startsWith(CUSTOM_THEME_PREFIX);
}

/** #rgb / #rrggbb，大小写都收；导出给导入校验复用，避免两处正则不一致 */
const HEX_COLOR = /^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/;

/** 判断颜色串是否是合法的十六进制色值 */
export function isHexColor(value: unknown): value is string {
  return typeof value === "string" && HEX_COLOR.test(value.trim());
}

/**
 * 十六进制色转 RGB 三元组。
 *
 * 参数：hex 为已校验的 #rgb / #rrggbb
 * 核心流程：#rgb 先扩成 #rrggbb，再按两位切出通道值。
 */
function toRgb(hex: string): [number, number, number] {
  let body = hex.trim().replace("#", "");
  if (body.length === 3) {
    body = body
      .split("")
      .map((char) => char + char)
      .join("");
  }
  return [
    Number.parseInt(body.slice(0, 2), 16),
    Number.parseInt(body.slice(2, 4), 16),
    Number.parseInt(body.slice(4, 6), 16),
  ];
}

/**
 * RGB 三元组转 #rrggbb。
 *
 * 参数：rgb 为 0~255 的三个通道
 * 核心流程：先夹进区间再补零，避免浮点取整溢出成三位。
 */
function toHex(rgb: [number, number, number]): string {
  const channel = (value: number) =>
    Math.min(255, Math.max(0, Math.round(value)))
      .toString(16)
      .padStart(2, "0");
  return `#${channel(rgb[0])}${channel(rgb[1])}${channel(rgb[2])}`;
}

/**
 * 两色线性混合。
 *
 * 参数：from/to 为两个十六进制色，amount 为 from -> to 的比例（0~1）
 * 核心流程：逐通道插值后回落成十六进制，派生色全部由它搭出来。
 */
export function mix(from: string, to: string, amount: number): string {
  const t = Math.min(1, Math.max(0, amount));
  const a = toRgb(from);
  const b = toRgb(to);
  return toHex([
    a[0] + (b[0] - a[0]) * t,
    a[1] + (b[1] - a[1]) * t,
    a[2] + (b[2] - a[2]) * t,
  ]);
}

/** 把十六进制色转成带透明度的 rgba()，给描边、蒙层用 */
function rgba(hex: string, alpha: number): string {
  const [r, g, b] = toRgb(hex);
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

/**
 * 色值对比度（WCAG 相对亮度公式）。
 *
 * 参数：a/b 为两个十六进制色
 * 核心流程：先按 sRGB 定义把通道线性化，再按 (L亮 + 0.05) / (L暗 + 0.05) 求比值；
 * 导出它是为了让设置弹窗/验算脚本能对任何主题（含用户导入的）算同一套数。
 */
export function contrastRatio(a: string, b: string): number {
  const luminance = (hex: string) => {
    const [r, g, bl] = toRgb(hex).map((value) => {
      const channel = value / 255;
      return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * r + 0.7152 * g + 0.0722 * bl;
  };
  const la = luminance(a);
  const lb = luminance(b);
  const light = Math.max(la, lb);
  const dark = Math.min(la, lb);
  return Math.round(((light + 0.05) / (dark + 0.05)) * 100) / 100;
}

/**
 * 由基础色派生约 20 个扩展变量。
 *
 * 参数：colors 为 11 个基础色，kind 决定往黑还是往白推，
 *       terminal 提供绿/黄（协议色、警告色复用终端配色）
 * 核心流程：暗色一律「底色压暗、面板提亮」，浅色反过来，
 * 保证同一套公式在两种明暗方向上都得到可分辨的层次。
 */
function deriveExtras(
  colors: Record<CssVarKey, string>,
  kind: ThemeKind,
  terminal: ITheme,
): Record<ExtraVarKey, string> {
  const dark = kind === "dark";
  const green = terminal.green ?? colors.text;
  const yellow = terminal.yellow ?? colors.text;
  const protoLocal = dark ? yellow : mix(yellow, "#000000", 0.3);

  return {
    "--bg-input": dark ? mix(colors.bg, "#000000", 0.22) : mix(colors.bg, "#ffffff", 0.7),
    "--bg-raised": dark ? mix(colors.bgPanel, "#ffffff", 0.08) : mix(colors.bgPanel, "#ffffff", 0.6),
    "--bg-tab": dark ? mix(colors.bg, "#000000", 0.12) : mix(colors.bg, "#000000", 0.05),
    "--bg-sunken": dark ? mix(colors.bg, "#000000", 0.08) : mix(colors.bg, "#ffffff", 0.5),
    "--bg-chip": dark ? mix(colors.bgPanel, "#ffffff", 0.14) : mix(colors.bg, "#000000", 0.07),
    "--text-soft": dark ? mix(colors.text, "#ffffff", 0.18) : mix(colors.text, "#ffffff", 0.18),
    "--text-faint": dark
      ? mix(colors.textDim, colors.bg, 0.5)
      : mix(colors.textDim, colors.bg, 0.22),
    "--on-accent": dark ? mix(colors.accent, "#000000", 0.9) : "#ffffff",
    "--btn-hover": dark ? mix(colors.bgHover, "#ffffff", 0.1) : mix(colors.bg, "#000000", 0.07),
    "--accent-soft": mix(colors.bg, colors.accent, dark ? 0.22 : 0.14),
    "--accent-line": rgba(colors.accent, dark ? 0.35 : 0.3),
    "--danger-bg": mix(colors.bg, colors.danger, dark ? 0.18 : 0.12),
    "--danger-border": mix(colors.bg, colors.danger, dark ? 0.32 : 0.35),
    "--danger-soft": dark ? mix(colors.danger, "#ffffff", 0.5) : mix(colors.danger, "#000000", 0.25),
    "--control-border": dark ? colors.border : colors.borderStrong,
    "--overlay": rgba(colors.bg, 0.92),
    "--shadow-menu": dark
      ? "0 10px 24px rgba(0, 0, 0, 0.45)"
      : "0 10px 24px rgba(15, 23, 42, 0.14)",
    "--proto-ssh": dark ? green : mix(green, "#000000", 0.25),
    "--proto-sftp": dark ? mix(colors.accent, "#ffffff", 0.3) : mix(colors.accent, "#000000", 0.15),
    "--proto-local": protoLocal,
    "--warning": protoLocal,
  };
}

/**
 * 内置暗/浅两套的派生色：沿用改造前 App.css 里的精确取值。
 *
 * 参数：无（只读常量）
 * 核心流程：这两套是缺省外观，走公式会带来肉眼可见的细微偏差，
 * 所以固定成原值 -> scale=1 时视觉与改造前完全一致；
 * 其余主题（含用户导入的）一律走 deriveExtras 的公式。
 */
const LEGACY_EXTRAS: Record<string, Record<ExtraVarKey, string>> = {
  dark: {
    "--bg-input": "#14161b",
    "--bg-raised": "#22262e",
    "--bg-tab": "#1a1d23",
    "--bg-sunken": "#191c22",
    "--bg-chip": "#262b34",
    "--text-soft": "#a8b0bd",
    "--text-faint": "#4c525d",
    "--on-accent": "#0b1220",
    "--btn-hover": "#2b303a",
    "--accent-soft": "#223247",
    "--accent-line": "rgba(74, 158, 255, 0.35)",
    "--danger-bg": "#3a2020",
    "--danger-border": "#4a2b2b",
    "--danger-soft": "#ffb3b3",
    "--control-border": "#2a2e37",
    "--overlay": "rgba(22, 24, 29, 0.92)",
    "--shadow-menu": "0 10px 24px rgba(0, 0, 0, 0.45)",
    "--proto-ssh": "#6ee7a8",
    "--proto-sftp": "#9db6ff",
    "--proto-local": "#e8c07a",
    "--warning": "#e8c07a",
  },
  light: {
    "--bg-input": "#ffffff",
    "--bg-raised": "#ffffff",
    "--bg-tab": "#e9ecf1",
    "--bg-sunken": "#f7f8fa",
    "--bg-chip": "#e3e6eb",
    "--text-soft": "#3f4753",
    "--text-faint": "#656c78",
    "--on-accent": "#ffffff",
    "--btn-hover": "#dfe3ea",
    "--accent-soft": "#dbe8fb",
    "--accent-line": "rgba(22, 103, 200, 0.3)",
    "--danger-bg": "#fbe3e1",
    "--danger-border": "#e5a9a3",
    "--danger-soft": "#8c1d18",
    "--control-border": "#8f97a4",
    "--overlay": "rgba(245, 246, 248, 0.92)",
    "--shadow-menu": "0 10px 24px rgba(15, 23, 42, 0.14)",
    "--proto-ssh": "#0f7a4d",
    "--proto-sftp": "#2b5cbf",
    "--proto-local": "#8a5a00",
    "--warning": "#8a5a00",
  },
};

/**
 * 终端配色的种子：每套主题只写 4 个社区色 + 少量覆盖。
 *
 * 参数：green/yellow/magenta/cyan 来自各主题官方配色
 * 核心流程：其余 ansi 色由基础色推出（见 buildTerminal），不需要手写 16 色。
 */
type AnsiSeed = {
  green: string;
  yellow: string;
  magenta: string;
  cyan: string;
  /** 少数主题的选区/黑白色与公式不一致时在此覆盖 */
  overrides?: Partial<ITheme>;
};

/** 对外暴露种子类型：导入自定义主题时要用内置种子兜底 */
export type { AnsiSeed };

/**
 * 从基础色派生 xterm 配色。
 *
 * 参数：colors 为 11 个基础色，seed 提供 4 个社区色，kind 决定亮色变体往哪推
 * 核心流程：背景/前景/光标/选区直接取基础色；黑白色按明暗方向取面板/正文；
 * 亮色（bright*）由同色相往反方向混合得到 -> 16 色齐备且不用逐套手写。
 */
export function buildTerminal(
  colors: Record<CssVarKey, string>,
  seed: AnsiSeed,
  kind: ThemeKind,
): ITheme {
  const dark = kind === "dark";
  const brighter = (value: string) => (dark ? mix(value, "#ffffff", 0.22) : mix(value, "#000000", 0.16));
  return {
    background: colors.bg,
    foreground: colors.text,
    cursor: colors.accent,
    cursorAccent: colors.bg,
    selectionBackground: colors.bgActive,
    black: dark ? colors.bgPanel : colors.text,
    red: colors.danger,
    green: seed.green,
    yellow: seed.yellow,
    blue: colors.accent,
    magenta: seed.magenta,
    cyan: seed.cyan,
    white: dark ? colors.text : colors.textDim,
    brightBlack: dark ? colors.textDim : mix(colors.textDim, "#000000", 0.2),
    brightRed: brighter(colors.danger),
    brightGreen: brighter(seed.green),
    brightYellow: brighter(seed.yellow),
    brightBlue: brighter(colors.accent),
    brightMagenta: brighter(seed.magenta),
    brightCyan: brighter(seed.cyan),
    brightWhite: dark ? mix(colors.text, "#ffffff", 0.15) : colors.text,
    ...seed.overrides,
  };
}

/**
 * 组装一套主题。
 *
 * 参数：id/name/kind 为元信息，colors 为 11 个基础色，seed 为终端社区色
 * 核心流程：先把终端配色派生出来（派生扩展色要用到绿/黄），再交给注册表。
 */
function makeTheme(
  id: string,
  name: string,
  kind: ThemeKind,
  colors: Record<CssVarKey, string>,
  seed: AnsiSeed,
): ThemePalette {
  return { id, name, kind, builtin: true, colors, terminal: buildTerminal(colors, seed, kind) };
}

/** 内置主题的固定顺序，设置弹窗按这个顺序排 */
export const BUILTIN_THEMES: ThemePalette[] = [
  makeTheme(
    "dark",
    "暗色",
    "dark",
    {
      bg: "#16181d",
      bgChrome: "#1b1e24",
      bgPanel: "#1c1f26",
      bgHover: "#23272f",
      bgActive: "#2b3f57",
      border: "#2a2e37",
      borderStrong: "#3c424e",
      text: "#d7dae0",
      textDim: "#8b919d",
      accent: "#4a9eff",
      danger: "#ff6b6b",
    },
    { green: "#6ee7a8", yellow: "#e8c07a", magenta: "#c792ea", cyan: "#89ddff" },
  ),
  makeTheme(
    "light",
    "浅色",
    "light",
    {
      bg: "#f5f6f8",
      bgChrome: "#eceef2",
      bgPanel: "#ffffff",
      bgHover: "#e7eaef",
      bgActive: "#d3e2fb",
      border: "#cbd0d9",
      borderStrong: "#8f97a4",
      text: "#1b1f27",
      textDim: "#4d5563",
      accent: "#1667c8",
      danger: "#b3261e",
    },
    {
      green: "#0f7a4d",
      yellow: "#8a5a00",
      magenta: "#7c3aed",
      cyan: "#0e7490",
      // 浅色的选区比 bgActive 更淡一档，和改造前一致
      overrides: { selectionBackground: "#cfe0f8" },
    },
  ),
  makeTheme(
    "island-dark",
    "Island Dark",
    "dark",
    {
      bg: "#1e1f22",
      bgChrome: "#2b2d30",
      bgPanel: "#2b2d30",
      bgHover: "#35373b",
      bgActive: "#3574f0",
      border: "#393b40",
      borderStrong: "#4b4e57",
      text: "#dfe1e5",
      textDim: "#9da0a8",
      accent: "#3574f0",
      danger: "#f75464",
    },
    { green: "#5fb865", yellow: "#e3b341", magenta: "#b9a6f2", cyan: "#4bb3c4" },
  ),
  makeTheme(
    "tokyonight",
    "Tokyo Night",
    "dark",
    {
      bg: "#1a1b26",
      bgChrome: "#16161e",
      bgPanel: "#1f2335",
      bgHover: "#292e42",
      bgActive: "#3b4261",
      border: "#292e42",
      borderStrong: "#3b4261",
      text: "#c0caf5",
      textDim: "#7f8ab0",
      accent: "#7aa2f7",
      danger: "#f7768e",
    },
    { green: "#9ece6a", yellow: "#e0af68", magenta: "#bb9af7", cyan: "#7dcfff" },
  ),
  makeTheme(
    "one-dark",
    "One Dark",
    "dark",
    {
      bg: "#282c34",
      bgChrome: "#21252b",
      bgPanel: "#2c313a",
      bgHover: "#333842",
      bgActive: "#3e4451",
      border: "#3e4451",
      borderStrong: "#4b5263",
      text: "#abb2bf",
      textDim: "#7f848e",
      accent: "#61afef",
      danger: "#e06c75",
    },
    { green: "#98c379", yellow: "#e5c07b", magenta: "#c678dd", cyan: "#56b6c2" },
  ),
];

/** 内置主题 id 集合：设置解析、删除回落都要判断 */
export const BUILTIN_THEME_IDS: string[] = BUILTIN_THEMES.map((theme) => theme.id);

/**
 * 按 id 取内置主题。
 *
 * 参数：id 为主题 id
 * 核心流程：线性查表（内置只有 5 套，不值得建 Map）；查不到返回 undefined。
 */
export function getTheme(id: string): ThemePalette | undefined {
  return BUILTIN_THEMES.find((theme) => theme.id === id);
}

/**
 * 把主题展开成「CSS 变量名 -> 色值」的完整表。
 *
 * 参数：palette 为任意主题（内置或导入）
 * 核心流程：先摊平 11 个基础色，再叠加派生色；
 * 内置暗/浅用历史精确值，其余走公式。
 */
export function themeToCssVars(palette: ThemePalette): Record<string, string> {
  const vars: Record<string, string> = {};
  for (const key of CSS_VAR_KEYS) {
    vars[CSS_VAR_OF[key]] = palette.colors[key];
  }
  const extras = LEGACY_EXTRAS[palette.id] ?? deriveExtras(palette.colors, palette.kind, palette.terminal);
  return Object.assign(vars, extras);
}

/**
 * 取主题的 xterm 配色。
 *
 * 参数：palette 为主题
 * 核心流程：配色在注册表构造时就已派生好，这里只做一次浅拷贝，
 * 避免调用方（xterm）就地改到注册表里的共享对象。
 */
export function themeToXterm(palette: ThemePalette): ITheme {
  return { ...palette.terminal };
}

/**
 * 把主题写到 DOM 上。
 *
 * 参数：palette 为要应用的主题
 * 核心流程：逐个 setProperty 覆盖约 30 个变量 -> 组件无需重渲染即可换肤；
 * 同时写 data-theme（明暗，驱动图标滤镜与兜底变量）、
 * data-theme-id（实际主题，排查用）与 color-scheme（滚动条/表单控件跟随）。
 */
export function applyTheme(palette: ThemePalette): void {
  const root = document.documentElement;
  for (const [name, value] of Object.entries(themeToCssVars(palette))) {
    root.style.setProperty(name, value);
  }
  root.dataset.theme = palette.kind;
  root.dataset.themeId = palette.id;
  root.style.colorScheme = palette.kind;
}

/**
 * 写首帧缓存。
 *
 * 参数：palette 为刚应用的主题
 * 核心流程：把 id / 明暗 / 11 个基础色存进 localStorage，
 * index.html 的内联脚本读同一份结构先上色 -> 浅色用户不会闪一帧暗色；
 * localStorage 不可用（隐私模式）时静默跳过，首帧退回 App.css 的暗色兜底。
 */
export function cacheTheme(palette: ThemePalette): void {
  try {
    window.localStorage.setItem(
      THEME_CACHE_KEY,
      JSON.stringify({ id: palette.id, kind: palette.kind, colors: palette.colors }),
    );
  } catch {
    // 存不下就不存，功能上只影响首帧观感
  }
}
