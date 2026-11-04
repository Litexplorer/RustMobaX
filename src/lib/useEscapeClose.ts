import { useEffect, useRef } from "react";

/** 栈里的一项：token 是实例的唯一标识，close 用 ref 取最新回调 */
type Entry = {
  token: object;
  close: () => void;
};

/**
 * 全局弹窗栈。
 *
 * 按挂载顺序入栈，后挂载的（视觉上盖在上面的）在栈顶；
 * Esc 只交给栈顶那一项，所以「弹窗里再叠弹窗」时天然只关最上层。
 */
const stack: Entry[] = [];

/** window 上是否已经挂上了那个唯一的监听：栈空即摘，避免常驻抢键 */
let listening = false;

/**
 * 唯一的 Esc 监听（window 捕获阶段）。
 *
 * 参数：event 为原生键盘事件
 * 核心流程：只认不带修饰键的 Escape → 有栈就拦下事件并调用栈顶的 close。
 * 在捕获阶段掐断传播，事件不会继续送到 xterm 的隐藏 textarea 或其它监听；
 * 同时 preventDefault，让 App 里那些先看 defaultPrevented 的全局快捷键直接放过。
 */
function handleWindowKeyDown(event: KeyboardEvent): void {
  if (event.key !== "Escape") return;
  if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
  // 输入法组字中的 Esc 属于输入法，不抢
  if (event.isComposing) return;
  const top = stack[stack.length - 1];
  if (!top) return;
  event.preventDefault();
  event.stopPropagation();
  top.close();
}

function startListening(): void {
  if (listening) return;
  window.addEventListener("keydown", handleWindowKeyDown, true);
  listening = true;
}

function stopListening(): void {
  if (!listening) return;
  window.removeEventListener("keydown", handleWindowKeyDown, true);
  listening = false;
}

/**
 * 入栈。
 *
 * 参数：entry 为本实例的栈项
 * 核心流程：同一实例重复入栈时先摘掉旧项再压到栈顶，
 * 保证不会有重复项残留（重复项出栈时会漏删，导致 Esc 落到已经关掉的弹窗上）。
 */
function pushEntry(entry: Entry): void {
  const existing = stack.indexOf(entry);
  if (existing !== -1) stack.splice(existing, 1);
  stack.push(entry);
  startListening();
}

/**
 * 出栈。
 *
 * 参数：entry 为本实例的栈项
 * 核心流程：从栈里摘掉自己（允许中途出栈，不必在栈顶），栈空了就摘掉监听。
 */
function popEntry(entry: Entry): void {
  const index = stack.indexOf(entry);
  if (index !== -1) stack.splice(index, 1);
  if (stack.length === 0) stopListening();
}

/**
 * 让弹窗支持 Esc 关闭，且多弹窗层叠时只关最上层。
 *
 * 参数：onClose 为关闭回调（Esc 等价于点「取消」/「×」），
 * enabled 为 false 时不入栈（弹窗照样渲染，但 Esc 不归它管，
 * 例如设置弹窗正在录制快捷键、请求正在飞的时候）
 * 核心流程：enabled 期间入栈 → 弹窗卸载或 enabled 变 false 时出栈；
 * 回调存 ref，父层每次渲染换新函数也不会反复增删监听。
 */
export function useEscapeClose(onClose: () => void, enabled: boolean = true): void {
  const closeRef = useRef(onClose);
  const entryRef = useRef<Entry | null>(null);

  // 每次渲染都同步最新回调，避免因为回调身份变化而重挂栈项
  useEffect(() => {
    closeRef.current = onClose;
  });

  useEffect(() => {
    if (!enabled) return undefined;
    const entry: Entry = { token: {}, close: () => closeRef.current() };
    entryRef.current = entry;
    pushEntry(entry);
    return () => {
      entryRef.current = null;
      popEntry(entry);
    };
  }, [enabled]);
}
