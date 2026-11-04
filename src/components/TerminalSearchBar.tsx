import { useEffect, useRef, useState } from "react";

type Props = {
  /** 当前命中序号（0 基），-1 表示还没定位到具体命中 */
  resultIndex: number;
  /** 命中总数 */
  resultCount: number;
  onFind: (query: string, backwards: boolean) => void;
  onClose: () => void;
};

/**
 * 终端面板内的搜索条。
 *
 * 参数：resultIndex/resultCount 为上层从 xterm 搜索插件转来的命中信息，
 * onFind 触发一次查找，onClose 收起搜索条。
 * 核心流程：输入即向前查找 -> 回车或按钮换方向 -> Esc 关闭；
 * 命中数字只做展示，真正的滚动定位由 xterm 搜索插件完成。
 */
export function TerminalSearchBar(props: Props) {
  const { resultIndex, resultCount, onFind, onClose } = props;
  const [query, setQuery] = useState("");
  const inputRef = useRef<HTMLInputElement | null>(null);

  // 打开即聚焦并全选 -> 可以直接敲新关键字覆盖上一次的输入
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const empty = query.trim() === "";
  const counter = empty
    ? ""
    : resultCount === 0
      ? "无匹配"
      : resultIndex < 0
        ? `共 ${resultCount} 个`
        : `${resultIndex + 1}/${resultCount}`;

  return (
    <div className="terminal-search" role="search">
      <input
        ref={inputRef}
        className="terminal-search-input"
        value={query}
        placeholder="在终端中查找"
        aria-label="在终端中查找"
        onChange={(event) => {
          setQuery(event.target.value);
          onFind(event.target.value, false);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            // 先拦住冒泡，避免 Esc 继续往终端或全局处理器走
            event.preventDefault();
            event.stopPropagation();
            onClose();
            return;
          }
          if (event.key === "Enter") {
            event.preventDefault();
            onFind(query, event.shiftKey);
          }
        }}
      />
      <span className="terminal-search-count">{counter}</span>
      <button
        type="button"
        className="terminal-search-button"
        title="上一个 (Shift+Enter)"
        onClick={() => onFind(query, true)}
      >
        上一个
      </button>
      <button
        type="button"
        className="terminal-search-button"
        title="下一个 (Enter)"
        onClick={() => onFind(query, false)}
      >
        下一个
      </button>
      <button
        type="button"
        className="terminal-search-button"
        title="关闭 (Esc)"
        onClick={onClose}
      >
        关闭
      </button>
    </div>
  );
}
