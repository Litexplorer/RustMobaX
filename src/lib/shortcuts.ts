/**
 * 快捷键表与键位规范化。
 *
 * 存储形式是一条 JSON 字符串（设置键 shortcuts），键是动作 id，值是 `Ctrl+Shift+X`
 * 这样的组合；这里只做纯函数，不碰 DOM 也不读写存储，方便单测与设置界面复用。
 */

/** 可配置的动作 id，与 App 里的分发器一一对应 */
export type ShortcutActionId =
  | "newSession"
  | "newGroup"
  | "import"
  | "closeTab"
  | "search"
  | "split"
  | "toggleSidebar"
  | "commandPalette"
  | "settings";

/** 动作 id 到组合的映射 */
export type ShortcutMap = Record<ShortcutActionId, string>;

/** 界面展示用：动作名与说明 */
export const SHORTCUT_ACTIONS: { id: ShortcutActionId; label: string; hint: string }[] = [
  { id: "newSession", label: "新建会话", hint: "打开会话设置弹窗" },
  { id: "newGroup", label: "新建分组", hint: "在当前选中的分组下建子分组" },
  { id: "import", label: "导入会话", hint: "导入 MobaXterm 会话文件" },
  { id: "closeTab", label: "关闭标签", hint: "关闭当前激活标签" },
  { id: "search", label: "终端内搜索", hint: "在终端面板里查找文本（由终端处理）" },
  { id: "split", label: "分屏", hint: "当前终端标签左右分屏" },
  { id: "toggleSidebar", label: "显示/隐藏侧栏", hint: "收起或展开左侧栏" },
  { id: "commandPalette", label: "命令面板", hint: "打开命令面板" },
  { id: "settings", label: "首选项", hint: "打开设置弹窗" },
];

/** 默认键位：与管理器里「恢复默认」共用 */
export const DEFAULT_SHORTCUTS: ShortcutMap = {
  newSession: "Ctrl+N",
  newGroup: "Ctrl+Shift+N",
  import: "Ctrl+O",
  closeTab: "Ctrl+W",
  search: "Ctrl+F",
  split: "Ctrl+\\",
  toggleSidebar: "Ctrl+B",
  commandPalette: "Ctrl+Shift+A",
  settings: "Ctrl+,",
};

/** 只按下修饰键时不算组合 */
const MODIFIER_KEYS = new Set(["Control", "Shift", "Alt", "Meta", "OS"]);

/** 需要改名的按键 -> 存储里用的短名 */
const KEY_ALIASES: Record<string, string> = {
  " ": "Space",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  Escape: "Esc",
  Enter: "Enter",
  Backspace: "Backspace",
  Delete: "Delete",
  Tab: "Tab",
  "\\": "\\",
};

/**
 * 把一次 keydown 规范化成存储用的组合字符串。
 *
 * 参数：event 为键盘事件
 * 核心流程：修饰键按 Ctrl/Alt/Shift 顺序排列（mac 的 Meta 归一成 Ctrl，
 * 保证配置跨平台可读），主键取大写字母或短名；只按修饰键返回 null。
 */
export function normalizeCombo(event: KeyboardEvent): string | null {
  if (MODIFIER_KEYS.has(event.key)) return null;
  const parts: string[] = [];
  if (event.ctrlKey || event.metaKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");

  const raw = event.key;
  let key = KEY_ALIASES[raw] ?? raw;
  if (key.length === 1) key = key.toUpperCase();
  else if (key.startsWith("F") && /^F\d{1,2}$/.test(key)) key = key.toUpperCase();
  else key = key.charAt(0).toUpperCase() + key.slice(1);

  // 字母与数字已经是大写/原样，符号保持原样
  parts.push(key);
  return parts.join("+");
}

/**
 * 判断一次按键是否命中某个组合。
 *
 * 参数：event 为键盘事件，combo 为存储里的组合
 * 核心流程：两侧都规范化后比较，避免大小写与修饰键顺序造成误判。
 */
export function matches(event: KeyboardEvent, combo: string): boolean {
  const normalized = normalizeCombo(event);
  return normalized !== null && normalized === combo;
}

/**
 * 把组合格式化成界面文案。
 *
 * 参数：combo 为存储里的组合
 * 核心流程：mac 上把 Ctrl 显示成 ⌘，其余原样用加号连接，方便用户对照。
 */
export function formatCombo(combo: string): string {
  const mac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
  return combo
    .split("+")
    .map((part) => (mac && part === "Ctrl" ? "⌘" : part))
    .join("+");
}

/**
 * 解析设置里的快捷键 JSON。
 *
 * 参数：raw 为后端存回来的字符串
 * 核心流程：解析失败或字段非法就回落缺省；重复绑定时保留先出现的那个，
 * 后面的动作回落到缺省值，并在回执里由设置界面提示冲突。
 */
export function parseShortcuts(raw: string | undefined | null): ShortcutMap {
  const result: ShortcutMap = { ...DEFAULT_SHORTCUTS };
  if (!raw) return result;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return result;
  }
  if (typeof parsed !== "object" || parsed === null) return result;

  const used = new Set<string>();
  for (const action of SHORTCUT_ACTIONS) {
    const value = (parsed as Record<string, unknown>)[action.id];
    if (typeof value !== "string") continue;
    const combo = value.trim();
    if (combo === "" || used.has(combo)) continue;
    result[action.id] = combo;
    used.add(combo);
  }
  return result;
}

/**
 * 序列化快捷键表。
 *
 * 参数：map 为当前键位
 * 核心流程：只写已知动作，顺序稳定，避免库里的 JSON 每次序列化都不一样。
 */
export function serializeShortcuts(map: ShortcutMap): string {
  const ordered: Record<string, string> = {};
  for (const action of SHORTCUT_ACTIONS) {
    ordered[action.id] = map[action.id];
  }
  return JSON.stringify(ordered);
}

/**
 * 找出重复绑定的动作。
 *
 * 参数：map 为当前键位
 * 核心流程：按组合聚合，出现多次的动作 id 全部返回，供设置界面标红。
 */
export function findConflicts(map: ShortcutMap): ShortcutActionId[] {
  const seen = new Map<string, ShortcutActionId[]>();
  for (const action of SHORTCUT_ACTIONS) {
    const combo = map[action.id];
    const list = seen.get(combo) ?? [];
    list.push(action.id);
    seen.set(combo, list);
  }
  const conflicts: ShortcutActionId[] = [];
  for (const ids of seen.values()) {
    if (ids.length > 1) conflicts.push(...ids);
  }
  return conflicts;
}
