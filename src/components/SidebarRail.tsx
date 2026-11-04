import { useRef } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";
import type { SidebarTab } from "../api/settings";
import { IconFolder, IconMacros, IconSessions, IconTools } from "./icons";

/** 图标栏的一个分页：key 与设置键 sidebar.defaultTab 的取值一致 */
type RailItem = { key: SidebarTab; label: string; icon: ReactNode };

/**
 * 图标栏分页清单。
 *
 * 无参数
 * 核心流程：顺序即自上而下的排列，与 MobaXterm 的左栏语义对齐；
 * 图标用固定 viewBox 的 SVG，不用 emoji，避免不同主题下渲染不一致。
 */
export const SIDEBAR_RAIL: RailItem[] = [
  { key: "sessions", label: "会话", icon: <IconSessions size={18} /> },
  { key: "tools", label: "工具", icon: <IconTools size={18} /> },
  { key: "macros", label: "宏", icon: <IconMacros size={18} /> },
  { key: "sftp", label: "文件", icon: <IconFolder size={18} /> },
];

type Props = {
  /** 当前选中的分页 */
  active: SidebarTab;
  /** 点击图标或方向键切换分页 */
  onSelect: (tab: SidebarTab) => void;
};

/**
 * 左侧 32px 竖排图标栏。
 *
 * 参数：active 为当前分页，onSelect 为切换回调
 * 核心流程：用 tablist/tab 语义建一个纵向单选组，只有选中项能 Tab 进入
 * （漫游 tabindex），上下方向键在四项之间循环切换并同步移动焦点；
 * 选中态同时给背景色与左侧 3px 强调条，并带 aria-selected 与 title，
 * 不靠颜色单独区分。
 */
export function SidebarRail({ active, onSelect }: Props) {
  const buttons = useRef<(HTMLButtonElement | null)[]>([]);

  /**
   * 处理图标栏上的按键。
   *
   * 参数：event 为键盘事件，index 为当前按钮在清单里的下标
   * 核心流程：上下方向键（含 Home/End）算出目标下标后切换分页，
   * 并把焦点移到新按钮上，保证键盘与鼠标操作落到同一处。
   */
  const handleKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>, index: number) => {
    const last = SIDEBAR_RAIL.length - 1;
    let next: number;
    if (event.key === "ArrowDown") next = index === last ? 0 : index + 1;
    else if (event.key === "ArrowUp") next = index === 0 ? last : index - 1;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = last;
    else return;
    event.preventDefault();
    const item = SIDEBAR_RAIL[next];
    onSelect(item.key);
    buttons.current[next]?.focus();
  };

  return (
    <div className="sidebar-rail" role="tablist" aria-orientation="vertical" aria-label="侧栏分页">
      {SIDEBAR_RAIL.map((item, index) => {
        const selected = item.key === active;
        return (
          <button
            key={item.key}
            ref={(element) => {
              buttons.current[index] = element;
            }}
            type="button"
            role="tab"
            id={`sidebar-tab-${item.key}`}
            aria-selected={selected}
            aria-controls={`sidebar-page-${item.key}`}
            tabIndex={selected ? 0 : -1}
            className={`rail-button ${selected ? "is-active" : ""}`}
            title={`${item.label}（快捷键：聚焦后按上下方向键切换）`}
            onClick={() => onSelect(item.key)}
            onKeyDown={(event) => handleKeyDown(event, index)}
          >
            {item.icon}
          </button>
        );
      })}
    </div>
  );
}
