import { useEffect, useMemo, useRef, useState } from "react";
import { filterCommands, type Command } from "../lib/commands";
import { useEscapeClose } from "../lib/useEscapeClose";

interface CommandPaletteProps {
  /** 面板是否可见；false 时整棵子树不渲染 */
  open: boolean;
  /** 全量命令，过滤与执行都发生在这里 */
  commands: Command[];
  /** 请求关闭面板（Esc、执行完一条命令、父层自己关） */
  onClose: () => void;
}

/**
 * 命令面板。
 *
 * 参数：见 CommandPaletteProps
 * 核心流程：打开时清空输入并把焦点放到搜索框；输入实时过滤，↑/↓ 循环移动选中，
 * Enter 执行、Esc 关闭、鼠标悬停即选中、点击即执行；
 * 执行顺序是「先关面板再跑命令」，这样异步动作（如打开弹窗、连远程）不会被面板盖住；
 * 面板打开期间在捕获阶段拦下 keydown，避免 App 的全局快捷键再收到一次。
 * 打开前的焦点元素记在 ref 里，Esc 关闭时同步还给；执行命令而关闭时只在 run()
 * 之后没有任何弹窗接管焦点的情况下才还，否则会把焦点从新弹窗抢走。
 */
export function CommandPalette({ open, commands, onClose }: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  /** 面板根节点，用来判断记录的焦点元素是不是面板自己（那不能还） */
  const panelRef = useRef<HTMLDivElement | null>(null);
  /** 打开面板前的焦点元素；关闭时由 restoreFocus 消费并清空 */
  const returnFocusRef = useRef<HTMLElement | null>(null);

  /**
   * 把键盘焦点还给打开面板前的元素。
   *
   * 参数：无
   * 核心流程：目标仍在文档里且不在面板内部就直接 focus；否则（原元素随 rerender
   * 被换掉、标签已关）退化为聚焦终端隐藏的 .xterm-helper-textarea，
   * 保证用户能立刻继续打字，而不是焦点落在 body 上。
   */
  const restoreFocus = () => {
    const previous = returnFocusRef.current;
    returnFocusRef.current = null;
    const panel = panelRef.current;
    if (previous && previous.isConnected && !panel?.contains(previous)) {
      previous.focus();
      return;
    }
    document.querySelector<HTMLElement>(".xterm-helper-textarea")?.focus();
  };

  const results = useMemo(() => filterCommands(commands, query), [commands, query]);
  /** 结果变短后旧下标可能越界，这里收回到范围；空列表用 -1 表示没有选中项 */
  const activeIndex = results.length === 0 ? -1 : Math.min(active, results.length - 1);

  /**
   * 关闭面板：先把焦点还给打开前的元素再真正关闭。
   *
   * 参数：无
   * 核心流程：同步还原焦点——此刻面板还在文档里，下一帧才卸载，focus 不会被卸载打断。
   */
  const closeWithFocusRestore = () => {
    restoreFocus();
    onClose();
  };

  // Esc = 关闭面板：统一走全局弹窗栈，本面板打开时 Esc 只关面板，
  // 不会连带关掉它下面那层弹窗；面板不开时（open 为 false）不入栈
  useEscapeClose(closeWithFocusRestore, open);

  // 每次打开都复位：记下当前焦点、清空输入、选中第一条、把焦点推给输入框
  useEffect(() => {
    if (!open) return;
    // 必须在本 effect 里取：输入框的 focus 在下面的 setTimeout 里才发生
    const previous = document.activeElement;
    returnFocusRef.current = previous instanceof HTMLElement ? previous : null;
    setQuery("");
    setActive(0);
    const timer = window.setTimeout(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    }, 0);
    return () => window.clearTimeout(timer);
  }, [open]);

  // 选中项始终留在可视区（结果一多，键盘移动不能跑出滚动的列表）
  useEffect(() => {
    if (activeIndex < 0) return;
    itemRefs.current[activeIndex]?.scrollIntoView({ block: "nearest" });
  }, [activeIndex, results]);

  /**
   * 移动选中项，到顶/到底循环。
   *
   * 参数：delta 为 +1（下）或 -1（上）
   * 核心流程：先按当前结果数量取模，保证从最后一条按 ↓ 回到第一条。
   */
  const move = (delta: number) => {
    if (results.length === 0) return;
    setActive((prev) => {
      const current = Math.min(Math.max(prev, 0), results.length - 1);
      return (current + delta + results.length) % results.length;
    });
  };

  /**
   * 执行一条命令。
   *
   * 参数：command 为选中的命令
   * 核心流程：先关面板再调用 run()，命令抛错只记 console.error，
   * 不弹原生 alert（面板已关，弹窗会打断用户，控制台足够定位）。
   */
  const runCommand = async (command: Command) => {
    try {
      await command.run();
    } catch (error) {
      console.error(`命令执行失败：${command.id}`, error);
    }
  };

  const execute = (command: Command) => {
    onClose();
    // 关面板是同步的，run 在这一帧之后才落地，避免被遮挡
    void (async () => {
      await runCommand(command);
      // 命令可能打开了别的弹窗（新会话 / 设置 / 密码提示…）。等一帧让弹窗自己的
      // focus 落地，只有「没人接管焦点」时才归还，避免把焦点从新弹窗抢回终端。
      window.setTimeout(() => {
        const current = document.activeElement;
        if (current && current !== document.body) return;
        if (document.querySelector("[role='dialog']")) return;
        restoreFocus();
      }, 0);
    })();
  };

  /**
   * 面板内的键盘处理。
   *
   * 参数：event 为 React 键盘事件
   * 核心流程：先 stopPropagation 把事件按住不往上传（App 的全局监听在 window 捕获阶段），
   * 再按 ↑/↓/Enter 分发；Esc 关闭已交给 useEscapeClose（全局弹窗栈），
   * 输入法组字中的 Enter 让给输入法。
   */
  const onKeyDownCapture = (event: React.KeyboardEvent<HTMLDivElement>) => {
    event.stopPropagation();
    const native = event.nativeEvent;
    if (native.isComposing) return;

    if (event.key === "ArrowDown") {
      event.preventDefault();
      move(1);
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      move(-1);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const picked = activeIndex >= 0 ? results[activeIndex] : undefined;
      if (picked) execute(picked);
    }
  };

  if (!open) return null;

  return (
    <div className="modal-backdrop" role="presentation">
      {/* 遮罩点击不关闭，与项目其它弹窗一致；关闭只有 Esc / 执行命令 */}
      <div
        ref={panelRef}
        className="modal palette"
        role="dialog"
        aria-label="命令面板"
        onKeyDownCapture={onKeyDownCapture}
      >
        <input
          ref={inputRef}
          className="palette-input"
          type="text"
          autoComplete="off"
          spellCheck={false}
          value={query}
          placeholder="输入命令名，回车执行"
          aria-label="搜索命令"
          onChange={(event) => {
            setQuery(event.target.value);
            // 每敲一个字都回到第一条，避免选中还停在已消失的下标上
            setActive(0);
          }}
        />
        <div className="palette-list" role="listbox" aria-label="命令列表">
          {results.map((command, index) => {
            const isActive = index === activeIndex;
            return (
              <button
                key={command.id}
                ref={(element) => {
                  itemRefs.current[index] = element;
                }}
                type="button"
                role="option"
                aria-selected={isActive}
                className={`palette-option ${isActive ? "is-active" : ""}`}
                onMouseEnter={() => setActive(index)}
                onClick={() => execute(command)}
              >
                <span className="palette-option-titles">
                  <span className="palette-option-title">{command.title}</span>
                  <span className="palette-option-title-en">{command.titleEn}</span>
                </span>
                <span className="palette-option-meta">
                  <span className="palette-option-group">{command.group}</span>
                  {command.hint && <span className="palette-option-hint">{command.hint}</span>}
                </span>
              </button>
            );
          })}
          {results.length === 0 && (
            <p className="palette-empty">没有匹配的命令 / No matching command</p>
          )}
        </div>
      </div>
    </div>
  );
}
