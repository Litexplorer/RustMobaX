import { useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import type { Session, SessionGroup } from "../api/session";
import { filterSessions, groupPath } from "../lib/tree";
import { IconFolder, IconSearch, IconServer, IconTerminal } from "./icons";

/** 列表最多渲染的条数：再多就靠滚动翻侧栏，首界面不做无限长列表 */
const MAX_RESULTS = 50;

interface HomeSessionSearchProps {
  /** 待搜索会话，与侧栏同源（App 的 sessions state） */
  sessions: Session[];
  /** 全部分组：把 session.groupId 还原成可搜索、可展示的路径 */
  groups: SessionGroup[];
  /** 打开一条会话；标签与认证逻辑都在 App 的 openSession 里，这里不重复 */
  onOpen: (session: Session) => void;
}

/**
 * 首界面（还没打开任何标签时）正中的会话搜索框。
 *
 * 参数：见 HomeSessionSearchProps
 * 核心流程：输入即过滤（空关键字不展示列表，避免首界面一进来铺满整屏）；
 * ↑/↓ 循环移动选中项、Enter 打开、Esc 清空并保持聚焦、鼠标悬停同步选中、点击即打开；
 * 键盘事件在捕获阶段拦下并 stopPropagation，避免与 App 的全局快捷键重复响应；
 * 挂载后自动把焦点放进输入框（setTimeout(0) + cleanup，StrictMode 双挂载也不会留下野定时器）；
 * 打开会话后清空关键字，其余动作（连远程、开标签）全部交给 onOpen。
 */
export function HomeSessionSearch({ sessions, groups, onOpen }: HomeSessionSearchProps) {
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const itemRefs = useRef<(HTMLLIElement | null)[]>([]);

  const results = useMemo(
    () => filterSessions(sessions, groups, query),
    [sessions, groups, query],
  );
  const visible = results.slice(0, MAX_RESULTS);
  /** 结果变短后旧下标可能越界，这里收回到范围内；空列表用 -1 表示没有选中项 */
  const active = visible.length === 0 ? -1 : Math.min(activeIndex, visible.length - 1);

  // 自动聚焦：StrictMode 下组件会挂载两次，定时器必须在 cleanup 里清掉，
  // 否则第一次挂载的定时器可能对着已经卸载的输入框 focus
  useEffect(() => {
    const timer = window.setTimeout(() => inputRef.current?.focus(), 0);
    return () => window.clearTimeout(timer);
  }, []);

  // 选中项始终留在可视区：结果一多，键盘移动不能跑出滚动窗口
  useEffect(() => {
    if (active < 0) return;
    itemRefs.current[active]?.scrollIntoView({ block: "nearest" });
  }, [active, results]);

  /** 改写关键字：选中项一起归零，避免停在旧的第 N 条 */
  const changeQuery = (value: string) => {
    setQuery(value);
    setActiveIndex(0);
  };

  /** 打开一条会话：先交给上层，再清空输入（口令弹窗接管时首界面还挂着） */
  const open = (session: Session) => {
    onOpen(session);
    changeQuery("");
  };

  /**
   * 键盘处理（捕获阶段）。
   *
   * 参数：event 为输入框/列表上的按键事件
   * 核心流程：↑↓ 取模循环、Enter 打开选中项、Esc 清空关键字并保持聚焦；
   * 三者都 stopPropagation，别让 App 的全局快捷键再收到一次。
   */
  const handleKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    // 输入法组字中的按键（尤其是确认候选词的 Enter）让给输入法，别顺手开会话
    if (event.nativeEvent.isComposing) return;

    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.stopPropagation();
      event.preventDefault();
      if (visible.length === 0) return;
      const delta = event.key === "ArrowDown" ? 1 : -1;
      setActiveIndex((prev) => {
        const current = Math.min(Math.max(prev, 0), visible.length - 1);
        return (current + delta + visible.length) % visible.length;
      });
      return;
    }

    if (event.key === "Enter") {
      event.stopPropagation();
      const target = active >= 0 ? visible[active] : null;
      if (target) open(target);
      return;
    }

    if (event.key === "Escape") {
      event.stopPropagation();
      changeQuery("");
      inputRef.current?.focus();
    }
  };

  /** 行右侧的次级信息：有主机就显示 host:port，否则退回分组路径 */
  const secondary = (session: Session) => {
    if (session.host) {
      return session.port === null ? session.host : `${session.host}:${session.port}`;
    }
    return groupPath(groups, session.groupId).join(" / ") || "本地";
  };

  return (
    <div className="welcome-search" onKeyDownCapture={handleKeyDown}>
      <span className="welcome-search-icon">
        <IconSearch size={14} />
      </span>
      <input
        ref={inputRef}
        value={query}
        placeholder="搜索会话：名称 / 主机 / 用户名 / 分组"
        aria-label="搜索会话"
        onChange={(event) => changeQuery(event.target.value)}
      />
      {query.trim() !== "" && (
        <ul className="welcome-search-results">
          {visible.map((session, index) => (
            <li
              key={session.id}
              ref={(element) => {
                itemRefs.current[index] = element;
              }}
              className={`welcome-search-item ${index === active ? "is-active" : ""}`}
              title={groupPath(groups, session.groupId).join(" / ") || session.name}
              // 按下时别让输入框失焦：点完还能接着输入或按 Esc
              onMouseDown={(event) => event.preventDefault()}
              onMouseEnter={() => setActiveIndex(index)}
              onClick={() => open(session)}
            >
              <span className={`session-icon protocol-${session.protocol}`}>
                {session.protocol === "sftp" ? (
                  <IconFolder size={15} />
                ) : session.protocol === "ssh" ? (
                  <IconServer size={15} />
                ) : (
                  <IconTerminal size={15} />
                )}
              </span>
              <span className="session-name">{session.name}</span>
              <span className="session-target">{secondary(session)}</span>
            </li>
          ))}
          {visible.length === 0 && <li className="welcome-search-empty">没有匹配的会话</li>}
          {results.length > MAX_RESULTS && (
            <li className="welcome-search-more">
              仅显示前 {MAX_RESULTS} 条，共 {results.length} 条
            </li>
          )}
        </ul>
      )}
    </div>
  );
}
