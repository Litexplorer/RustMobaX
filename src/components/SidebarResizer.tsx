import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent } from "react";

/** 键盘微调步长（px） */
const KEY_STEP = 16;

type Props = {
  /** 当前侧栏宽度（px） */
  width: number;
  /** 宽度下界 */
  min: number;
  /** 宽度上界 */
  max: number;
  /** 拖动过程中持续回调，只改界面不写库 */
  onDrag: (width: number) => void;
  /** 松手或键盘微调后提交，由上层落库 */
  onCommit: (width: number) => void;
};

/**
 * 把宽度夹进区间。
 *
 * 参数：value 为期望宽度，min/max 为闭区间端点
 * 核心流程：取整后夹紧，保证拖动到边界时数值稳定、不会来回抖。
 */
function clampWidth(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, Math.round(value)));
}

/**
 * 侧栏右边缘的拖动条。
 *
 * 参数：见 Props
 * 核心流程：7px 的命中区里画 1px 竖线，hover 与聚焦时线变成强调色；
 * 拖动时按「起点宽度 + 指针位移」算新宽度，只回调 onDrag 更新界面，
 * 松手才 onCommit 落库（避免每次移动都写设置）；
 * 拖动期间给 body 加类，禁掉文本选中并把光标固定成 col-resize；
 * 聚焦后可用左右方向键微调，键盘用户也能改宽度。
 */
export function SidebarResizer({ width, min, max, onDrag, onCommit }: Props) {
  const [dragging, setDragging] = useState(false);
  /** 拖动起点：指针横坐标与当时的宽度 */
  const startRef = useRef<{ x: number; width: number } | null>(null);
  /** 松手时要提交的宽度 -> 放 ref 里，监听器不必每次移动都重绑 */
  const pendingRef = useRef<number | null>(null);

  // 拖动期间禁掉文本选中：挂在 body 上，指针滑到终端上方也不会选中终端文字
  useEffect(() => {
    if (!dragging) return undefined;
    document.body.classList.add("is-resizing-sidebar");
    return () => document.body.classList.remove("is-resizing-sidebar");
  }, [dragging]);

  /** 按下拖动条：记下起点并进入拖动态 */
  const handlePointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    // preventDefault 会吃掉默认聚焦行为 -> 手动补上，拖完还能用方向键微调
    event.currentTarget.focus({ preventScroll: true });
    startRef.current = { x: event.clientX, width };
    pendingRef.current = width;
    setDragging(true);
  };

  // 移动与松开挂在 window 上 -> 指针跑到终端上方也不会丢事件；
  // 不用 setPointerCapture，在 WebView 里更稳
  useEffect(() => {
    if (!dragging) return undefined;
    const handleMove = (event: PointerEvent) => {
      const start = startRef.current;
      if (!start) return;
      const next = clampWidth(start.width + (event.clientX - start.x), min, max);
      pendingRef.current = next;
      onDrag(next);
    };
    const handleUp = () => {
      const next = pendingRef.current;
      pendingRef.current = null;
      startRef.current = null;
      setDragging(false);
      if (next !== null) onCommit(next);
    };
    window.addEventListener("pointermove", handleMove);
    window.addEventListener("pointerup", handleUp);
    window.addEventListener("pointercancel", handleUp);
    return () => {
      window.removeEventListener("pointermove", handleMove);
      window.removeEventListener("pointerup", handleUp);
      window.removeEventListener("pointercancel", handleUp);
    };
  }, [dragging, min, max, onDrag, onCommit]);

  /** 键盘微调：焦点在拖动条上时用左右方向键改宽度并立刻落库 */
  const handleKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const grow = event.key === "ArrowRight";
    const next = clampWidth(width + (grow ? KEY_STEP : -KEY_STEP), min, max);
    if (next !== width) onCommit(next);
  };

  return (
    <div
      className={`sidebar-resizer ${dragging ? "is-dragging" : ""}`}
      role="separator"
      aria-orientation="vertical"
      aria-label="调整侧栏宽度，聚焦后可用左右方向键微调"
      aria-valuenow={width}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      onPointerDown={handlePointerDown}
      onKeyDown={handleKeyDown}
    />
  );
}
